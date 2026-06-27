//! Hilo permission enforcement — glob-based path matching with mode bits.
//!
//! This crate provides the `PermissionEngine` that evaluates whether an operation
//! (Read, Write, Execute) is allowed on a given path based on a set of glob-based
//! `PermissionRule` entries. Rules are iterated in order — first match wins.
//!
//! # Example
//!
//! ```rust
//! use hilo_permissions::{PermissionEngine, PermissionRule, PermissionOp};
//!
//! let rules = vec![
//!     PermissionRule { paths: vec!["src/**".into()], mode: 0o644, allow_delete: true },
//!     PermissionRule { paths: vec![".vfs/**".into()], mode: 0o444, allow_delete: false },
//! ];
//! let engine = PermissionEngine::from_rules(rules);
//!
//! assert!(engine.check("src/main.rs", PermissionOp::Read).is_ok());
//! assert!(engine.check("src/main.rs", PermissionOp::Write).is_ok());
//! assert!(engine.check(".vfs/manifest.yaml", PermissionOp::Read).is_ok());
//! assert!(engine.check(".vfs/manifest.yaml", PermissionOp::Write).is_err());
//! ```

use std::path::Path;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single permission rule — a set of glob patterns and the mode they enforce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRule {
    /// Glob patterns to match against file paths (e.g., `"src/**"`, `".vfs/**"`).
    pub paths: Vec<String>,
    /// Octal mode enforced when this rule matches (e.g., `0o444`, `0o644`).
    pub mode: u32,
    /// Whether deletion is allowed for files matching this rule.
    pub allow_delete: bool,
}

/// A backend-level permission rule — enforces a mode on an entire mount tree.
///
/// Backend rules apply to any path whose first component matches `name`
/// (e.g., a rule with `name = "shared-lib"` matches `"shared-lib/src/main.rs"`).
/// Backend rules take priority over glob-based `PermissionRule` entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendPermissionRule {
    /// The mount name this rule applies to (first path component).
    pub name: String,
    /// Octal mode enforced for all paths under this backend mount
    /// (e.g., `0o444` for read-only dependency repos).
    pub mode: u32,
}

/// The result of applying a permission rule to a specific path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionResult {
    /// Octal mode for this path.
    pub mode: u32,
    /// True if any read bit is set in the mode.
    pub readable: bool,
    /// True if the owner write bit is set in the mode.
    pub writable: bool,
}

impl PermissionRule {
    /// Apply this rule to a path, returning the computed `PermissionResult`.
    ///
    /// Checks whether any of the rule's glob patterns match the given path.
    /// If a match is found, the rule's mode is returned. Otherwise returns `None`.
    pub fn apply(&self, path: &Path) -> Option<PermissionResult> {
        let path_str = path_to_str(path);
        for pattern in &self.paths {
            if glob_matches(pattern, &path_str) {
                return Some(PermissionResult {
                    mode: self.mode,
                    readable: mode_has_read(self.mode),
                    writable: mode_has_write(self.mode),
                });
            }
        }
        None
    }
}

/// The kind of operation being checked against permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionOp {
    Read,
    Write,
    Execute,
}

/// Errors returned by `PermissionEngine::check()`.
#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    /// The operation is denied by a matching rule.
    #[error("permission denied for '{path}': {op:?} not allowed (mode {mode:#o})")]
    Denied {
        path: String,
        op: PermissionOp,
        mode: u32,
    },
}

// ---------------------------------------------------------------------------
// PermissionEngine
// ---------------------------------------------------------------------------

/// Evaluates permission rules for file-system paths.
///
/// Rules are evaluated in order — backend rules are checked first (by path
/// component prefix-match), then glob-based rules. The first match wins.
/// If no rule matches, the `default_mode` (typically `0o644`) applies.
#[derive(Debug, Clone)]
pub struct PermissionEngine {
    rules: Vec<PermissionRule>,
    backend_rules: Vec<BackendPermissionRule>,
    default_mode: u32,
}

impl PermissionEngine {
    /// Create an engine from a list of `PermissionRule` entries.
    ///
    /// The engine will apply rules in the order provided. If no rule matches
    /// a path, `default_mode` is used (files: `0o644`, dirs: `0o755`).
    pub fn from_rules(rules: Vec<PermissionRule>) -> Self {
        Self {
            rules,
            backend_rules: Vec::new(),
            default_mode: 0o644,
        }
    }

    /// Create a new engine with the given rules and an explicit default mode.
    pub fn new(rules: Vec<PermissionRule>, default_mode: u32) -> Self {
        Self {
            rules,
            backend_rules: Vec::new(),
            default_mode,
        }
    }

    /// Create an engine with glob rules, backend rules, and an explicit default mode.
    pub fn new_with_backends(
        rules: Vec<PermissionRule>,
        backend_rules: Vec<BackendPermissionRule>,
        default_mode: u32,
    ) -> Self {
        Self {
            rules,
            backend_rules,
            default_mode,
        }
    }

    /// Check whether `op` is allowed on `path`.
    ///
    /// Returns `Ok(())` if allowed, or `Err(PermissionError::Denied)` with
    /// details about which rule blocked the operation.
    pub fn check<P: AsRef<Path>>(&self, path: P, op: PermissionOp) -> Result<(), PermissionError> {
        let path = path.as_ref();
        let mode = self.compute_mode(path);

        let allowed = match op {
            PermissionOp::Read => mode_has_read(mode),
            PermissionOp::Write => mode_has_write(mode),
            PermissionOp::Execute => mode_has_exec(mode),
        };

        if allowed {
            Ok(())
        } else {
            Err(PermissionError::Denied {
                path: path_to_str(path).into_owned(),
                op,
                mode,
            })
        }
    }

    /// Compute the permission mode for `path` given the engine's rules.
    ///
    /// Backend rules are checked first (by first-path-component prefix match);
    /// then glob-based rules are iterated in order. The first match wins.
    /// If no rule matches, the `default_mode` is returned.
    pub fn compute_mode(&self, path: &Path) -> u32 {
        let path_str = path_to_str(path);

        // 1. Check backend rules first — they take priority over glob rules.
        let first_component = path_str.split('/').next().unwrap_or(&path_str);
        for backend in &self.backend_rules {
            if backend.name == first_component {
                return backend.mode;
            }
        }

        // 2. Fall through to glob-based rules.
        for rule in &self.rules {
            for pattern in &rule.paths {
                if glob_matches(pattern, &path_str) {
                    return rule.mode;
                }
            }
        }

        // 3. Default.
        self.default_mode
    }
}

// ---------------------------------------------------------------------------
// Default protections (Hilo spec — §4 permissions block)
// ---------------------------------------------------------------------------

/// The default permission protections from the Hilo spec.
///
/// These rules make infrastructure files (`.vfs/`, `.git/`, generated files,
/// lock files, vendored code) read-only at the kernel level, while allowing
/// normal read-write access to source directories.
pub fn default_protections() -> Vec<PermissionRule> {
    vec![
        // Infrastructure — strictly read-only.
        PermissionRule {
            paths: vec![".vfs/**".into()],
            mode: 0o444,
            allow_delete: false,
        },
        PermissionRule {
            paths: vec![".git/**".into()],
            mode: 0o444,
            allow_delete: false,
        },
        PermissionRule {
            paths: vec![".gitignore".into()],
            mode: 0o444,
            allow_delete: false,
        },
        PermissionRule {
            paths: vec!["**/*.sum".into()],
            mode: 0o444,
            allow_delete: false,
        },
        PermissionRule {
            paths: vec!["**/*.lock".into()],
            mode: 0o444,
            allow_delete: false,
        },
        PermissionRule {
            paths: vec!["**/*.pb.go".into()],
            mode: 0o444,
            allow_delete: false,
        },
        PermissionRule {
            paths: vec!["**/*.gen.go".into()],
            mode: 0o444,
            allow_delete: false,
        },
        PermissionRule {
            paths: vec!["**/vendor/**".into()],
            mode: 0o444,
            allow_delete: false,
        },
        PermissionRule {
            paths: vec!["**/node_modules/**".into()],
            mode: 0o444,
            allow_delete: false,
        },
        PermissionRule {
            paths: vec![".github/workflows/**".into()],
            mode: 0o444,
            allow_delete: false,
        },
        // Source directories — normal read-write.
        PermissionRule {
            paths: vec!["src/**".into()],
            mode: 0o644,
            allow_delete: true,
        },
        PermissionRule {
            paths: vec!["lib/**".into()],
            mode: 0o644,
            allow_delete: true,
        },
        PermissionRule {
            paths: vec!["cmd/**".into()],
            mode: 0o644,
            allow_delete: true,
        },
    ]
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Convert a path to a string for glob matching.
///
/// Uses `/` separators and strips leading `./` for consistent matching.
fn path_to_str(path: &Path) -> std::borrow::Cow<'_, str> {
    let s = path.to_string_lossy();
    let trimmed = s.trim_start_matches("./");
    if trimmed.len() < s.len() {
        std::borrow::Cow::Owned(trimmed.to_string())
    } else {
        s
    }
}

/// Check whether a glob pattern matches a path string.
fn glob_matches(pattern: &str, path: &str) -> bool {
    if let Ok(glob) = glob::Pattern::new(pattern) {
        glob.matches(path) || glob.matches(path.trim_start_matches("./"))
    } else {
        false
    }
}

fn mode_has_read(mode: u32) -> bool {
    (mode & 0o444) != 0
}

fn mode_has_write(mode: u32) -> bool {
    (mode & 0o200) != 0
}

fn mode_has_exec(mode: u32) -> bool {
    (mode & 0o111) != 0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to make a path reference.
    fn p(s: &str) -> &Path {
        Path::new(s)
    }

    // --- PermissionRule::apply() ---

    #[test]
    fn apply_matching_glob() {
        let rule = PermissionRule {
            paths: vec![".vfs/**".into()],
            mode: 0o444,
            allow_delete: false,
        };
        let result = rule.apply(p(".vfs/manifest.yaml")).unwrap();
        assert_eq!(result.mode, 0o444);
        assert!(result.readable);
        assert!(!result.writable);
    }

    #[test]
    fn apply_non_matching() {
        let rule = PermissionRule {
            paths: vec![".vfs/**".into()],
            mode: 0o444,
            allow_delete: false,
        };
        assert!(rule.apply(p("src/main.rs")).is_none());
    }

    #[test]
    fn apply_writable_rule() {
        let rule = PermissionRule {
            paths: vec!["src/**".into()],
            mode: 0o644,
            allow_delete: true,
        };
        let result = rule.apply(p("src/main.rs")).unwrap();
        assert_eq!(result.mode, 0o644);
        assert!(result.readable);
        assert!(result.writable);
    }

    // --- PermissionEngine::compute_mode() ---

    #[test]
    fn compute_mode_first_match_wins() {
        let rules = vec![
            PermissionRule {
                paths: vec![".vfs/**".into()],
                mode: 0o444,
                allow_delete: false,
            },
            PermissionRule {
                paths: vec!["src/**".into()],
                mode: 0o644,
                allow_delete: true,
            },
        ];
        let engine = PermissionEngine::from_rules(rules);

        assert_eq!(engine.compute_mode(p(".vfs/manifest.yaml")), 0o444);
        assert_eq!(engine.compute_mode(p(".vfs/something/deep")), 0o444);
        assert_eq!(engine.compute_mode(p("src/main.rs")), 0o644);
    }

    #[test]
    fn compute_mode_default_when_no_match() {
        let engine = PermissionEngine::new(vec![], 0o644);
        assert_eq!(engine.compute_mode(p("random/file.txt")), 0o644);
    }

    #[test]
    fn compute_mode_defaults_on_unmatched_path() {
        let rules = vec![PermissionRule {
            paths: vec![".vfs/**".into()],
            mode: 0o444,
            allow_delete: false,
        }];
        let engine = PermissionEngine::from_rules(rules);
        assert_eq!(engine.compute_mode(p("something/else.txt")), 0o644);
    }

    // --- PermissionEngine::check() ---

    #[test]
    fn check_read_on_0444_allows() {
        let rules = vec![PermissionRule {
            paths: vec![".vfs/**".into()],
            mode: 0o444,
            allow_delete: false,
        }];
        let engine = PermissionEngine::from_rules(rules);
        assert!(engine.check(".vfs/x.yaml", PermissionOp::Read).is_ok());
    }

    #[test]
    fn check_write_on_0444_denied() {
        let rules = vec![PermissionRule {
            paths: vec![".vfs/**".into()],
            mode: 0o444,
            allow_delete: false,
        }];
        let engine = PermissionEngine::from_rules(rules);
        let err = engine
            .check(".vfs/x.yaml", PermissionOp::Write)
            .unwrap_err();
        assert!(err.to_string().contains("permission denied"));
        assert!(err.to_string().contains("Write"));
    }

    #[test]
    fn check_write_on_0644_allows() {
        let rules = vec![PermissionRule {
            paths: vec!["src/**".into()],
            mode: 0o644,
            allow_delete: true,
        }];
        let engine = PermissionEngine::from_rules(rules);
        assert!(engine.check("src/main.rs", PermissionOp::Read).is_ok());
        assert!(engine.check("src/main.rs", PermissionOp::Write).is_ok());
    }

    #[test]
    fn check_execute_on_0644_denied() {
        let rules = vec![PermissionRule {
            paths: vec!["src/**".into()],
            mode: 0o644,
            allow_delete: false,
        }];
        let engine = PermissionEngine::from_rules(rules);
        assert!(engine.check("src/main.rs", PermissionOp::Execute).is_err());
    }

    #[test]
    fn check_execute_on_0755_allows() {
        let rules = vec![PermissionRule {
            paths: vec!["bin/**".into()],
            mode: 0o755,
            allow_delete: true,
        }];
        let engine = PermissionEngine::from_rules(rules);
        assert!(engine.check("bin/tool", PermissionOp::Read).is_ok());
        assert!(engine.check("bin/tool", PermissionOp::Write).is_ok());
        assert!(engine.check("bin/tool", PermissionOp::Execute).is_ok());
    }

    #[test]
    fn check_explicit_deny_rule() {
        let rules = vec![PermissionRule {
            paths: vec!["secrets/**".into()],
            mode: 0o000, // no access
            allow_delete: false,
        }];
        let engine = PermissionEngine::from_rules(rules);
        assert!(engine.check("secrets/key.pem", PermissionOp::Read).is_err());
        assert!(engine
            .check("secrets/key.pem", PermissionOp::Write)
            .is_err());
        assert!(engine
            .check("secrets/key.pem", PermissionOp::Execute)
            .is_err());
    }

    // --- default_protections() ---

    #[test]
    fn default_protections_has_expected_count() {
        let rules = default_protections();
        assert_eq!(rules.len(), 13);
    }

    #[test]
    fn default_protections_infrastructure_readonly() {
        let rules = default_protections();
        let engine = PermissionEngine::from_rules(rules);

        // Infrastructure files are read-only
        assert!(engine
            .check(".vfs/manifest.yaml", PermissionOp::Read)
            .is_ok());
        assert!(engine
            .check(".vfs/manifest.yaml", PermissionOp::Write)
            .is_err());

        assert!(engine.check(".git/config", PermissionOp::Read).is_ok());
        assert!(engine.check(".git/config", PermissionOp::Write).is_err());

        assert!(engine.check(".gitignore", PermissionOp::Read).is_ok());
        assert!(engine.check(".gitignore", PermissionOp::Write).is_err());

        assert!(engine.check("Cargo.lock", PermissionOp::Read).is_ok());
        assert!(engine.check("Cargo.lock", PermissionOp::Write).is_err());
    }

    #[test]
    fn default_protections_source_writable() {
        let rules = default_protections();
        let engine = PermissionEngine::from_rules(rules);

        // Source files are read-write
        assert!(engine.check("src/main.rs", PermissionOp::Read).is_ok());
        assert!(engine.check("src/main.rs", PermissionOp::Write).is_ok());

        assert!(engine.check("lib/utils.rs", PermissionOp::Read).is_ok());
        assert!(engine.check("lib/utils.rs", PermissionOp::Write).is_ok());

        assert!(engine
            .check("cmd/server/main.go", PermissionOp::Read)
            .is_ok());
        assert!(engine
            .check("cmd/server/main.go", PermissionOp::Write)
            .is_ok());
    }

    #[test]
    fn default_protections_unmatched_default() {
        let rules = default_protections();
        let engine = PermissionEngine::from_rules(rules);

        // Paths not covered by any rule get the default 0o644
        assert!(engine.check("random/file.txt", PermissionOp::Read).is_ok());
        assert!(engine.check("random/file.txt", PermissionOp::Write).is_ok());
    }

    // --- edge cases ---

    #[test]
    fn empty_rules_default_behavior() {
        let engine = PermissionEngine::from_rules(vec![]);
        assert!(engine.check("anything.txt", PermissionOp::Read).is_ok());
        assert!(engine.check("anything.txt", PermissionOp::Write).is_ok());
    }

    #[test]
    fn custom_default_mode_restrictive() {
        let engine = PermissionEngine::new(vec![], 0o444);
        assert!(engine.check("file.txt", PermissionOp::Read).is_ok());
        assert!(engine.check("file.txt", PermissionOp::Write).is_err());
    }

    #[test]
    fn rule_order_matters_first_wins() {
        let rules = vec![
            PermissionRule {
                paths: vec!["src/**".into()],
                mode: 0o444,
                allow_delete: false,
            },
            PermissionRule {
                paths: vec!["src/special/**".into()],
                mode: 0o644,
                allow_delete: true,
            },
        ];
        let engine = PermissionEngine::from_rules(rules);
        // First rule ("src/**") matches everything in src/ including special/
        assert_eq!(engine.compute_mode(p("src/special/file.txt")), 0o444);
    }

    #[test]
    fn permission_error_display() {
        let err = PermissionError::Denied {
            path: ".vfs/secret.yaml".into(),
            op: PermissionOp::Write,
            mode: 0o444,
        };
        let msg = err.to_string();
        assert!(msg.contains(".vfs/secret.yaml"));
        assert!(msg.contains("Write"));
        assert!(msg.contains("444"));
    }

    // --- backend permission rules ---

    #[test]
    fn backend_rule_matches_path() {
        let backend_rules = vec![BackendPermissionRule {
            name: "shared-lib".into(),
            mode: 0o444,
        }];
        let engine = PermissionEngine::new_with_backends(vec![], backend_rules, 0o644);

        // Path under backend mount gets the backend mode
        assert_eq!(engine.compute_mode(p("shared-lib/src/main.rs")), 0o444);
        assert!(engine
            .check("shared-lib/src/main.rs", PermissionOp::Read)
            .is_ok());
        assert!(engine
            .check("shared-lib/src/main.rs", PermissionOp::Write)
            .is_err());
    }

    #[test]
    fn backend_rule_overrides_glob_rule() {
        let glob_rules = vec![PermissionRule {
            paths: vec!["src/**".into()],
            mode: 0o644,
            allow_delete: true,
        }];
        let backend_rules = vec![BackendPermissionRule {
            name: "shared-lib".into(),
            mode: 0o444,
        }];
        let engine = PermissionEngine::new_with_backends(glob_rules, backend_rules, 0o644);

        // Backend rule takes priority over glob rule for backend paths
        assert_eq!(engine.compute_mode(p("shared-lib/src/main.rs")), 0o444);
        // Glob rule still applies for non-backend paths
        assert_eq!(engine.compute_mode(p("src/main.rs")), 0o644);
    }

    #[test]
    fn path_not_under_any_backend_falls_through_to_glob() {
        let glob_rules = vec![PermissionRule {
            paths: vec!["src/**".into()],
            mode: 0o644,
            allow_delete: true,
        }];
        let backend_rules = vec![BackendPermissionRule {
            name: "shared-lib".into(),
            mode: 0o444,
        }];
        let engine = PermissionEngine::new_with_backends(glob_rules, backend_rules, 0o755);

        // Path not under any backend → glob rules apply
        assert_eq!(engine.compute_mode(p("src/main.rs")), 0o644);
        // Path not matching any rule → default
        assert_eq!(engine.compute_mode(p("random/file.txt")), 0o755);
    }

    #[test]
    fn multiple_backend_rules() {
        let backend_rules = vec![
            BackendPermissionRule {
                name: "shared-lib".into(),
                mode: 0o444,
            },
            BackendPermissionRule {
                name: "docs".into(),
                mode: 0o444,
            },
        ];
        let engine = PermissionEngine::new_with_backends(vec![], backend_rules, 0o644);

        assert_eq!(engine.compute_mode(p("shared-lib/pkg/utils.go")), 0o444);
        assert_eq!(engine.compute_mode(p("docs/api/spec.md")), 0o444);
        assert_eq!(engine.compute_mode(p("src/main.rs")), 0o644);
    }
}
