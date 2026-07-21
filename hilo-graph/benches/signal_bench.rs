//! Signal engine benchmarks — understand, tokenization, symbol extraction.
//!
//! Run with: `cargo bench -p hilo_graph --bench signal_bench`

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hilo_graph::graph::GraphDB;
use hilo_graph::signal::{understand, understand_with_source, SignalOpts};
use hilo_metadata::inventory::Edge;

/// Build a chain of N edges (file_0 → file_1 → … → file_N).
fn edge_chain(n: usize) -> Vec<Edge> {
    (0..n)
        .map(|i| Edge {
            from: format!("file_{i}.rs"),
            to: format!("file_{}.rs", i + 1),
            rel: "imports".into(),
            provenance: "ast_exact".to_string(),
            confidence: 1.0,
        })
        .collect()
}

// ── understand: end-to-end signal engine ────────────────────────

fn bench_understand_100_files(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = edge_chain(100);
    db.insert_edges(&edges).unwrap();
    let opts = SignalOpts::default();

    c.bench_function("signal/understand/100-files", |b| {
        b.iter(|| {
            let result = understand(
                black_box(&db),
                black_box("rate limiter auth"),
                black_box(&opts),
            );
            black_box(result.unwrap());
        });
    });
}

fn bench_understand_500_files(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = edge_chain(500);
    db.insert_edges(&edges).unwrap();
    let opts = SignalOpts::default();

    c.bench_function("signal/understand/500-files", |b| {
        b.iter(|| {
            let result = understand(
                black_box(&db),
                black_box("rate limiter auth"),
                black_box(&opts),
            );
            black_box(result.unwrap());
        });
    });
}

// ── tokenize_task (via understand on an empty graph) ────────────
//
// tokenize_task is a private function, exercised through understand().
// On an empty graph the pipeline short-circuits after tokenization
// because there are no files to match, isolating tokenization cost.

fn bench_tokenize_task_short(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    // Empty graph — no files to match, only tokenization runs.
    let task = "rate limiter auth";
    let opts = SignalOpts::default();

    c.bench_function("signal/tokenize/short-3-words", |b| {
        b.iter(|| {
            let result = understand(black_box(&db), black_box(task), black_box(&opts));
            black_box(result.unwrap());
        });
    });
}

fn bench_tokenize_task_medium(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    let task = "authentication middleware rate limiter database query cache";
    let opts = SignalOpts::default();

    c.bench_function("signal/tokenize/medium-10-words", |b| {
        b.iter(|| {
            let result = understand(black_box(&db), black_box(task), black_box(&opts));
            black_box(result.unwrap());
        });
    });
}

fn bench_tokenize_task_long(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    let task = "authentication middleware session management oauth provider jwt token validation \
                rate limiter sliding window counter distributed lock redis cache database query \
                connection pool orm migration schema index full text search async task queue \
                worker pool message broker event bus websocket real time notification push";
    let opts = SignalOpts::default();

    c.bench_function("signal/tokenize/long-50-words", |b| {
        b.iter(|| {
            let result = understand(black_box(&db), black_box(task), black_box(&opts));
            black_box(result.unwrap());
        });
    });
}

// ── extract_symbols (via understand_with_source) ────────────────
//
// extract_symbols is a private function, exercised through the
// understand_with_source() pipeline with a source_reader closure.

/// A realistic Rust source fixture with various definition types.
const RUST_FIXTURE: &str = r#"
use std::collections::HashMap;
use std::sync::Arc;

/// Authenticates users via JWT tokens.
pub struct Authenticator {
    secret: String,
    ttl: u64,
}

impl Authenticator {
    /// Create a new authenticator with the given secret.
    pub fn new(secret: &str) -> Self {
        Authenticator {
            secret: secret.to_string(),
            ttl: 3600,
        }
    }

    /// Validate a JWT token and return the claims.
    pub fn validate_token(&self, token: &str) -> Result<HashMap<String, String>, String> {
        if token.is_empty() {
            return Err("empty token".into());
        }
        let mut claims = HashMap::new();
        claims.insert("sub".to_string(), "user_123".to_string());
        claims.insert("role".to_string(), "admin".to_string());
        Ok(claims)
    }
}

/// Rate limiter using a sliding window counter.
pub struct RateLimiter {
    max_requests: u64,
    window_secs: u64,
    counters: HashMap<String, Vec<u64>>,
}

impl RateLimiter {
    pub fn new(max: u64, window: u64) -> Self {
        RateLimiter {
            max_requests: max,
            window_secs: window,
            counters: HashMap::new(),
        }
    }

    pub fn check_rate(&mut self, key: &str) -> bool {
        let now = 1000u64;
        let window = self.window_secs;
        let entries = self.counters.entry(key.to_string()).or_default();
        entries.retain(|&t| now - t < window);
        entries.push(now);
        entries.len() <= self.max_requests as usize
    }
}

/// A trait for middleware components.
pub trait Middleware {
    fn handle(&self, request: &str) -> Result<String, String>;
}

pub struct LoggingMiddleware;

impl Middleware for LoggingMiddleware {
    fn handle(&self, request: &str) -> Result<String, String> {
        println!("request: {}", request);
        Ok(request.to_string())
    }
}

macro_rules! define_error {
    ($name:ident, $msg:expr) => {
        #[derive(Debug)]
        pub struct $name(pub String);
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}: {}", $msg, self.0)
            }
        }
    };
}

define_error!(AuthError, "authentication failed");
define_error!(RateLimitError, "rate limit exceeded");

pub enum Status {
    Active,
    Inactive,
    Banned,
}

pub const VERSION: &str = "1.0.0";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authenticator() {
        let auth = Authenticator::new("test_secret");
        assert!(auth.validate_token("valid_token").is_ok());
    }
}
"#;

fn bench_extract_symbols_rust(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    // Insert one edge so the graph has a single Rust file.
    let edges = [Edge {
        from: "src/auth/authenticator.rs".into(),
        to: "src/lib.rs".into(),
        rel: "imports".into(),
        provenance: "ast_exact".to_string(),
        confidence: 1.0,
    }];
    db.insert_edges(&edges).unwrap();
    let opts = SignalOpts {
        seed_limit: 8,
        ..Default::default()
    };

    let reader = |path: &str| -> Option<String> {
        if path.contains("authenticator") {
            Some(RUST_FIXTURE.to_string())
        } else {
            None
        }
    };

    c.bench_function("signal/extract-symbols/rust-fixture", |b| {
        b.iter(|| {
            let result = understand_with_source(
                black_box(&db),
                black_box("authenticator"),
                black_box(&opts),
                Some(reader),
            );
            black_box(result.unwrap());
        });
    });
}

criterion_group!(
    benches,
    bench_understand_100_files,
    bench_understand_500_files,
    bench_tokenize_task_short,
    bench_tokenize_task_medium,
    bench_tokenize_task_long,
    bench_extract_symbols_rust,
);
criterion_main!(benches);
