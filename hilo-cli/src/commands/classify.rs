//! `hilo classify` — Auto-classify source files with role/status xattrs.
//!
//! Uses tree-sitter AST queries to detect entrypoints, tests, libraries,
//! and other file roles. Writes results as `user.vfs.role` and `user.vfs.status`
//! extended attributes. No LLM required.

use anyhow::{Context, Result};
use hilo_graph::{classify_file, Classification, Language};
use hilo_metadata::xattr;
use std::fs;
use std::path::Path;

/// Supported source file extensions mapped to Hilo languages.
const SOURCE_EXTS: &[(&str, Language)] = &[
    ("rs", Language::Rust),
    ("py", Language::Python),
    ("go", Language::Go),
    ("js", Language::JavaScript),
    ("jsx", Language::JavaScript),
    ("ts", Language::TypeScript),
    ("tsx", Language::TypeScript),
    ("java", Language::Java),
    ("c", Language::C),
    ("cpp", Language::Cpp),
    ("cc", Language::Cpp),
    ("cxx", Language::Cpp),
    ("h", Language::C),
    ("hpp", Language::Cpp),
    ("hxx", Language::Cpp),
    ("rb", Language::Ruby),
];

/// Run the classify command.
pub fn run_classify(dry_run: bool, verbose: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to get current directory")?;

    let mut file_count = 0;
    let mut classified = 0;
    let mut errors = 0;

    let mut by_role: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    walk_files(&cwd, &mut |path| {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let Some(&(_, language)) = SOURCE_EXTS.iter().find(|(e, _)| e == &ext.as_str()) else {
            return;
        };

        file_count += 1;

        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                if verbose {
                    eprintln!("  skip {}: {e}", path.display());
                }
                errors += 1;
                return;
            }
        };

        let rel_path = path
            .strip_prefix(&cwd)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        match classify_file(language, &rel_path, &source) {
            Ok(classification) => {
                let Classification {
                    role,
                    status,
                    reason,
                } = &classification;

                if dry_run {
                    println!(
                        "{:30} → role={:<12} status={:<10} ({})",
                        rel_path, role, status, reason
                    );
                } else {
                    // Write as xattrs
                    if let Err(e) =
                        xattr::set_vfs_xattr(std::path::Path::new(&rel_path), "role", role)
                    {
                        eprintln!("  xattr error on {}: {e}", rel_path);
                        errors += 1;
                        return;
                    }
                    if let Err(e) =
                        xattr::set_vfs_xattr(std::path::Path::new(&rel_path), "status", status)
                    {
                        eprintln!("  xattr error on {}: {e}", rel_path);
                        errors += 1;
                        return;
                    }
                    if verbose {
                        println!(
                            "  {:30} → role={:<12} status={:<10}",
                            rel_path, role, status
                        );
                    }
                }

                *by_role.entry(role.clone()).or_insert(0) += 1;
                classified += 1;
            }
            Err(e) => {
                if verbose {
                    eprintln!("  classify error on {}: {e}", rel_path);
                }
                errors += 1;
            }
        }
    });

    println!();
    println!("  Files scanned:  {file_count}");
    println!("  Classified:     {classified}");
    if errors > 0 {
        println!("  Errors:         {errors}");
    }
    println!();
    println!("  By role:");
    let mut roles: Vec<_> = by_role.iter().collect();
    roles.sort_by(|a, b| b.1.cmp(a.1));
    for (role, count) in roles {
        println!("    {role:<14} {count}");
    }

    if dry_run {
        println!();
        println!("  (Dry run — no xattrs written. Remove --dry-run to apply.)");
    }

    Ok(())
}

/// Walk all files recursively, skipping .git, target, node_modules.
fn walk_files(root: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // Skip common non-source directories
        if name == ".git"
            || name == "target"
            || name == "node_modules"
            || name == ".vfs"
            || name == "vendor"
            || name == "__pycache__"
        {
            continue;
        }

        if path.is_dir() {
            walk_files(&path, f);
        } else if path.is_file() {
            f(&path);
        }
    }
}
