//! `hilo classify` — Auto-classify source files with role/status xattrs.
//!
//! Uses tree-sitter AST queries to detect entrypoints, tests, libraries,
//! and other file roles. Writes results as `user.vfs.role` and `user.vfs.status`
//! extended attributes. No LLM required.
//!
//! With `--features`, also infers `user.vfs.feature` from directory structure
//! and an optional `.vfs/features/tags.yaml` override file.

use anyhow::Context;
use anyhow::Result;
use hilo_core::manifest::FeatureInference;
use hilo_graph::{classify_file, infer_feature, Language};
use hilo_metadata::xattr;
use std::collections::HashMap;
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
    ("cs", Language::CSharp),
    ("kt", Language::Kotlin),
    ("kts", Language::Kotlin),
    ("php", Language::Php),
    ("phtml", Language::Php),
    ("swift", Language::Swift),
    ("ex", Language::Elixir),
    ("exs", Language::Elixir),
    ("hs", Language::Haskell),
    ("lhs", Language::Haskell),
    ("erl", Language::Erlang),
    ("hrl", Language::Erlang),
    ("scala", Language::Scala),
    ("sc", Language::Scala),
    ("zig", Language::Zig),
    ("lua", Language::Lua),
    ("dart", Language::Dart),
];

/// Run the classify command.
pub fn run_classify(dry_run: bool, verbose: bool, features: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to get current directory")?;

    // Load feature inference config if requested
    let (feature_config, overrides) = if features {
        let config = FeatureInference::default();
        let ov = load_override_file(&cwd, config.override_file.as_deref())?;
        (Some(config), ov)
    } else {
        (None, None)
    };

    let mut file_count = 0;
    let mut classified = 0;
    let mut errors = 0;
    let mut feature_count = 0;

    let mut by_role: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut by_feature: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

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
                let role = classification.role.clone();
                let status = classification.status.clone();
                let reason = classification.reason.clone();

                // 4. Feature inference (after classification, before xattr write)
                let feature = if let Some(ref fc) = feature_config {
                    if fc.enabled {
                        infer_feature(&rel_path, &fc.strategy, overrides.as_ref())
                    } else {
                        None
                    }
                } else {
                    None
                };

                if feature.is_some() {
                    feature_count += 1;
                }

                if dry_run {
                    if let Some(ref feat) = feature {
                        println!(
                            "{:30} → role={:<12} status={:<10} feature={} ({})",
                            rel_path, role, status, feat, reason
                        );
                    } else {
                        println!(
                            "{:30} → role={:<12} status={:<10} ({})",
                            rel_path, role, status, reason
                        );
                    }
                } else {
                    // Write role and status as xattrs
                    if let Err(e) =
                        xattr::set_vfs_xattr(std::path::Path::new(&rel_path), "role", &role)
                    {
                        eprintln!("  xattr error on {}: {e}", rel_path);
                        errors += 1;
                        return;
                    }
                    if let Err(e) =
                        xattr::set_vfs_xattr(std::path::Path::new(&rel_path), "status", &status)
                    {
                        eprintln!("  xattr error on {}: {e}", rel_path);
                        errors += 1;
                        return;
                    }
                    // Write feature xattr if present
                    if let Some(ref feat) = feature {
                        if let Err(e) =
                            xattr::set_vfs_xattr(std::path::Path::new(&rel_path), "feature", feat)
                        {
                            eprintln!("  xattr error on {}: {e}", rel_path);
                            errors += 1;
                            return;
                        }
                    }
                    if verbose {
                        if let Some(ref feat) = feature {
                            println!(
                                "  {:30} → role={:<12} status={:<10} feature={}",
                                rel_path, role, status, feat
                            );
                        } else {
                            println!(
                                "  {:30} → role={:<12} status={:<10}",
                                rel_path, role, status
                            );
                        }
                    }
                }

                *by_role.entry(role.clone()).or_insert(0) += 1;
                if let Some(ref feat) = feature {
                    *by_feature.entry(feat.clone()).or_insert(0) += 1;
                }
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
    if feature_count > 0 {
        println!("  Features set:   {feature_count}");
    }
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

    if !by_feature.is_empty() {
        println!();
        println!("  By feature:");
        let mut feats: Vec<_> = by_feature.iter().collect();
        feats.sort_by(|a, b| b.1.cmp(a.1));
        for (feat, count) in feats {
            println!("    {feat:<14} {count}");
        }
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

/// Load feature override file from `.vfs/features/tags.yaml`.
///
/// Format: YAML mapping of file path → feature name.
/// Directory-prefix entries end with `/`.
fn load_override_file(
    cwd: &Path,
    override_file: Option<&str>,
) -> Result<Option<HashMap<String, String>>> {
    let path = if let Some(of) = override_file {
        cwd.join(of)
    } else {
        // Default: .vfs/features/tags.yaml
        cwd.join(".vfs").join("features").join("tags.yaml")
    };

    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read override file: {}", path.display()))?;

    let overrides: HashMap<String, String> = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse override file: {}", path.display()))?;

    Ok(Some(overrides))
}
