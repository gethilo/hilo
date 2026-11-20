//! Structured logging initialization via tracing.
//!
//! Provides a single `init_logging(json)` function that configures
//! the tracing subscriber. When `json = true`, output is JSON-formatted
//! (suitable for daemon/agent consumers). When `json = false`, output
//! is human-readable (suitable for CLI usage).
//!
//! DF-WARPFS-176: every writer this module installs is wrapped in
//! [`FlushPerEvent`], which flushes the underlying writer after each
//! `write`. `tracing_subscriber`'s fmt layer never calls `flush()` on the
//! writer it is handed, so when the sink is buffered (a `BufWriter`/
//! `LineWriter` over a file, or any future buffered sink) events accumulate
//! in user space and a process killed hard (SIGKILL/OOM/host death) loses
//! its entire session. Flushing at the writer keeps "an event was emitted"
//! and "the bytes are on the fd" the same statement, for every sink.

use std::io::{self, Write};
use std::marker::PhantomData;

use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Initialize the tracing subscriber.
///
/// `json` selects the output format:
/// - `true`: JSON output (for daemon mode, MCP, FUSE, triggers)
/// - `false`: human-readable output (for CLI usage)
///
/// If the `RUST_LOG` environment variable is set, it takes precedence
/// over the default filter (`info`).
pub fn init_logging(json: bool) {
    init_logging_to(json, std::io::stdout);
}

/// Initialize the tracing subscriber writing to a custom sink.
///
/// Stdio transports (MCP over stdin/stdout) MUST pass `std::io::stderr`:
/// stdout is reserved for protocol bytes (newline-delimited JSON-RPC), and
/// any tracing output there corrupts the framing for naive clients.
///
/// `writer` is a writer factory (`Fn() -> W`), matching what
/// `tracing_subscriber::fmt::SubscriberBuilder::with_writer` accepts.
pub fn init_logging_to<F, W>(json: bool, writer: F)
where
    F: Fn() -> W + Send + Sync + 'static,
    W: std::io::Write + 'static,
{
    if json {
        json_subscriber(writer).init();
    } else {
        human_subscriber(writer).init();
    }
}

/// The env-filtered, span-closing complex used by both formats.
fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Human-readable subscriber over a per-event-flushing writer.
///
/// Kept as a named constructor (rather than inlined into
/// [`init_logging_to`]) so tests can drive the production builder shape
/// without touching the process-global default subscriber.
fn human_subscriber<F, W>(writer: F) -> impl tracing::Subscriber + Send + Sync + 'static
where
    F: Fn() -> W + Send + Sync + 'static,
    W: std::io::Write + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(FlushPerEvent::new(writer))
        .finish()
}

/// JSON subscriber over a per-event-flushing writer.
fn json_subscriber<F, W>(writer: F) -> impl tracing::Subscriber + Send + Sync + 'static
where
    F: Fn() -> W + Send + Sync + 'static,
    W: std::io::Write + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(FlushPerEvent::new(writer))
        .json()
        .finish()
}

/// A `Write` adapter that flushes the wrapped writer after every write.
///
/// The flush happens per `write` call, not per line: it must not depend on
/// the event text containing a newline (a JSON event, or a partial write the
/// fmt layer leaves without a trailing newline, would otherwise sit in the
/// buffer). Errors from the flush propagate to `tracing`, exactly like a
/// write error does.
pub struct FlushOnWrite<W>(W);

impl<W: Write> Write for FlushOnWrite<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.0.write(buf)?;
        self.0.flush()?;
        Ok(written)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.0.write_all(buf)?;
        self.0.flush()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// `MakeWriter` adapter handing out [`FlushOnWrite`] writers.
///
/// `PhantomData<fn() -> W>` keeps the factory type `Send + Sync` without
/// requiring the writer type itself to be (a `Write` sink is created and
/// dropped per event; it is never shared).
pub struct FlushPerEvent<F, W>(F, PhantomData<fn() -> W>);

impl<F, W> FlushPerEvent<F, W> {
    /// Wrap a writer factory (`Fn() -> W`, the shape tracing's fmt builder
    /// accepts) so every writer it produces flushes per event.
    pub fn new(writer: F) -> Self {
        Self(writer, PhantomData)
    }
}

impl<'a, F, W> MakeWriter<'a> for FlushPerEvent<F, W>
where
    F: Fn() -> W,
    W: Write + 'static,
{
    type Writer = FlushOnWrite<W>;

    fn make_writer(&'a self) -> Self::Writer {
        FlushOnWrite((self.0)())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Landed = Arc<Mutex<Vec<u8>>>;

    /// A sink that keeps bytes in its own buffer until `flush()` is called —
    /// the durability contract of a `BufWriter` (block-buffered) or
    /// `LineWriter` over the daemon's log file. Bytes in `pending` are
    /// invisible to every other process, and die with the process.
    struct DeferredWriter {
        landed: Landed,
        pending: Vec<u8>,
    }

    impl Write for DeferredWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.pending.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            let mut landed = self.landed.lock().unwrap();
            landed.extend_from_slice(&self.pending);
            self.pending.clear();
            Ok(())
        }
    }

    fn deferred_sink() -> (Landed, impl Fn() -> DeferredWriter + Send + Sync + 'static) {
        let landed: Landed = Arc::new(Mutex::new(Vec::new()));
        let factory = {
            let landed = Arc::clone(&landed);
            move || DeferredWriter {
                landed: Arc::clone(&landed),
                pending: Vec::new(),
            }
        };
        (landed, factory)
    }

    fn landed_text(landed: &Landed) -> String {
        String::from_utf8_lossy(&landed.lock().unwrap()).into_owned()
    }

    // ───────── DF-WARPFS-176: per-event durability ─────────

    #[test]
    fn every_event_reaches_the_sink_before_the_next_one() {
        // RED-BEFORE: tracing_subscriber's fmt layer never flushes the writer
        // it is handed, so with an unwrapped buffered sink both assertions
        // below see an empty `landed` (the events sit in `pending` and would
        // be lost by SIGKILL). No explicit flush is called anywhere in this
        // test — durability must come from the production writer path.
        let (landed, factory) = deferred_sink();
        let subscriber = human_subscriber(factory);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("df-warpfs-176 first event");
            let after_first = landed_text(&landed);
            assert!(
                after_first.contains("df-warpfs-176 first event"),
                "event 1 must be visible on the durable sink as soon as it is \
                 emitted, with no explicit flush: {after_first:?}"
            );

            tracing::info!("df-warpfs-176 second event");
            let after_second = landed_text(&landed);
            assert!(
                after_second.contains("df-warpfs-176 first event"),
                "event 1 must still be there after event 2: {after_second:?}"
            );
            assert!(
                after_second.contains("df-warpfs-176 second event"),
                "event 2 must be visible on the durable sink as soon as it is \
                 emitted, with no explicit flush: {after_second:?}"
            );
        });
    }

    #[test]
    fn the_writer_flushes_each_write_without_a_newline() {
        // The flush cannot be line-triggered: a partial write (or a JSON
        // event formatted without a trailing newline) must still reach the
        // sink. DF-WARPFS-176.
        let (landed, factory) = deferred_sink();
        let mut writer = FlushPerEvent::new(factory).make_writer();

        writer.write_all(b"partial-line").unwrap();
        assert_eq!(
            landed_text(&landed),
            "partial-line",
            "a write with no newline must be durable immediately"
        );

        writer.write_all(b" continues\n").unwrap();
        assert_eq!(landed_text(&landed), "partial-line continues\n");
    }

    #[test]
    fn json_subscriber_also_flushes_per_event() {
        // Both formats go through FlushPerEvent; a JSON daemon (serve --mcp,
        // FUSE) must not be the unbuffered-format special case.
        let (landed, factory) = deferred_sink();
        let subscriber = json_subscriber(factory);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "df-warpfs-176", "json-event");
            let text = landed_text(&landed);
            assert!(
                text.contains("json-event"),
                "JSON event must be durable immediately: {text:?}"
            );
        });
    }
}
