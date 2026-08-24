//! Structured logging initialization via tracing.
//!
//! Provides a single `init_logging(json)` function that configures
//! the tracing subscriber. When `json = true`, output is JSON-formatted
//! (suitable for daemon/agent consumers). When `json = false`, output
//! is human-readable (suitable for CLI usage).

use tracing_subscriber::fmt::format::FmtSpan;
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
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(writer);

    if json {
        builder.json().init();
    } else {
        builder.init();
    }
}
