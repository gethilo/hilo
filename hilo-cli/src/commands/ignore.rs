//! `hilo ignore check` — inspect ignore decisions for a path.
//!
//! Loads the workspace ignore file (`.hiloignore`, with `.vfsignore` accepted
//! as a legacy alias) and reports whether the given path would be excluded,
//! along with the exact rule line that decided it. This is the diagnostic
//! companion to `hilo workspace sync`'s ignore-aware transfer (spec:
//! backend-backed-workspace-spec.md §9).

use std::path::PathBuf;

use clap::{Args, Subcommand};
use hilo_backends::IgnoreMatcher;

#[derive(Subcommand)]
pub enum IgnoreCommand {
    /// Report whether a path is ignored by the workspace ignore file, and
    /// which rule decided it.
    Check(IgnoreArgs),
}

#[derive(Args)]
pub struct IgnoreArgs {
    /// Path to check, relative to the current directory.
    pub path: String,

    /// Ignore file to load instead of <cwd>/.hiloignore.
    #[arg(long)]
    pub ignore_file: Option<PathBuf>,
}

pub fn run_ignore_check(args: IgnoreArgs) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;

    let ignore_file = match args.ignore_file {
        Some(f) => f,
        None => {
            let primary = cwd.join(".hiloignore");
            let legacy = cwd.join(".vfsignore");
            if primary.exists() {
                primary
            } else if legacy.exists() {
                legacy
            } else {
                primary
            }
        }
    };

    let matcher = IgnoreMatcher::from_file(&ignore_file)?;

    // Resolve the target to a POSIX path relative to the workspace root.
    let target = cwd.join(&args.path);
    let abs = if target.exists() {
        target.canonicalize()?
    } else {
        target
    };
    let rel = abs.strip_prefix(&cwd).unwrap_or(&abs);
    let rel_str = rel.to_string_lossy().replace('\\', "/");

    let decision = matcher.decision(&rel_str);
    println!("path: {rel_str}");
    println!("ignored: {}", decision.ignored);
    match decision.rule {
        Some(rule) => println!("rule: {rule}"),
        None => println!("rule: (none)"),
    }
    println!("source: {}", ignore_file.display());
    Ok(())
}
