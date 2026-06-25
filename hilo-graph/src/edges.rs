//! Cross-repo external edge detection and formatting.
//!
//! External edges use the format `external:<repo-name>:<path>` in the `to`
//! field of an edge, indicating that the dependency lives in a different
//! repository within the multi-repo workspace.

use std::collections::{HashMap, HashSet};

/// Format an `external:` edge target string.
///
/// # Examples
/// ```
/// let target = hilo_graph::edges::format_external_edge("shared-lib", "pkg/utils.go");
/// assert_eq!(target, "external:shared-lib:pkg/utils.go");
/// ```
pub fn format_external_edge(repo: &str, path: &str) -> String {
    format!("external:{}:{}", repo, path)
}

/// Parse an `external:repo:path` string into `(repo, relative_path)`.
/// Returns `None` if the string doesn't start with `external:`.
///
/// # Examples
/// ```
/// assert_eq!(
///     hilo_graph::edges::parse_external_edge("external:shared-lib:pkg/utils.go"),
///     Some(("shared-lib", "pkg/utils.go"))
/// );
/// assert_eq!(hilo_graph::edges::parse_external_edge("std:fmt"), None);
/// ```
pub fn parse_external_edge(to: &str) -> Option<(&str, &str)> {
    let rest = to.strip_prefix("external:")?;
    let (repo, path) = rest.split_once(':')?;

    Some((repo, path))
}

/// Check whether an edge target `to` is an external edge reference.
pub fn is_external(to: &str) -> bool {
    to.starts_with("external:")
}

/// Given an import path and workspace metadata, determine whether the import
/// points to a file in another workspace repo.
///
/// `repo_mounts` maps repo names → their mount directory prefixes (e.g.,
/// `"shared-lib"` → `"shared-lib/"`).  Returns `Some((repo_name, relative_path))`
/// if the path falls under a workspace repo mount.
pub fn find_external_repo(
    path: &str,
    repo_mounts: &HashMap<String, String>,
) -> Option<(String, String)> {
    for (repo, prefix) in repo_mounts {
        if let Some(rel) = path.strip_prefix(prefix.as_str()) {
            // `strip_prefix` only matches exact prefix boundaries, so e.g.
            // `shared-lib/` won't match `shared-lib-extra/main.go`.
            let rel_clean = rel.trim_start_matches('/').trim_start_matches('\\');
            return Some((repo.clone(), rel_clean.to_string()));
        }
    }
    None
}

/// Build a map of repo-name → mount-directory-prefix from a list of
/// `(repo_name, mount_at)` pairs.  The prefix is derived from the last
/// path component of the mount `at` location.
pub fn build_repo_mounts(pairs: &[(String, String)]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (repo, at) in pairs {
        let prefix = at
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(at.as_str());
        // Ensure trailing slash for prefix matching.
        let prefix_with_slash = if prefix.ends_with('/') {
            prefix.to_string()
        } else {
            format!("{}/", prefix)
        };
        map.insert(repo.clone(), prefix_with_slash);
    }
    map
}

/// Resolve the set of known local files from a list of edges, building a
/// `HashSet<&str>` of `to` values that are NOT external.
pub fn local_target_set(edges: &[hilo_metadata::inventory::Edge]) -> HashSet<&str> {
    edges.iter().map(|e| e.to.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_external_edge() {
        assert_eq!(
            format_external_edge("shared-lib", "pkg/utils.go"),
            "external:shared-lib:pkg/utils.go"
        );
    }

    #[test]
    fn test_parse_external_edge_valid() {
        assert_eq!(
            parse_external_edge("external:shared-lib:pkg/utils.go"),
            Some(("shared-lib", "pkg/utils.go"))
        );
    }

    #[test]
    fn test_parse_external_edge_not_external() {
        assert_eq!(parse_external_edge("std:fmt"), None);
        assert_eq!(parse_external_edge("pkg/models"), None);
    }

    #[test]
    fn test_is_external() {
        assert!(is_external("external:repo:file.go"));
        assert!(!is_external("std:fmt"));
        assert!(!is_external("pkg/models"));
    }

    #[test]
    fn test_find_external_repo_matches() {
        let mut mounts = HashMap::new();
        mounts.insert("shared-lib".into(), "shared-lib/".into());
        mounts.insert("auth-service".into(), "auth-service/".into());

        assert_eq!(
            find_external_repo("shared-lib/pkg/utils.go", &mounts),
            Some(("shared-lib".into(), "pkg/utils.go".into()))
        );
        assert_eq!(
            find_external_repo("auth-service/src/handler.go", &mounts),
            Some(("auth-service".into(), "src/handler.go".into()))
        );
    }

    #[test]
    fn test_find_external_repo_no_match() {
        let mut mounts = HashMap::new();
        mounts.insert("shared-lib".into(), "shared-lib/".into());
        assert_eq!(find_external_repo("std:fmt", &mounts), None);
        assert_eq!(find_external_repo("vendor/lib", &mounts), None);
    }

    #[test]
    fn test_find_external_repo_prefix_guard() {
        let mut mounts = HashMap::new();
        mounts.insert("shared-lib".into(), "shared-lib/".into());
        // "shared-lib-extra" should NOT match the "shared-lib/" prefix because
        // the next char after the prefix is '-', not '/'.
        assert_eq!(
            find_external_repo("shared-lib-extra/main.go", &mounts),
            None
        );
    }

    #[test]
    fn test_build_repo_mounts() {
        let pairs = vec![
            ("auth-service".into(), "/mnt/vfs/auth-service/".into()),
            ("shared-lib".into(), "/mnt/vfs/shared-lib/".into()),
            ("docs".into(), "docs/".into()),
        ];
        let map = build_repo_mounts(&pairs);
        assert_eq!(map.get("auth-service"), Some(&"auth-service/".to_string()));
        assert_eq!(map.get("shared-lib"), Some(&"shared-lib/".to_string()));
        assert_eq!(map.get("docs"), Some(&"docs/".to_string()));
    }
}
