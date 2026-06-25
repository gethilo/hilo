//! `hilo meta <path>` — list or set Hilo extended attributes on a file.

use std::path::Path;

use anyhow::{Context, Result};
use hilo_metadata::xattr;

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
        let v = value.unwrap_or("");
        // Support literal `\n` in the value for multiline content.
        let v = v.replace("\\n", "\n");
        xattr::set_vfs_xattr(file_path, name, &v)
            .with_context(|| format!("failed to set xattr user.vfs.{name} on {path}"))?;
        // Display the canonical name (strip user.vfs. prefix if user passed it).
        let display_name = name.strip_prefix("user.vfs.").unwrap_or(name);
        println!("Set user.vfs.{display_name} = {v} on {path}");
        return Ok(());
    }

    // List mode (default — no --set flag).
    let attrs = xattr::list_vfs_xattrs(file_path)
        .with_context(|| format!("failed to list xattrs for {path}"))?;

    if attrs.is_empty() {
        println!("No Hilo metadata for {path}");
        return Ok(());
    }

    let prefix = "user.vfs.";
    for full_name in &attrs {
        // list_vfs_xattrs returns full names like "user.vfs.imports".
        // Strip the prefix for display, then fetch via the stripped key
        // (get_vfs_xattr re-adds the prefix internally).
        let stripped = full_name.strip_prefix(prefix).unwrap_or(full_name);
        let value = xattr::get_vfs_xattr(file_path, stripped)
            .with_context(|| format!("failed to read xattr {full_name}"))?;
        match value {
            Some(v) => println!("{stripped}: {v}"),
            None => println!("{stripped}:"),
        }
    }

    Ok(())
}
