//! COV-5: symbol-level connection + test audit.
//!
//! The graph's own primitives are FILE-scoped (`impact`, `related`,
//! `untested`), so a public function that is defined but never called from any
//! entrypoint — or called but never linked to a test — is invisible as an
//! individual fact and only shows up as a diluted file-level ratio. This
//! module answers, per public function/surface, two questions that today
//! cannot be asked:
//!
//! 1. **is it reachable from a declared entrypoint?**
//! 2. **does it carry a coverage link?**
//!
//! The output is the NAMED list, never a score.
//!
//! ## Buckets (never collapsed)
//!
//! | bucket        | meaning                                                |
//! |---------------|--------------------------------------------------------|
//! | `unreachable` | no caller path from a declared entrypoint              |
//! | `unlinked`    | reachable, but no test link                            |
//! | `ok`          | reachable AND linked                                   |
//!
//! A fourth, explicitly named class sits BESIDE the three buckets (never
//! inside `ok`): [`UnknownReport`] names the files whose language has no
//! public-function extractor, so a partial implementation reports the buckets
//! it can prove and names the rest as unknown instead of collapsing them into
//! `ok`. Every bucket carries the RULE that produced it, so an empty bucket is
//! an explicit zero with a rule, never a success-shaped empty report (the
//! GAP-085 / DF-WARPFS-41 class).
//!
//! ## Rules (exact, so a reader can disagree with them)
//!
//! - **Public function**: a language-level public/exported *function*
//!   definition — Rust `pub fn` (a bare `pub`; `pub(crate)`/`pub(super)` are
//!   NOT public), Go `func`/`func (recv) name` whose name is exported
//!   (leading uppercase), Python `def`/`async def` whose name does not start
//!   with `_`, TypeScript/JavaScript declarations inside an `export`.
//!   Definitions in TEST files are the link source, not the audited surface,
//!   so they are not enumerated. One row per definition; the name is
//!   UNQUALIFIED (methods carry their bare name, e.g. `new`).
//! - **Declared entrypoint**: a file whose role classifies as `entrypoint`
//!   (the same `classify::classify_file` rules GAP-099/COV-1 use: `main.rs` /
//!   `__main__.py` / `index.js` / `Program.cs` / `Main.kt` / `index.php` /
//!   `main.swift` by convention, or an AST-detected `main`/canonical entry
//!   symbol). No hand list.
//! - **Reachable**: a symbol is reachable when its defining file is itself a
//!   declared entrypoint, OR some OTHER file that is reachable-from-an-
//!   entrypoint names it, OR the symbol is named inside its own file beyond
//!   its definition sites (an in-file caller, e.g. a `#[cfg(test)] mod tests`
//!   block) while that file is itself reachable. File reachability is the
//!   forward closure over the reference graph `A → B when A names a public
//!   symbol defined in B`, seeded with the declared entrypoint files.
//! - **Reference** is a lexical identifier occurrence in the file text —
//!   comments and string literals included — for either a public symbol's name
//!   or a module's stem (its file name without extension, the token a caller
//!   writes to reach the module). It deliberately OVER-approximates:
//!   `unreachable` therefore means "neither this file's module nor any of its
//!   public symbols is textually named by a reachable file", which is
//!   conservative and produces no false accusation. The cost is a documented
//!   miss of dead code whose name is common enough to appear incidentally
//!   elsewhere.
//! - **Linked**: some TEST file (the `classify::is_test_file` rules) names the
//!   symbol, or an on-disk COV-2 coverage link ([`CoverageLink`]) targets the
//!   symbol's name or defining file. That is COV-2's `symbol_name_match`
//!   evidence layer applied at symbol granularity.
//!
//! Because every name is unqualified, a name defined in several files shares
//! references across all of them: a reference to `new` is evidence for every
//! file that defines a public `new`. That direction is again the conservative
//! one — it can only make more symbols reachable, never fewer.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::classify;
use crate::coverage_links::CoverageLink;
use crate::parser::Language;

/// Languages whose public-function surface this module can enumerate.
///
/// Anything else in the corpus is reported through [`UnknownReport`] — never
/// silently treated as `ok`.
pub const AUDIT_LANGUAGES: [Language; 5] = [
    Language::Rust,
    Language::Go,
    Language::Python,
    Language::TypeScript,
    Language::JavaScript,
];

/// The lowercase language name used in census/unknown rows.
pub fn language_name(lang: Language) -> &'static str {
    match lang {
        Language::Go => "go",
        Language::Python => "python",
        Language::TypeScript => "typescript",
        Language::Rust => "rust",
        Language::JavaScript => "javascript",
        Language::Java => "java",
        Language::C => "c",
        Language::Cpp => "cpp",
        Language::Ruby => "ruby",
        Language::CSharp => "csharp",
        Language::Kotlin => "kotlin",
        Language::Php => "php",
        Language::Swift => "swift",
        Language::Elixir => "elixir",
        Language::Haskell => "haskell",
        Language::Erlang => "erlang",
        Language::Scala => "scala",
        Language::Zig => "zig",
        Language::Lua => "lua",
        Language::Dart => "dart",
        Language::Clojure => "clojure",
        Language::OCaml => "ocaml",
        Language::R => "r",
        Language::Julia => "julia",
        Language::Elm => "elm",
        Language::Nim => "nim",
        Language::Terraform => "terraform",
    }
}

/// Which of the three buckets a public symbol landed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditBucket {
    /// No caller path from a declared entrypoint.
    Unreachable,
    /// Reachable from an entrypoint, but no test link.
    Unlinked,
    /// Reachable AND linked.
    Ok,
}

impl AuditBucket {
    /// Every bucket, in report order.
    pub const ALL: [AuditBucket; 3] = [
        AuditBucket::Unreachable,
        AuditBucket::Unlinked,
        AuditBucket::Ok,
    ];

    /// The snake_case wire form (the `buckets` object key).
    pub fn as_str(self) -> &'static str {
        match self {
            AuditBucket::Unreachable => "unreachable",
            AuditBucket::Unlinked => "unlinked",
            AuditBucket::Ok => "ok",
        }
    }
}

/// A public function definition — the named unit the audit reports on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PubSymbol {
    pub name: String,
    /// Repo-relative, `/`-separated path of the defining file.
    pub file: String,
    /// 1-indexed definition line.
    pub line: usize,
}

/// One audited public symbol with its evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolAudit {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub bucket: AuditBucket,
    pub reachable: bool,
    pub linked: bool,
    /// How many files name this symbol (its own definition file excluded).
    pub referenced_by_count: usize,
    /// The first `max_evidence` referencing files, sorted.
    pub referenced_by: Vec<String>,
    /// How many TEST files name this symbol.
    pub tested_by_count: usize,
    /// The first `max_evidence` test files naming it, sorted.
    pub tested_by: Vec<String>,
}

impl SymbolAudit {
    /// The named member row a bucket lists.
    pub fn member(&self) -> PubSymbol {
        PubSymbol {
            name: self.name.clone(),
            file: self.file.clone(),
            line: self.line,
        }
    }
}

/// One bucket: its count, the rule that produced it, and its NAMED members.
///
/// `count == 0` always comes with `rule` — an empty bucket is an explicit
/// zero, never a silent empty success.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BucketReport {
    pub count: usize,
    pub rule: String,
    pub symbols: Vec<PubSymbol>,
}

/// The three buckets, keyed exactly as the task names them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Buckets {
    pub unreachable: BucketReport,
    pub unlinked: BucketReport,
    pub ok: BucketReport,
}

impl Buckets {
    /// The bucket report for one bucket.
    pub fn get(&self, bucket: AuditBucket) -> &BucketReport {
        match bucket {
            AuditBucket::Unreachable => &self.unreachable,
            AuditBucket::Unlinked => &self.unlinked,
            AuditBucket::Ok => &self.ok,
        }
    }

    fn get_mut(&mut self, bucket: AuditBucket) -> &mut BucketReport {
        match bucket {
            AuditBucket::Unreachable => &mut self.unreachable,
            AuditBucket::Unlinked => &mut self.unlinked,
            AuditBucket::Ok => &mut self.ok,
        }
    }
}

/// The declared entrypoints the reachability rule started from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EntryPointReport {
    pub count: usize,
    pub rule: String,
    pub files: Vec<String>,
}

/// What the audit could NOT classify — named, never folded into `ok`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UnknownReport {
    /// Number of files that were not audited (unsupported language).
    pub count: usize,
    pub rule: String,
    /// The languages with no public-function extractor that appear in the tree.
    pub languages: Vec<String>,
    pub files: Vec<String>,
}

/// Per-language census — makes a zero distinguishable from an unexercised
/// extractor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageCensus {
    pub language: String,
    /// Files of this language in the corpus.
    pub files: usize,
    /// Public functions extracted from them.
    pub public_symbols: usize,
    /// The extractor rule that produced `public_symbols`.
    pub extractor: String,
}

/// Options for one audit pass.
#[derive(Debug, Clone)]
pub struct AuditOptions {
    /// Evidence lists (`referenced_by` / `tested_by`) are truncated to this
    /// many entries per symbol; the full counts are always carried.
    pub max_evidence: usize,
    /// COV-2 links read from disk. A link whose `target` equals a symbol's
    /// name or defining file marks that symbol linked.
    pub coverage_links: Vec<CoverageLink>,
}

impl Default for AuditOptions {
    fn default() -> Self {
        AuditOptions {
            max_evidence: 10,
            coverage_links: Vec::new(),
        }
    }
}

/// The full audit report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AuditReport {
    pub schema: u32,
    /// Repo root the corpus was collected from (empty for a synthetic corpus).
    pub root: String,
    pub scanned_files: usize,
    pub public_symbols: usize,
    pub entrypoints: EntryPointReport,
    /// Exactly three keys: `unreachable`, `unlinked`, `ok`.
    pub buckets: Buckets,
    /// The class that is explicitly NOT a bucket (unsupported languages).
    pub unknown: UnknownReport,
    pub census: Vec<LanguageCensus>,
    /// Every audited symbol, in deterministic (file, line, name) order.
    pub symbols: Vec<SymbolAudit>,
    /// The rules the pass ran under, in prose.
    pub rules: Vec<String>,
}

impl AuditReport {
    /// The current schema version written to disk and to `--json` output.
    pub const SCHEMA: u32 = 1;
}

/// Directory names never walked by [`collect_corpus`].
///
/// The same pruning `hilo graph warm` applies to discovery (PERF-005 +
/// `guard::DEFAULT_PRUNE_DIRS`), so an audit and a warm see the same tree.
const PRUNE_DIRS: [&str; 10] = [
    "target",
    "node_modules",
    "vendor",
    "__pycache__",
    "venv",
    ".venv",
    "site-packages",
    ".cache",
    ".rustup",
    ".npm",
];

/// Walk `root` and read every source file the audit can classify.
///
/// Returns `(language, repo_relative_path, source)` tuples sorted by path, so
/// the audit result is deterministic. Hidden entries and the dependency/cache
/// directories in [`PRUNE_DIRS`] are skipped — the same shape `graph warm`
/// discovers.
pub fn collect_corpus(root: &Path) -> Vec<(Language, String, String)> {
    let mut out: Vec<(Language, String, String)> = Vec::new();
    collect_into(root, root, &mut out);
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

fn collect_into(root: &Path, dir: &Path, out: &mut Vec<(Language, String, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    children.sort();
    for path in children {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name,
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if PRUNE_DIRS.contains(&name) {
                continue;
            }
            collect_into(root, &path, out);
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let Some(language) = Language::from_path(&path) else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        out.push((language, rel, source));
    }
}

/// Audit a corpus of `(language, repo_relative_path, source)` tuples.
///
/// `root` is echoed into the report (a display value only — nothing is read
/// from it here).
pub fn audit(files: &[(Language, String, String)], root: &str, opts: &AuditOptions) -> AuditReport {
    // ── 1. Per-file facts: entrypoint?, test?, public symbols ──────────────
    let n = files.len();
    let mut is_test = vec![false; n];
    let mut is_entry = vec![false; n];
    let mut file_index: HashMap<&str, usize> = HashMap::with_capacity(n);

    // Definition site of every public symbol: name → symbol indices.
    let mut symbols: Vec<SymbolAudit> = Vec::new();
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    // How many public definitions of `name` live in file `i` — used to tell an
    // in-file REFERENCE apart from the definition's own identifier token.
    let mut def_count: HashMap<(usize, String), usize> = HashMap::new();

    let mut census_by_lang: BTreeMap<&'static str, (usize, usize, String)> = BTreeMap::new();
    let mut unknown_files: Vec<String> = Vec::new();
    let mut unsupported_langs: BTreeSet<String> = BTreeSet::new();

    for (i, (lang, rel, src)) in files.iter().enumerate() {
        file_index.insert(rel.as_str(), i);
        is_test[i] = classify::is_test_file(rel);
        let role = classify::classify_file(*lang, rel, src)
            .map(|c| c.role)
            .unwrap_or_default();
        is_entry[i] = role == "entrypoint";

        let census = census_by_lang
            .entry(language_name(*lang))
            .or_insert_with(|| (0, 0, extractor_rule(*lang)));
        census.0 += 1;

        // Test files are the LINK SOURCE, not the audited surface; generated
        // files are not hand-written surface either. Both stay in the corpus
        // (they can reference symbols) but define no audited symbol and, when
        // their language has no extractor, are not counted as `unknown` noise
        // either — the census still names the language.
        if is_test[i] || role == "generated" {
            continue;
        }

        if !AUDIT_LANGUAGES.contains(lang) {
            unsupported_langs.insert(language_name(*lang).to_string());
            unknown_files.push(rel.clone());
            continue;
        }

        let found = extract_public_functions(*lang, src);
        census.1 += found.len();
        for (name, line) in found {
            let idx = symbols.len();
            *def_count.entry((i, name.clone())).or_insert(0) += 1;
            by_name.entry(name.clone()).or_default().push(idx);
            symbols.push(SymbolAudit {
                name,
                file: rel.clone(),
                line,
                bucket: AuditBucket::Ok,
                reachable: false,
                linked: false,
                referenced_by_count: 0,
                referenced_by: Vec::new(),
                tested_by_count: 0,
                tested_by: Vec::new(),
            });
        }
    }

    // ── 2. Reference graph: A → B when A names a public symbol of B, or A
    //      names B's MODULE (its file stem) ─────────────────────────────────
    let mut refs: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); symbols.len()];
    // `edges[a]` holds FILE indices (the files whose public surface or module
    // name `a` references), so the reachability closure below can index
    // `reachable_file` directly.
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    // Identifier occurrence counts per file — the in-file reference signal.
    let mut occurrences: Vec<BTreeMap<&str, usize>> = Vec::with_capacity(n);

    // Module indices: a file stem → the files it names (`ops` → `…/ops.rs`).
    // Files are also reached by MODULE PATH (`use crate::ops`, `mod ops;`,
    // `hilo_fuse::ops`), which no public-function name carries — without this
    // a leaf module used by a reachable file would read as unreachable. Stems
    // are over-approximate by construction, which is again the conservative
    // direction (more reachable, never fewer).
    let mut by_stem: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (_, rel, _)) in files.iter().enumerate() {
        by_stem.entry(file_stem(rel)).or_default().push(i);
    }

    for (a, (_, rel, src)) in files.iter().enumerate() {
        let occ = identifier_counts(src);
        let mut targets: BTreeSet<usize> = BTreeSet::new();
        for &ident in occ.keys() {
            if let Some(stem_files) = by_stem.get(ident) {
                for &f in stem_files {
                    if f != a {
                        targets.insert(f);
                    }
                }
            }
            let Some(defs) = by_name.get(ident) else {
                continue;
            };
            for &d in defs {
                if symbols[d].file == *rel {
                    continue; // a definition is not a cross-file reference
                }
                refs[d].insert(a);
                if let Some(&file_of_def) = file_index.get(symbols[d].file.as_str()) {
                    targets.insert(file_of_def);
                }
            }
        }
        edges[a] = targets.into_iter().collect();
        occurrences.push(occ);
    }

    // ── 3. File reachability: forward closure from the entrypoints ────────
    let mut reachable_file = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..n {
        if is_entry[i] {
            reachable_file[i] = true;
            stack.push(i);
        }
    }
    while let Some(a) = stack.pop() {
        for &b in &edges[a] {
            if !reachable_file[b] {
                reachable_file[b] = true;
                stack.push(b);
            }
        }
    }

    // ── 4. Bucket every symbol ────────────────────────────────────────────
    let entrypoint_files: Vec<String> = (0..n)
        .filter(|&i| is_entry[i])
        .map(|i| files[i].1.clone())
        .collect();

    for idx in 0..symbols.len() {
        let own_file = symbols[idx].file.clone();
        let own_name = symbols[idx].name.clone();
        let own_index = file_index.get(own_file.as_str()).copied();
        let own_is_entry = own_index.map(|i| is_entry[i]).unwrap_or(false);
        let own_file_reachable = own_index.map(|i| reachable_file[i]).unwrap_or(false);

        // A symbol named inside its OWN file beyond its definition sites (an
        // in-file caller, often a `#[cfg(test)] mod tests` block) counts as a
        // reference — but only once the file itself is reachable, so a symbol
        // nobody ever names stays `unreachable` and the definition's own
        // identifier token is never mistaken for a use.
        let in_file_reference = own_index
            .map(|fi| {
                let seen = occurrences[fi].get(own_name.as_str()).copied().unwrap_or(0);
                let defined = def_count.get(&(fi, own_name.clone())).copied().unwrap_or(0);
                seen > defined
            })
            .unwrap_or(false);

        // Files that name this symbol, split into all / test-only.
        let mut all_refs: Vec<String> = Vec::new();
        let mut test_refs: Vec<String> = Vec::new();
        let mut referencing_reachable = false;
        for &a in &refs[idx] {
            all_refs.push(files[a].1.clone());
            if is_test[a] {
                test_refs.push(files[a].1.clone());
            }
            referencing_reachable |= reachable_file[a];
        }
        all_refs.sort();
        test_refs.sort();

        let linked_by_cov2 = opts
            .coverage_links
            .iter()
            .any(|l| l.target == own_name || l.target == own_file);

        let reachable =
            own_is_entry || referencing_reachable || (own_file_reachable && in_file_reference);
        let linked = !test_refs.is_empty() || linked_by_cov2;
        let bucket = if !reachable {
            AuditBucket::Unreachable
        } else if !linked {
            AuditBucket::Unlinked
        } else {
            AuditBucket::Ok
        };

        let symbol = &mut symbols[idx];
        symbol.reachable = reachable;
        symbol.linked = linked;
        symbol.bucket = bucket;
        symbol.referenced_by_count = all_refs.len();
        symbol.referenced_by = all_refs.iter().take(opts.max_evidence).cloned().collect();
        symbol.tested_by_count = test_refs.len();
        symbol.tested_by = test_refs.iter().take(opts.max_evidence).cloned().collect();
    }

    // ── 5. Assemble buckets (named members, deterministic order) ──────────
    symbols.sort_by(|a, b| {
        (a.file.as_str(), a.line, a.name.as_str()).cmp(&(b.file.as_str(), b.line, b.name.as_str()))
    });
    let mut buckets = Buckets::default();
    for bucket in AuditBucket::ALL {
        let members: Vec<PubSymbol> = symbols
            .iter()
            .filter(|s| s.bucket == bucket)
            .map(SymbolAudit::member)
            .collect();
        let report = BucketReport {
            count: members.len(),
            rule: bucket_rule(bucket, &entrypoint_files),
            symbols: members,
        };
        *buckets.get_mut(bucket) = report;
    }

    let unknown_langs: Vec<String> = unsupported_langs.into_iter().collect();
    unknown_files.sort();
    let supported_list = AUDIT_LANGUAGES
        .iter()
        .map(|l| language_name(*l))
        .collect::<Vec<_>>()
        .join(", ");
    let unknown = UnknownReport {
        count: unknown_files.len(),
        rule: if unknown_files.is_empty() {
            format!(
                "every source file in the corpus is in a language with a public-function extractor ({supported_list}); nothing was left unaudited"
            )
        } else {
            format!(
                "{} source file(s) in a language without a public-function extractor were NOT audited and are named here rather than counted as ok; supported languages: {supported_list}",
                unknown_files.len()
            )
        },
        languages: unknown_langs,
        files: unknown_files,
    };

    let census: Vec<LanguageCensus> = census_by_lang
        .into_iter()
        .map(
            |(language, (files, public_symbols, extractor))| LanguageCensus {
                language: language.to_string(),
                files,
                public_symbols,
                extractor,
            },
        )
        .collect();

    let entrypoints = EntryPointReport {
        count: entrypoint_files.len(),
        rule: format!(
            "files whose role classifies as `entrypoint` via classify_file (filename convention: main.rs/__main__.py/index.js/Program.cs/Main.kt/index.php/main.swift, or an AST-detected main / canonical entry symbol); {} found",
            entrypoint_files.len()
        ),
        files: entrypoint_files,
    };

    AuditReport {
        schema: AuditReport::SCHEMA,
        root: root.to_string(),
        scanned_files: n,
        public_symbols: symbols.len(),
        entrypoints,
        buckets,
        unknown,
        census,
        symbols,
        rules: vec![
            "public function: Rust bare `pub fn`, Go exported func/method (leading uppercase), Python `def`/`async def` not starting with `_`, TypeScript/JavaScript declarations inside an `export`".into(),
            "declared entrypoint: `classify_file` role == `entrypoint` (code-derived, no hand list)".into(),
            "reachable: the defining file is itself a declared entrypoint, or a file reachable-from-an-entrypoint names the symbol; file reachability is the forward closure of `A names a public symbol of B` from the entrypoints".into(),
            "reference: lexical identifier occurrence in the file text (comments and strings included) — over-approximate by construction, so `unreachable` is a conservative no-false-positive claim".into(),
            "linked: a test file (is_test_file rules) names the symbol, or an on-disk COV-2 coverage link targets the symbol's name or file".into(),
            "test files and generated files define no audited symbol (they are the link source and machine output); their language is still counted in the census".into(),
        ],
    }
}

fn extractor_rule(lang: Language) -> String {
    match lang {
        Language::Rust => {
            "tree-sitter rust: function_item with a bare `pub` visibility_modifier".into()
        }
        Language::Go => {
            "tree-sitter go: function_declaration / method_declaration whose name starts uppercase"
                .into()
        }
        Language::Python => {
            "tree-sitter python: function_definition whose name does not start with `_`".into()
        }
        Language::TypeScript | Language::JavaScript => {
            "tree-sitter ts/js: declarations inside an `export_statement`".into()
        }
        other => format!("no public-function extractor for {}", language_name(other)),
    }
}

fn bucket_rule(bucket: AuditBucket, entrypoint_files: &[String]) -> String {
    let entry_rule = if entrypoint_files.is_empty() {
        "no declared entrypoint was found in the corpus, so NOTHING can be proven reachable — every public symbol is `unreachable` (the honest partial result, not a pass)"
            .to_string()
    } else {
        format!(
            "reachability computed from {} declared entrypoint file(s)",
            entrypoint_files.len()
        )
    };
    match bucket {
        AuditBucket::Unreachable => format!(
            "no caller path from a declared entrypoint: no file reachable from an entrypoint names the symbol, and its defining file is not itself an entrypoint ({entry_rule})"
        ),
        AuditBucket::Unlinked => format!(
            "reachable from a declared entrypoint, but no test file names the symbol and no COV-2 coverage link targets it ({entry_rule})"
        ),
        AuditBucket::Ok => format!(
            "reachable from a declared entrypoint AND carries a coverage link ({entry_rule})"
        ),
    }
}

// ─────────────────────────── public-symbol extraction ───────────────────────

/// Public function definitions `(name, 1-indexed line)` in `source`.
///
/// Returns an empty vec for a language outside [`AUDIT_LANGUAGES`].
pub fn extract_public_functions(lang: Language, source: &str) -> Vec<(String, usize)> {
    let Some(ts_lang) = ts_language(lang) else {
        return Vec::new();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    walk(tree.root_node(), bytes, lang, &mut out, false);
    // Source order (line, then name) — the order a reader sees in the file.
    out.sort_by_key(|a| (a.1, a.0.clone()));
    out.dedup();
    out
}

fn ts_language(lang: Language) -> Option<tree_sitter::Language> {
    Some(match lang {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        _ => return None,
    })
}

fn walk(
    node: tree_sitter::Node,
    source: &[u8],
    lang: Language,
    out: &mut Vec<(String, usize)>,
    inside_function: bool,
) {
    match lang {
        Language::Rust => {
            if node.kind() == "function_item" && rust_is_public(node, source) {
                push_name(node, source, out);
            }
        }
        Language::Go => {
            if matches!(node.kind(), "function_declaration" | "method_declaration") {
                if let Some(name) = name_of(node, source) {
                    if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                        out.push((name, node.start_position().row + 1));
                    }
                }
            }
        }
        Language::Python => {
            // Nested `def`s are not module/class surface: only module-level
            // functions and class methods count.
            if node.kind() == "function_definition" && !inside_function {
                if let Some(name) = name_of(node, source) {
                    if !name.starts_with('_') {
                        out.push((name, node.start_position().row + 1));
                    }
                }
            }
        }
        Language::TypeScript | Language::JavaScript if node.kind() == "export_statement" => {
            // Only the declarations DIRECTLY exported are public; a `const`
            // nested inside an exported function body is not.
            collect_exported(node, source, out);
            return;
        }
        _ => {}
    }

    let next_inside = inside_function || node.kind() == "function_definition";
    let mut cursor = node.walk();
    let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();
    for child in children {
        walk(child, source, lang, out, next_inside);
    }
}

/// Names declared directly by one `export_statement` (TS/JS).
fn collect_exported(node: tree_sitter::Node, source: &[u8], out: &mut Vec<(String, usize)>) {
    let mut cursor = node.walk();
    let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();
    for child in children {
        match child.kind() {
            "function_declaration"
            | "generator_function_declaration"
            | "class_declaration"
            | "abstract_class_declaration" => push_name(child, source, out),
            "lexical_declaration" | "variable_declaration" => {
                let mut inner = child.walk();
                let declarators: Vec<tree_sitter::Node> = child.children(&mut inner).collect();
                for declarator in declarators {
                    if declarator.kind() == "variable_declarator" {
                        push_name(declarator, source, out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn push_name(node: tree_sitter::Node, source: &[u8], out: &mut Vec<(String, usize)>) {
    if let Some(name) = name_of(node, source) {
        out.push((name, node.start_position().row + 1));
    }
}

fn name_of(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let n = node.child_by_field_name("name")?;
    let text = n.utf8_text(source).ok()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// A Rust item is public only when its visibility modifier is a BARE `pub` —
/// `pub(crate)` / `pub(super)` / `pub(in …)` are crate-internal.
fn rust_is_public(node: tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();
    children.iter().any(|c| {
        c.kind() == "visibility_modifier" && c.utf8_text(source).map(str::trim) == Ok("pub")
    })
}

// ─────────────────────────── identifier scan ────────────────────────────────

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// The module stem of a repo-relative path: `hilo-fuse/src/ops.rs` → `ops`.
///
/// This is the token a caller writes to reach the module (`use crate::ops`,
/// `mod ops;`, `hilo_fuse::ops`), so it is the second way a file becomes
/// reachable. Conventional stems (`lib`, `mod`, `main`, `index`) are
/// deliberately NOT special-cased: they are ordinary module names here, and
/// excluding them would invent an unlisted exception to a uniform rule.
fn file_stem(rel: &str) -> &str {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.split('.').next().unwrap_or(name)
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Every identifier-like token in `source` with its occurrence count —
/// comments and strings included.
///
/// Deliberately over-approximate (see the module docs): the scan never drops a
/// token, so a symbol can be reported unreachable only when its name is
/// textually absent from every reachable file.
fn identifier_counts(source: &str) -> BTreeMap<&str, usize> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if is_ident_start(bytes[i]) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            *counts.entry(&source[start..i]).or_insert(0) += 1;
        } else {
            i += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage_links::EvidenceKind;

    fn corpus(files: &[(&str, &str)]) -> Vec<(Language, String, String)> {
        files
            .iter()
            .map(|(rel, src)| {
                let lang = Language::from_path(Path::new(rel)).expect("known extension");
                (lang, (*rel).to_string(), (*src).to_string())
            })
            .collect()
    }

    fn names_of(lang: Language, src: &str) -> Vec<String> {
        extract_public_functions(lang, src)
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    }

    /// AC4: a fixture with one of each lands exactly one symbol in each bucket.
    #[test]
    fn fixture_one_hit_per_bucket() {
        let files = corpus(&[
            // Declared entrypoint (filename convention): names both live
            // symbols, so their files become reachable.
            (
                "src/main.rs",
                "fn main() {\n    wired_one();\n    unlinked_one();\n}\n",
            ),
            ("src/wired.rs", "pub fn wired_one() {\n    let _ = 1;\n}\n"),
            (
                "src/unlinked.rs",
                "pub fn unlinked_one() {\n    let _ = 2;\n}\n",
            ),
            // No reachable file names this one.
            (
                "src/orphan.rs",
                "pub fn orphan_one() {\n    let _ = 3;\n}\n",
            ),
            // Test file: names only the wired symbol.
            (
                "src/wired_test.rs",
                "#[test]\nfn t_wired() {\n    wired_one();\n}\n",
            ),
        ]);

        let report = audit(&files, "", &AuditOptions::default());

        assert_eq!(report.public_symbols, 3, "three public fns in the fixture");
        assert_eq!(report.entrypoints.count, 1);
        assert_eq!(report.entrypoints.files, vec!["src/main.rs".to_string()]);

        assert_eq!(report.buckets.unreachable.count, 1);
        assert_eq!(report.buckets.unlinked.count, 1);
        assert_eq!(report.buckets.ok.count, 1);

        assert_eq!(
            report.buckets.unreachable.symbols[0].name, "orphan_one",
            "the never-named pub fn is unreachable"
        );
        assert_eq!(
            report.buckets.unlinked.symbols[0].name, "unlinked_one",
            "named from a reachable non-test file, no test link"
        );
        assert_eq!(
            report.buckets.ok.symbols[0].name, "wired_one",
            "reachable and test-linked"
        );

        // Evidence is carried, not just a verdict.
        let ok = report
            .symbols
            .iter()
            .find(|s| s.name == "wired_one")
            .expect("wired_one row");
        assert_eq!(ok.tested_by, vec!["src/wired_test.rs".to_string()]);
        assert!(ok.referenced_by.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn rust_pub_crate_is_not_public_and_bare_pub_is() {
        let src = "\
pub fn exported() {}
pub(crate) fn crate_only() {}
pub(super) fn super_only() {}
fn private() {}
pub async fn async_exported() {}
pub unsafe fn unsafe_exported() {}
";
        assert_eq!(
            names_of(Language::Rust, src),
            vec!["exported", "async_exported", "unsafe_exported"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn rust_methods_inside_impl_are_public_surface() {
        let src = "struct E;\nimpl E {\n    pub fn build() {}\n    fn hidden() {}\n}\n";
        assert_eq!(names_of(Language::Rust, src), vec!["build".to_string()]);
    }

    #[test]
    fn go_export_rule_is_leading_uppercase() {
        let src = "package p\n\nfunc Exported() {}\nfunc unexported() {}\nfunc (s *S) Method() {}\nfunc (s *S) other() {}\n";
        assert_eq!(
            names_of(Language::Go, src),
            vec!["Exported".to_string(), "Method".to_string()]
        );
    }

    #[test]
    fn python_private_underscore_is_not_public() {
        let src = "def public_one():\n    pass\n\n\ndef _private():\n    pass\n\n\nasync def async_public():\n    pass\n";
        assert_eq!(
            names_of(Language::Python, src),
            vec!["public_one".to_string(), "async_public".to_string()]
        );
    }

    #[test]
    fn python_nested_def_is_not_surface() {
        let src = "def outer():\n    def inner():\n        pass\n    return inner\n";
        assert_eq!(names_of(Language::Python, src), vec!["outer".to_string()]);
    }

    #[test]
    fn typescript_only_exports_are_public() {
        let src = "export function exported() {}\nfunction local() {}\n\
                   export const arrow = () => {};\nconst hidden = () => {};\n\
                   export class Widget {}\nclass Internal {}\n";
        assert_eq!(
            names_of(Language::TypeScript, src),
            vec!["exported", "arrow", "Widget"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn typescript_exported_body_does_not_leak_inner_consts() {
        let src = "export function outer() {\n  const inner = () => {};\n  return inner;\n}\n";
        assert_eq!(
            names_of(Language::TypeScript, src),
            vec!["outer".to_string()]
        );
    }

    #[test]
    fn unsupported_language_is_named_never_counted_ok() {
        let files = corpus(&[
            ("main.rs", "fn main() {}\npub fn live() {}\n"),
            ("app.rb", "def ruby_thing\nend\n"),
        ]);
        let report = audit(&files, "", &AuditOptions::default());
        assert_eq!(report.unknown.count, 1);
        assert_eq!(report.unknown.languages, vec!["ruby".to_string()]);
        assert_eq!(report.unknown.files, vec!["app.rb".to_string()]);
        assert!(report.unknown.rule.contains("NOT audited"));
        // The ruby file is not silently an `ok` row.
        assert!(report.symbols.iter().all(|s| s.file != "app.rb"));
    }

    #[test]
    fn test_file_definitions_are_not_audited_surface() {
        let files = corpus(&[
            ("src/main.rs", "fn main() {}\n"),
            ("src/helper_test.rs", "pub fn test_only_helper() {}\n"),
        ]);
        let report = audit(&files, "", &AuditOptions::default());
        assert_eq!(report.public_symbols, 0);
        assert!(report
            .symbols
            .iter()
            .all(|s| s.file != "src/helper_test.rs"));
    }

    #[test]
    fn empty_bucket_is_an_explicit_zero_with_a_rule() {
        let files = corpus(&[
            ("main.rs", "fn main() {}\npub fn ok_one() {}\n"),
            ("t_test.rs", "fn t() { ok_one(); }\n"),
        ]);
        let report = audit(&files, "", &AuditOptions::default());
        assert_eq!(report.buckets.unreachable.count, 0);
        assert!(!report.buckets.unreachable.rule.is_empty());
        assert_eq!(report.buckets.unlinked.count, 0);
        assert!(!report.buckets.unlinked.rule.is_empty());
        assert_eq!(report.buckets.ok.count, 1);
        // The bucket key set is exactly the three the task names, in order.
        let keys: Vec<&str> = AuditBucket::ALL.iter().map(|b| b.as_str()).collect();
        assert_eq!(keys, vec!["unreachable", "unlinked", "ok"]);
    }

    #[test]
    fn no_entrypoint_means_nothing_is_proven_reachable() {
        let files = corpus(&[("src/lib.rs", "pub fn helper() {}\n")]);
        let report = audit(&files, "", &AuditOptions::default());
        assert_eq!(report.entrypoints.count, 0);
        assert_eq!(report.buckets.unreachable.count, 1);
        assert!(report
            .buckets
            .unreachable
            .rule
            .contains("no declared entrypoint"));
    }

    #[test]
    fn definition_itself_is_not_a_reference() {
        // The only occurrence of `lonely` is its own definition site.
        let files = corpus(&[
            ("main.rs", "fn main() {}\n"),
            (
                "lib.rs",
                "pub fn lonely() {\n    let _ = lonely_helper();\n}\npub fn lonely_helper() {}\n",
            ),
        ]);
        let report = audit(&files, "", &AuditOptions::default());
        assert_eq!(
            report.buckets.unreachable.count, 2,
            "self-references inside the defining file do not make a symbol reachable: {:#?}",
            report.buckets
        );
    }

    #[test]
    fn in_file_reference_counts_only_beyond_the_definition() {
        // `used_inside` is called from the file's own test module; `dead_one`
        // is only ever its own definition. Both live in a reachable file.
        let files = corpus(&[
            ("src/main.rs", "fn main() { helper(); }\n"),
            (
                "src/helper.rs",
                "pub fn helper() {}\n\npub fn used_inside() {}\n\npub fn dead_one() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        used_inside();\n    }\n}\n",
            ),
        ]);
        let report = audit(&files, "", &AuditOptions::default());
        let bucket_of = |name: &str| {
            report
                .symbols
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} row"))
                .bucket
        };
        assert_eq!(bucket_of("helper"), AuditBucket::Unlinked);
        assert_eq!(
            bucket_of("used_inside"),
            AuditBucket::Unlinked,
            "an in-file caller in a reachable file is a reference"
        );
        assert_eq!(
            bucket_of("dead_one"),
            AuditBucket::Unreachable,
            "a symbol nobody ever names stays unreachable"
        );
    }

    #[test]
    fn module_path_reference_reaches_a_leaf_files_symbols() {
        // `ops.rs` is reached only by its MODULE name (`use crate::ops`), so
        // its public fns must not read as unreachable.
        let files = corpus(&[
            (
                "src/main.rs",
                "mod ops;\n\nfn main() {\n    ops::start();\n}\n",
            ),
            ("src/ops.rs", "pub fn start() {}\n"),
        ]);
        let report = audit(&files, "", &AuditOptions::default());
        assert_eq!(report.entrypoints.count, 1);
        assert_eq!(report.buckets.unreachable.count, 0, "{:#?}", report.buckets);
        // No test names `start`, so it is reachable-but-unlinked.
        assert_eq!(report.buckets.unlinked.count, 1);
    }

    #[test]
    fn file_stem_strips_directory_and_extension() {
        assert_eq!(file_stem("hilo-fuse/src/ops.rs"), "ops");
        assert_eq!(file_stem("main.rs"), "main");
        assert_eq!(file_stem("a/b/c.d.e"), "c");
    }

    #[test]
    fn cov2_link_marks_a_reachable_symbol_linked() {
        let files = corpus(&[
            ("src/main.rs", "fn main() { thing(); }\n"),
            ("src/thing.rs", "pub fn thing() {}\n"),
        ]);
        let opts = AuditOptions {
            coverage_links: vec![CoverageLink::new(
                "tests/thing_test.rs",
                "thing",
                EvidenceKind::DeclaredMapping,
            )],
            ..AuditOptions::default()
        };
        let report = audit(&files, "", &opts);
        assert_eq!(report.buckets.ok.count, 1, "{:#?}", report.buckets);
        assert_eq!(report.buckets.unlinked.count, 0);
    }

    #[test]
    fn census_reports_every_language_even_at_zero_symbols() {
        let files = corpus(&[
            ("main.rs", "fn main() {}\npub fn a() {}\n"),
            ("lib.rs", "pub fn b() {}\n"),
        ]);
        let report = audit(&files, "", &AuditOptions::default());
        let rust = report
            .census
            .iter()
            .find(|c| c.language == "rust")
            .expect("rust census row");
        assert_eq!(rust.files, 2);
        assert_eq!(rust.public_symbols, 2);
        assert!(!rust.extractor.is_empty());
    }

    #[test]
    fn report_round_trips_and_bucket_keys_are_the_three_named() {
        let files = corpus(&[("main.rs", "fn main() {}\npub fn a() {}\n")]);
        let report = audit(&files, "/repo", &AuditOptions::default());
        let value = serde_json::to_value(&report).unwrap();
        let mut keys: Vec<&str> = value["buckets"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["ok", "unlinked", "unreachable"]);
        assert_eq!(value["schema"].as_u64(), Some(1));
        assert_eq!(value["root"], "/repo");
        let back: AuditReport = serde_json::from_value(value).unwrap();
        assert_eq!(back, report);
    }

    #[test]
    fn identifiers_scan_is_stable_and_inclusive() {
        let counts = identifier_counts("let x = foo_bar(42); // baz\n");
        assert!(counts.contains_key("foo_bar"));
        assert!(counts.contains_key("x"));
        assert!(
            counts.contains_key("baz"),
            "comments are included by design"
        );
        assert!(!counts.contains_key("42"));
        // Occurrences are COUNTED, not just collected: this is what tells an
        // in-file reference apart from the definition's own token.
        let counts = identifier_counts("foo(); foo(); foo\n");
        assert_eq!(counts.get("foo"), Some(&3));
    }

    #[test]
    fn collect_corpus_prunes_caches_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        std::fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        std::fs::write(dir.path().join("src/b.rs"), "pub fn b() {}\n").unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(
            dir.path().join("node_modules/big.js"),
            "export function x() {}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(".hidden/h.rs"), "pub fn h() {}\n").unwrap();

        let corpus = collect_corpus(dir.path());
        let paths: Vec<&str> = corpus.iter().map(|(_, p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/b.rs"]);
    }
}
