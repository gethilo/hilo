use hilo_cli::cli;

/// Restore the default `SIGPIPE` disposition at process start.
///
/// The Rust runtime installs `SIG_IGN` for `SIGPIPE` before `main` runs, so a
/// write into a closed pipe (`hilo graph stats | head`, `| true`) surfaces as
/// `EPIPE` / "Broken pipe (os error 32)" and the `std` print macros turn that
/// write error into a panic with exit code 101. Every other Unix filter is
/// killed silently by the kernel instead (exit status 141), so restore the
/// default handler and let SIGPIPE do its job. This is the ripgrep/fd
/// behaviour; it belongs to the CLI binary only, never to a library crate.
#[cfg(unix)]
fn reset_sigpipe() {
    // SAFETY: installing a default disposition for a valid signal number
    // cannot violate memory safety; the return value is only `SIG_ERR` for an
    // invalid signal, and `SIGPIPE` is always valid on unix.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// No-op on platforms without POSIX signals (Windows has no `SIGPIPE`).
#[cfg(not(unix))]
fn reset_sigpipe() {}

fn main() {
    reset_sigpipe();

    if let Err(e) = cli::run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
