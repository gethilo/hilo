//! `hilo ignore check` — inspect ignore decisions for a path.
//!
//! Loads the workspace ignore stack — built-in defaults (spec §4.2), the
//! root `.hiloignore` (with `.vfsignore` accepted as a legacy alias), and
//! nested `.hiloignore` files — and reports whether the given path would be
//! excluded, the exact rule line that decided it, and where the rule came
//! from. This is the diagnostic companion to `hilo workspace sync`'s
//! ignore-aware transfer (spec: backend-backed-workspace-spec.md §9).

use std::path::PathBuf;

use clap::{Args, Subcommand};
use hilo_backends::{IgnoreMatcher, IgnoreSource};

#[derive(Subcommand)]
pub enum IgnoreCommand {
    /// Report whether a path is ignored by the workspace ignore stack, and
    /// which rule decided it.
    Check(IgnoreArgs),
}

#[derive(Args)]
pub struct IgnoreArgs {
    /// Path to check, relative to the current directory.
    pub path: String,

    /// Extra ignore file to load on top of the built-in defaults and the
    /// root `.hiloignore`.
    #[arg(long)]
    pub ignore_file: Option<PathBuf>,

    /// Skip the built-in default ignore patterns.
    #[arg(long)]
    pub no_default_ignores: bool,
}

pub fn run_ignore_check(args: IgnoreArgs) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;

    // Root ignore file: `.hiloignore`, with `.vfsignore` accepted as a
    // legacy alias when no root `.hiloignore` exists.
    let legacy = cwd.join(".vfsignore");
    let extra = match args.ignore_file {
        Some(f) => Some(f),
        None if !cwd.join(".hiloignore").exists() && legacy.exists() => Some(legacy),
        None => None,
    };

    let matcher = IgnoreMatcher::load(&cwd, extra.as_deref(), args.no_default_ignores)?;

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
    match decision.source {
        Some(IgnoreSource::Builtin) => println!("source: builtin defaults"),
        Some(IgnoreSource::RootFile) => println!("source: root .hiloignore"),
        Some(IgnoreSource::NestedFile(dir)) => {
            println!("source: nested {}", dir.display())
        }
        None => println!("source: (none)"),
    }
    Ok(())
}
