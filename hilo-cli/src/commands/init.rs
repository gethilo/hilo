//! `hilo init` — create the `.vfs/` directory tree and a default manifest.

use std::path::Path;

use anyhow::{Context, Result};
use hilo_core::manifest::{Manifest, Project};
use hilo_metadata::inventory;

use crate::commands::guard;
use crate::commands::hooks;

/// Dedicated `.gitignore` section for the derived DuckDB cache (DF-WARPFS-96).
///
/// The wildcard covers DuckDB sidecars as well as `graph.db` itself. Inventory
/// truth (`.vfs/manifest.yaml` and `.vfs/graph/edges.jsonl`) stays tracked.
pub const HILO_DUCKDB_IGNORE_HEADER: &str =
    "# Hilo: derived DuckDB cache is rebuildable (edges.jsonl stays committed)";
pub const HILO_DUCKDB_IGNORE_ENTRY: &str = ".vfs/graph/graph.db*";

/// The remaining rebuildable `.vfs/graph` cache state managed by `hilo init`.
pub const HILO_MANAGED_IGNORE_HEADER: &str =
    "# --- hilo managed: rebuildable .vfs cache state (installed by `hilo init`) ---";
const HILO_MANAGED_IGNORE_ENTRIES: &[&str] = &[
    ".vfs/graph/graph.db",
    ".vfs/graph/graph.db.wal",
    ".vfs/graph/graph.duckdb",
    ".vfs/graph/graph.duckdb.wal",
    ".vfs/graph/.last_warm",
    ".vfs/graph/.parse_cache.json",
    ".vfs/graph/.symbols_cache.json",
    ".vfs/graph/.last_reconcile",
];

fn append_ignore_section(out: &mut String, header: &str, entries: &[&str]) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(header);
    out.push('\n');
    for entry in entries {
        out.push_str(entry);
        out.push('\n');
    }
}

/// Ensure `<root>/.gitignore` carries the managed rebuildable-cache sections.
///
/// Idempotent: a second call with both sections already present leaves the file
/// byte-identical. Pre-existing content is preserved verbatim. Repositories
/// initialized by an older Hilo release receive the new DuckDB wildcard even
/// when they already carry the legacy managed-cache header.
pub fn ensure_gitignore_entries(root: &Path) -> Result<()> {
    let gitignore = root.join(".gitignore");
    let existing = match std::fs::read_to_string(&gitignore) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", gitignore.display())),
    };

    let has_duckdb_entry = existing
        .lines()
        .any(|line| line == HILO_DUCKDB_IGNORE_ENTRY);
    let has_managed_section = existing
        .lines()
        .any(|line| line == HILO_MANAGED_IGNORE_HEADER);
    if has_duckdb_entry && has_managed_section {
        return Ok(());
    }

    let mut out = existing;
    if !has_duckdb_entry {
        append_ignore_section(
            &mut out,
            HILO_DUCKDB_IGNORE_HEADER,
            &[HILO_DUCKDB_IGNORE_ENTRY],
        );
    }
    if !has_managed_section {
        append_ignore_section(
            &mut out,
            HILO_MANAGED_IGNORE_HEADER,
            HILO_MANAGED_IGNORE_ENTRIES,
        );
    }

    std::fs::write(&gitignore, out)
        .with_context(|| format!("failed to write {}", gitignore.display()))
}

/// Create the `.vfs/` structure and a minimal `manifest.yaml` in the current
/// directory.
///
/// PERF-005: refuses to run when the current directory is the user's HOME —
/// an accidental `hilo init` there scatters `.vfs/` state across the home
/// directory. `--allow-home` restores the previous behavior explicitly.
///
/// Idempotent: if `.vfs/manifest.yaml` already exists it is left untouched.
///
/// GAP-087: `no_hooks` (the `--no-hooks` flag) skips git hook installation
/// entirely — see [`run_in`].
pub fn run(allow_home: bool, no_hooks: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    run_in(&cwd, allow_home, None, no_hooks)
}

/// Injectable core of [`run`] (PERF-005): root and HOME are parameters so the
/// refusal/override contract is testable in a temporary directory without
/// touching the real HOME or the process working directory.
///
/// When `no_hooks` is true the `.vfs/` tree and manifest are still created, but
/// nothing under `.git/hooks/` is created or modified: pre-existing hook files
/// stay byte-identical and `.git/hooks/` is not even created. Hook installation
/// only ever accompanies a fresh manifest, so the opt-out is honored on an
/// already-initialized project too.
pub fn run_in(
    root: &Path,
    allow_home: bool,
    home: Option<std::path::PathBuf>,
    no_hooks: bool,
) -> Result<()> {
    let cwd = root;

    guard::ensure_not_home_with(cwd, allow_home, home)?;

    // Create the .vfs/ directory tree (idempotent — safe to call repeatedly).
    inventory::create_vfs_structure(cwd).context("failed to create .vfs directory structure")?;

    // DF-WARPFS-96: install a dedicated wildcard for the rebuildable DuckDB
    // cache, so the first ordinary commit does not add a multi-megabyte
    // graph.db or its sidecars. Inventory truth (manifest.yaml, edges.jsonl)
    // stays tracked; the managed section below also covers other caches.
    ensure_gitignore_entries(cwd).context("failed to install .gitignore entries")?;

    let manifest_path = cwd.join(".vfs").join("manifest.yaml");

    // Idempotent: never overwrite an existing manifest.
    if manifest_path.exists() {
        println!("Initialized Hilo in {}", cwd.display());
        return Ok(());
    }

    // Derive the project name from the current directory's name.
    let dir_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hilo-project".to_string());

    // Manifest does not derive Default, so construct it field by field.
    // Every sub-struct implements Default (verified in hilo-core/manifest.rs).
    let manifest = Manifest {
        version: 2,
        project: Project {
            name: dir_name,
            description: String::new(),
        },
        interfaces: Default::default(),
        repos: Vec::new(),
        backends: Default::default(),
        metadata: Default::default(),
        graph: Default::default(),
        permissions: Default::default(),
        triggers: Vec::new(),
        rules: Vec::new(),
        // DF-WARPFS-59: plugins are NOT implemented — the field stays empty
        // and `skip_serializing_if` omits the block entirely, so `hilo init`
        // no longer advertises a plugin feature that never fires.
        plugins: Vec::new(),
        discovery: Default::default(),
        sandbox: Default::default(),
        performance: Default::default(),
    };

    let yaml = serde_yaml::to_string(&manifest).context("failed to serialize manifest to YAML")?;
    std::fs::write(&manifest_path, yaml)
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;

    println!("Initialized Hilo in {}", cwd.display());

    // Install git hooks for auto-metadata-update on commit and pull.
    //
    // GAP-087: `--no-hooks` returns before `.git/hooks/` is touched at all, so
    // an opt-out can neither create nor modify a hook file.
    if no_hooks {
        println!("Skipped git hook installation (--no-hooks)");
        return Ok(());
    }

    hooks::install_hooks(cwd).context("failed to install git hooks")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_refuses_home_root() {
        let home = TempDir::new().unwrap();
        let err = run_in(home.path(), false, Some(home.path().to_path_buf()), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("--allow-home"),
            "error must name the flag: {msg}"
        );
        assert!(
            !home.path().join(".vfs").exists(),
            "refused init must not create .vfs in HOME"
        );
    }

    #[test]
    fn init_allow_home_overrides() {
        let home = TempDir::new().unwrap();
        run_in(home.path(), true, Some(home.path().to_path_buf()), false).unwrap();
        assert!(
            home.path().join(".vfs").join("manifest.yaml").exists(),
            "--allow-home init must proceed"
        );
    }

    #[test]
    fn init_normal_project_still_works() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        run_in(
            project.path(),
            false,
            Some(home.path().to_path_buf()),
            false,
        )
        .unwrap();
        assert!(project.path().join(".vfs").join("manifest.yaml").exists());
    }

    // ─────────────────── GAP-087: --no-hooks opt-out ───────────────────

    /// Snapshot `<root>/.git/hooks` as sorted (name, bytes) pairs.
    ///
    /// Returns an empty vec when the directory does not exist — an absent
    /// `.git/hooks/` and an empty one are both "nothing for Hilo to touch".
    fn snapshot_hooks(root: &Path) -> Vec<(String, Vec<u8>)> {
        let hooks_dir = root.join(".git").join("hooks");
        if !hooks_dir.exists() {
            return Vec::new();
        }
        let mut entries: Vec<(String, Vec<u8>)> = std::fs::read_dir(&hooks_dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                let bytes = std::fs::read(entry.path()).unwrap();
                (name, bytes)
            })
            .collect();
        entries.sort();
        entries
    }

    /// Create `<root>/.git/hooks/post-commit` with caller-supplied content.
    fn write_existing_hook(root: &Path, content: &str) {
        let hooks_dir = root.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(hooks_dir.join("post-commit"), content).unwrap();
    }

    #[test]
    fn init_no_hooks_leaves_existing_hooks_byte_identical() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let custom = "#!/bin/sh\necho 'pre-existing project hook'\n";
        write_existing_hook(project.path(), custom);
        let before = snapshot_hooks(project.path());

        run_in(project.path(), false, Some(home.path().to_path_buf()), true).unwrap();

        assert!(
            project.path().join(".vfs").join("manifest.yaml").exists(),
            "opt-out must still create the manifest"
        );
        assert_eq!(
            snapshot_hooks(project.path()),
            before,
            ".git/hooks must be byte-identical after --no-hooks"
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join(".git/hooks/post-commit")).unwrap(),
            custom,
            "pre-existing hook content must be untouched"
        );
        assert!(
            !project.path().join(".git/hooks/post-merge").exists(),
            "--no-hooks must not create a post-merge hook"
        );
    }

    #[test]
    fn init_no_hooks_does_not_create_git_hooks_dir() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();

        run_in(project.path(), false, Some(home.path().to_path_buf()), true).unwrap();

        assert!(project.path().join(".vfs").join("manifest.yaml").exists());
        assert!(
            !project.path().join(".git").exists(),
            "--no-hooks must not create .git/ (or .git/hooks/) at all"
        );
    }

    #[test]
    fn init_no_hooks_honors_opt_out_when_manifest_exists() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();

        // Pre-existing project: manifest already present, plus custom hook
        // content. Re-running with --no-hooks must not touch either.
        std::fs::create_dir_all(project.path().join(".vfs")).unwrap();
        std::fs::write(
            project.path().join(".vfs").join("manifest.yaml"),
            "version: 2\nproject:\n  name: existing\n",
        )
        .unwrap();
        let custom = "#!/bin/sh\necho 'custom hook'\n";
        write_existing_hook(project.path(), custom);
        let manifest_before =
            std::fs::read(project.path().join(".vfs").join("manifest.yaml")).unwrap();
        let hooks_before = snapshot_hooks(project.path());

        run_in(project.path(), false, Some(home.path().to_path_buf()), true).unwrap();

        assert_eq!(
            std::fs::read(project.path().join(".vfs").join("manifest.yaml")).unwrap(),
            manifest_before,
            "existing manifest must stay byte-identical"
        );
        assert_eq!(
            snapshot_hooks(project.path()),
            hooks_before,
            "opt-out on an initialized project must leave hooks untouched"
        );
        assert!(!project.path().join(".git/hooks/post-merge").exists());
    }

    #[test]
    fn init_without_no_hooks_still_installs_hooks() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        write_existing_hook(project.path(), "#!/bin/sh\necho 'keep me'\n");

        run_in(
            project.path(),
            false,
            Some(home.path().to_path_buf()),
            false,
        )
        .unwrap();

        let post_commit =
            std::fs::read_to_string(project.path().join(".git/hooks/post-commit")).unwrap();
        assert!(
            post_commit.contains("echo 'keep me'"),
            "existing hook content must be preserved on install"
        );
        assert!(
            post_commit.contains("### HILO"),
            "default init must still install the Hilo block"
        );
        assert!(
            project.path().join(".git/hooks/post-merge").exists(),
            "default init must still install post-merge"
        );
    }

    #[test]
    fn init_no_hooks_still_preserves_home_guard() {
        let home = TempDir::new().unwrap();
        let err = run_in(home.path(), false, Some(home.path().to_path_buf()), true).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("--allow-home"),
            "error must name the flag: {msg}"
        );
        assert!(!home.path().join(".vfs").exists());
    }

    #[test]
    fn init_installs_gitignore_block() {
        let project = TempDir::new().unwrap();
        run_in(project.path(), false, None, true).unwrap();

        let gi = std::fs::read_to_string(project.path().join(".gitignore")).unwrap();
        let duckdb_section = format!("{HILO_DUCKDB_IGNORE_HEADER}\n{HILO_DUCKDB_IGNORE_ENTRY}\n");
        assert!(
            gi.contains(&duckdb_section),
            "init must install the exact dedicated DuckDB section: {gi}"
        );
        assert!(
            gi.contains(HILO_MANAGED_IGNORE_HEADER),
            "init must retain the managed cache section: {gi}"
        );
        for entry in [
            HILO_DUCKDB_IGNORE_ENTRY,
            ".vfs/graph/graph.db",
            ".vfs/graph/graph.db.wal",
            ".vfs/graph/graph.duckdb",
            ".vfs/graph/graph.duckdb.wal",
            ".vfs/graph/.parse_cache.json",
            ".vfs/graph/.symbols_cache.json",
            ".vfs/graph/.last_reconcile",
        ] {
            assert!(
                gi.lines().any(|line| line == entry),
                "managed sections must contain the exact entry {entry}: {gi}"
            );
        }
        // Inventory truth stays tracked — never in either managed section.
        assert!(!gi.contains(".vfs/manifest.yaml"));
        assert!(!gi.contains(".vfs/graph/edges.jsonl"));
    }

    #[test]
    fn init_gitignore_ignores_duckdb_but_not_inventory() {
        let project = TempDir::new().unwrap();
        let git_init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(project.path())
            .status()
            .unwrap();
        assert!(git_init.success(), "git init must succeed for the fixture");

        run_in(project.path(), false, None, true).unwrap();

        for cache_path in [
            ".vfs/graph/graph.db",
            ".vfs/graph/graph.db.wal",
            ".vfs/graph/graph.db.tmp",
        ] {
            let verdict = std::process::Command::new("git")
                .args(["check-ignore", cache_path])
                .current_dir(project.path())
                .status()
                .unwrap();
            assert_eq!(
                verdict.code(),
                Some(0),
                "{cache_path} must be ignored after init"
            );
        }

        for inventory_path in [".vfs/manifest.yaml", ".vfs/graph/edges.jsonl"] {
            let verdict = std::process::Command::new("git")
                .args(["check-ignore", inventory_path])
                .current_dir(project.path())
                .status()
                .unwrap();
            assert_eq!(
                verdict.code(),
                Some(1),
                "{inventory_path} is source-of-truth inventory and must stay visible"
            );
        }
    }

    #[test]
    fn init_gitignore_idempotent_and_preserves_existing() {
        let project = TempDir::new().unwrap();
        let existing = "# my project ignores\n*.tmp\n";
        std::fs::write(project.path().join(".gitignore"), existing).unwrap();

        run_in(project.path(), false, None, true).unwrap();
        let first = std::fs::read_to_string(project.path().join(".gitignore")).unwrap();
        assert!(
            first.starts_with(existing),
            "pre-existing content preserved verbatim"
        );
        assert!(first.contains(HILO_DUCKDB_IGNORE_HEADER));
        assert!(first.contains(HILO_MANAGED_IGNORE_HEADER));

        // Re-init with an existing manifest → idempotent, byte-identical.
        run_in(project.path(), false, None, true).unwrap();
        let second = std::fs::read_to_string(project.path().join(".gitignore")).unwrap();
        assert_eq!(
            first, second,
            "re-init must not duplicate or modify either section"
        );
        assert_eq!(
            second.matches(HILO_DUCKDB_IGNORE_HEADER).count(),
            1,
            "re-init must leave one DuckDB section"
        );
        assert_eq!(
            second.matches(HILO_DUCKDB_IGNORE_ENTRY).count(),
            1,
            "re-init must leave one DuckDB wildcard"
        );
    }

    #[test]
    fn init_upgrades_legacy_managed_block_with_duckdb_wildcard() {
        let project = TempDir::new().unwrap();
        let legacy =
            format!("{HILO_MANAGED_IGNORE_HEADER}\n.vfs/graph/graph.db\n.vfs/graph/graph.db.wal\n");
        std::fs::write(project.path().join(".gitignore"), &legacy).unwrap();

        run_in(project.path(), false, None, true).unwrap();
        let upgraded = std::fs::read_to_string(project.path().join(".gitignore")).unwrap();

        assert!(
            upgraded.starts_with(&legacy),
            "legacy rules must be preserved"
        );
        assert_eq!(upgraded.matches(HILO_MANAGED_IGNORE_HEADER).count(), 1);
        assert_eq!(upgraded.matches(HILO_DUCKDB_IGNORE_HEADER).count(), 1);
        assert_eq!(upgraded.matches(HILO_DUCKDB_IGNORE_ENTRY).count(), 1);
    }
}
