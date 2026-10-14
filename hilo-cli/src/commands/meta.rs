//! `hilo meta <path>` — list or set Hilo extended attributes on a file.
//!
//! DF-WARPFS-95: xattrs do not survive `git push`/`git clone`, so a
//! successful `--set` is also mirrored into the git-tracked sidecar
//! `.vfs/annotations.jsonl` (best-effort: only inside a Hilo workspace, and
//! a sidecar failure never fails the xattr write). List mode merges in
//! sidecar records that have no local xattr, and `hilo meta apply`
//! re-applies sidecar records as xattrs after a fresh clone — idempotently,
//! never clobbering an existing local xattr.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hilo_metadata::sidecar;
use hilo_metadata::xattr;

const PREFIX: &str = "user.vfs.";

/// Print every `user.vfs.*` extended attribute on `path` (prefix stripped).
///
/// When `set_name` is provided, sets `user.vfs.<set_name>` to `value`
/// (defaulting to empty string when `value` is `None`).
///
/// If the file has no Hilo metadata a short message is printed instead
/// (list mode) or the attribute is created (set mode).
pub fn run(path: &str, set_name: Option<&str>, value: Option<&str>) -> Result<()> {
    let file_path = Path::new(path);

    if !file_path.exists() {
        anyhow::bail!("no such file: {path}");
    }

    if let Some(name) = set_name {
        // --set mode: write the extended attribute.
        // Pre-validate the name so a `--set role=value` typo fails with the
        // correct usage instead of silently creating a garbage xattr named
        // `user.vfs.role=value` with an empty value.
        let display_name = name.strip_prefix(PREFIX).unwrap_or(name);
        if name.is_empty()
            || display_name.is_empty()
            || display_name.contains('=')
            || display_name
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            anyhow::bail!(
                "invalid attribute name \"{name}\" — usage: hilo meta <path> --set <attr> --value <value>"
            );
        }
        let v = value.unwrap_or("");
        // Support literal `\n` in the value for multiline content.
        let v = v.replace("\\n", "\n");
        xattr::set_vfs_xattr(file_path, name, &v)
            .with_context(|| format!("failed to set xattr user.vfs.{display_name} on {path}"))?;
        println!("Set user.vfs.{display_name} = {v} on {path}");
        record_sidecar(file_path, name, &v);
        return Ok(());
    }

    // List mode (default — no --set flag).
    let attrs = xattr::list_vfs_xattrs(file_path)
        .with_context(|| format!("failed to list xattrs for {path}"))?;

    let workspace = workspace_rel_path(file_path);
    let mut printed = false;

    for full_name in &attrs {
        // list_vfs_xattrs returns full names like "user.vfs.imports".
        // Strip the prefix for display, then fetch via the stripped key
        // (get_vfs_xattr re-adds the prefix internally).
        let stripped = full_name.strip_prefix(PREFIX).unwrap_or(full_name);
        let value = xattr::get_vfs_xattr(file_path, stripped)
            .with_context(|| format!("failed to read xattr {full_name}"))?;
        match value {
            Some(v) => println!("{stripped}: {v}"),
            None => println!("{stripped}:"),
        }
        printed = true;
    }

    // DF-WARPFS-95: merge sidecar records for this file whose attr is NOT
    // present as an xattr — that gap is exactly what `hilo meta apply`
    // would fill (e.g. right after a fresh clone).
    if let Some((root, rel_path)) = workspace {
        let xattr_keys: HashSet<&str> = attrs
            .iter()
            .map(|a| a.strip_prefix(PREFIX).unwrap_or(a))
            .collect();
        let records = sidecar::read_annotations(&root)?;
        for rec in records {
            if rec.rel_path == rel_path && !xattr_keys.contains(rec.attr.as_str()) {
                println!("{}: {} (sidecar)", rec.attr, rec.value);
                printed = true;
            }
        }
    }

    if !printed {
        println!("No Hilo metadata for {path}");
    }

    Ok(())
}

/// DF-WARPFS-95: re-apply `.vfs/annotations.jsonl` records as xattrs.
///
/// Idempotent by design: a record is applied ONLY when the target xattr is
/// currently missing — a local xattr (any value) always wins, so re-running
/// apply on a live workspace is a no-op and never clobbers local edits.
/// Missing sidecar / empty sidecar is a normal state, not an error.
pub fn run_apply(dry_run: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let root = sidecar::find_workspace_root(&cwd)
        .context("not inside a Hilo workspace (no .vfs/ found)")?;

    let records = sidecar::read_annotations(&root)?;
    if records.is_empty() {
        println!("no sidecar annotations");
        return Ok(());
    }

    let mut applied = 0usize;
    for rec in &records {
        let abs = root.join(&rec.rel_path);
        if !abs.exists() {
            println!("skipped {} (file missing)", rec.rel_path);
            continue;
        }
        // A read failure is treated as "not set" — the apply attempt below
        // will surface the real error if the write also fails.
        let current = xattr::get_vfs_xattr(&abs, &rec.attr).unwrap_or(None);
        if current.is_some() {
            // Idempotent: the local xattr wins, nothing to do.
            continue;
        }
        if dry_run {
            println!(
                "would apply user.vfs.{} on {} = {}",
                rec.attr, rec.rel_path, rec.value
            );
            applied += 1;
        } else {
            match xattr::set_vfs_xattr(&abs, &rec.attr, &rec.value) {
                Ok(()) => {
                    applied += 1;
                    println!(
                        "applied user.vfs.{} on {} = {}",
                        rec.attr, rec.rel_path, rec.value
                    );
                }
                Err(e) => eprintln!(
                    "warning: failed to apply user.vfs.{} on {}: {e}",
                    rec.attr, rec.rel_path
                ),
            }
        }
    }

    if dry_run {
        println!(
            "would apply {applied} of {} records (dry run)",
            records.len()
        );
    } else {
        println!("applied {applied} of {} records", records.len());
    }
    Ok(())
}

/// Best-effort sidecar mirror of a successful xattr write.
///
/// Only records when `file_path` lives inside a Hilo workspace (found by
/// walking up to the nearest ancestor with `.vfs/`); outside a workspace the
/// xattr-only behavior is preserved silently. A sidecar failure is printed
/// as a warning on stderr and never fails the already-successful xattr
/// write.
fn record_sidecar(file_path: &Path, name: &str, value: &str) {
    let Some((root, rel_path)) = workspace_rel_path(file_path) else {
        return;
    };
    let attr = name.strip_prefix(PREFIX).unwrap_or(name);
    let rec = sidecar::AnnotationRecord {
        rel_path,
        attr: attr.to_string(),
        value: value.to_string(),
    };
    if let Err(e) = sidecar::record_annotation(&root, &rec) {
        eprintln!("warning: sidecar write failed: {e}");
        return;
    }
    println!("Recorded in .vfs/annotations.jsonl");
}

/// Find the enclosing workspace root and the file's path relative to it.
///
/// Returns `None` outside any Hilo workspace. When the lexical
/// `strip_prefix` fails (unusual relative inputs), the raw path string is
/// kept as `rel_path` so the record still round-trips for the common
/// same-path invocation.
fn workspace_rel_path(file_path: &Path) -> Option<(PathBuf, String)> {
    let root = sidecar::find_workspace_root(file_path)?;
    let rel = file_path
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file_path.to_string_lossy().into_owned());
    Some((root, rel))
}
