use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

// ============================================================
// ERROR TYPE
// ============================================================

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("Invalid field: {0}")]
    InvalidField(String),
}

// ============================================================
// DESERIALIZER HELPERS
// ============================================================

/// Accept either a single string or a list of strings, always producing Vec<String>.
fn string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StrOrVec {
        Single(String),
        Multi(Vec<String>),
    }
    Ok(match StrOrVec::deserialize(deserializer)? {
        StrOrVec::Single(s) => vec![s],
        StrOrVec::Multi(v) => v,
    })
}

/// Accept either a string or an integer, always producing a String.
/// Handles YAML parsers that resolve `0444` as an integer.
fn string_or_int<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StrOrInt {
        Str(String),
        Int(i64),
    }
    Ok(match StrOrInt::deserialize(deserializer)? {
        StrOrInt::Str(s) => s,
        StrOrInt::Int(n) => n.to_string(),
    })
}

// ============================================================
// DEFAULT FUNCTIONS
// ============================================================

fn default_true() -> bool {
    true
}
fn default_version() -> u32 {
    2
}
fn default_mount_point() -> String {
    "/mnt/vfs/project".to_string()
}
fn default_ninep_listen() -> String {
    "0.0.0.0:5640".to_string()
}
fn default_mcp_transport() -> String {
    "stdio".to_string()
}
fn default_mcp_port() -> u16 {
    8766
}
fn default_repo_ref() -> String {
    "main".to_string()
}
fn default_ttl() -> u32 {
    3600
}
fn default_max_edges() -> u64 {
    100_000
}
fn default_impact_depth() -> u32 {
    5
}
fn default_default_mode() -> String {
    "0644".to_string()
}
fn default_trigger_timeout() -> String {
    "5s".to_string()
}
fn default_plugin_priority() -> u32 {
    10
}
fn default_fi_strategy() -> String {
    "directory".to_string()
}
fn default_cache_path() -> String {
    ".vfs/cache/".to_string()
}
fn default_cache_max_size() -> String {
    "1GB".to_string()
}
fn default_attr_timeout() -> f64 {
    1.0
}
fn default_entry_timeout() -> f64 {
    1.0
}
fn default_max_read() -> u32 {
    131_072
}
fn default_max_write() -> u32 {
    131_072
}
fn default_duckdb_threads() -> u32 {
    4
}
fn default_duckdb_memory() -> String {
    "512MB".to_string()
}
fn default_debounce() -> String {
    "500ms".to_string()
}
fn default_max_concurrent() -> u32 {
    8
}
fn default_languages() -> Vec<String> {
    vec![
        "go".into(),
        "python".into(),
        "typescript".into(),
        "rust".into(),
        "javascript".into(),
        "java".into(),
        "c".into(),
        "cpp".into(),
        "ruby".into(),
    ]
}
fn default_test_patterns() -> Vec<String> {
    vec![
        "*_test.go".into(),
        "test_*.py".into(),
        "*.test.ts".into(),
        "*.spec.ts".into(),
    ]
}

// ============================================================
// MANIFEST (top-level)
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Manifest {
    #[serde(default = "default_version")]
    pub version: u32,
    pub project: Project,
    #[serde(default)]
    pub interfaces: Interfaces,
    #[serde(default)]
    pub repos: Vec<Repo>,
    #[serde(default)]
    pub backends: Backends,
    #[serde(default)]
    pub metadata: Metadata,
    #[serde(default)]
    pub graph: Graph,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    #[serde(default)]
    pub rules: Vec<QueryRule>,
    #[serde(default)]
    pub plugins: Vec<Plugin>,
    #[serde(default)]
    pub discovery: Discovery,
    #[serde(default)]
    pub sandbox: Sandbox,
    #[serde(default)]
    pub performance: Performance,
}

// ============================================================
// PROJECT
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

// ============================================================
// INTERFACES
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Interfaces {
    #[serde(default)]
    pub fuse: FuseInterface,
    #[serde(default)]
    pub ninep: NinepInterface,
    #[serde(default)]
    pub cli: CliInterface,
    #[serde(default)]
    pub mcp: McpInterface,
    #[serde(default)]
    pub golib: GolibInterface,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FuseInterface {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_mount_point")]
    pub mount_point: String,
    #[serde(default)]
    pub allow_other: bool,
    #[serde(default)]
    pub direct_io: bool,
    #[serde(default = "default_true")]
    pub auto_unmount: bool,
}

impl Default for FuseInterface {
    fn default() -> Self {
        Self {
            enabled: true,
            mount_point: default_mount_point(),
            allow_other: false,
            direct_io: false,
            auto_unmount: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NinepInterface {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_ninep_listen")]
    pub listen: String,
}

impl Default for NinepInterface {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: default_ninep_listen(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliInterface {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for CliInterface {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpInterface {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_mcp_transport")]
    pub transport: String,
    #[serde(default = "default_mcp_port")]
    pub port: u16,
}

impl Default for McpInterface {
    fn default() -> Self {
        Self {
            enabled: true,
            transport: default_mcp_transport(),
            port: default_mcp_port(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct GolibInterface {
    #[serde(default)]
    pub enabled: bool,
}

// ============================================================
// REPO
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(rename = "ref", default = "default_repo_ref")]
    pub git_ref: String,
    #[serde(default)]
    pub at: String,
    #[serde(default = "default_true")]
    pub writable: bool,
    #[serde(default = "default_true")]
    pub auto_pull: bool,
    #[serde(default)]
    pub manifest: Option<String>,
}

// ============================================================
// BACKENDS
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Backends {
    #[serde(default)]
    pub s3: Vec<S3Backend>,
    #[serde(default)]
    pub remote: Vec<RemoteBackend>,
    #[serde(default)]
    pub local: Vec<LocalBackend>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Backend {
    pub bucket: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub cache: Option<String>,
    #[serde(default = "default_ttl")]
    pub ttl: u32,
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub lazy_load: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteBackend {
    pub url: String,
    #[serde(default)]
    pub at: String,
    #[serde(rename = "ref", default = "default_repo_ref")]
    pub git_ref: String,
    #[serde(default)]
    pub cache: Option<String>,
    #[serde(default)]
    pub auto_pull: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalBackend {
    pub path: String,
    #[serde(default)]
    pub at: String,
    #[serde(default = "default_true")]
    pub writable: bool,
}

// ============================================================
// METADATA
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct Metadata {
    #[serde(default)]
    pub namespaces: Vec<String>,
    #[serde(default)]
    pub auto: AutoMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoMetadata {
    #[serde(default = "default_true")]
    pub hash: bool,
    #[serde(default = "default_true")]
    pub last_modified: bool,
    #[serde(default = "default_true")]
    pub complexity: bool,
}

impl Default for AutoMetadata {
    fn default() -> Self {
        Self {
            hash: true,
            last_modified: true,
            complexity: true,
        }
    }
}

// ============================================================
// GRAPH
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    #[serde(default)]
    pub edges: String,
    #[serde(default)]
    pub blob_index: String,
    #[serde(default)]
    pub duckdb_cache: String,
    #[serde(default)]
    pub auto_discover: AutoDiscover,
    #[serde(default)]
    pub impact: Impact,
    #[serde(default = "default_true")]
    pub cross_repo: bool,
    #[serde(default = "default_max_edges")]
    pub max_edges_per_file: u64,
    #[serde(default = "default_true")]
    pub deduplicate: bool,
    #[serde(default)]
    pub extensions: Vec<GraphExtension>,
}

impl Default for Graph {
    fn default() -> Self {
        Self {
            edges: String::new(),
            blob_index: String::new(),
            duckdb_cache: String::new(),
            auto_discover: AutoDiscover::default(),
            impact: Impact::default(),
            cross_repo: true,
            max_edges_per_file: default_max_edges(),
            deduplicate: true,
            extensions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoDiscover {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_languages")]
    pub languages: Vec<String>,
    #[serde(default = "default_true")]
    pub on_first_read: bool,
    #[serde(default = "default_true")]
    pub on_write: bool,
    #[serde(default)]
    pub on_mount: bool,
}

impl Default for AutoDiscover {
    fn default() -> Self {
        Self {
            enabled: true,
            languages: default_languages(),
            on_first_read: true,
            on_write: true,
            on_mount: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Impact {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_impact_depth")]
    pub max_depth: u32,
    #[serde(default = "default_true")]
    pub parallel: bool,
}

impl Default for Impact {
    fn default() -> Self {
        Self {
            enabled: true,
            max_depth: default_impact_depth(),
            parallel: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphExtension {
    pub name: String,
    pub pattern: String,
    pub relation: String,
}

// ============================================================
// PERMISSIONS
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
    #[serde(default = "default_default_mode")]
    pub default_mode: String,
    #[serde(default)]
    pub backends: Vec<BackendPermission>,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            default_mode: default_default_mode(),
            backends: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRule {
    pub paths: Vec<String>,
    #[serde(deserialize_with = "string_or_int")]
    pub mode: String,
    #[serde(default)]
    pub allow_delete: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendPermission {
    pub name: String,
    #[serde(deserialize_with = "string_or_int")]
    pub mode: String,
}

// ============================================================
// TRIGGERS
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trigger {
    pub name: String,
    pub when: String,
    #[serde(deserialize_with = "string_or_vec")]
    pub on: Vec<String>,
    pub run: String,
    #[serde(rename = "async", default = "default_true")]
    pub r#async: bool,
    #[serde(default = "default_trigger_timeout")]
    pub timeout: String,
    #[serde(default)]
    pub debounce: Option<String>,
    #[serde(default)]
    pub on_failure: Option<String>,
    #[serde(default)]
    pub on_success: Option<TriggerAction>,
}

impl Default for Trigger {
    fn default() -> Self {
        Self {
            name: String::new(),
            when: String::new(),
            on: Vec::new(),
            run: String::new(),
            r#async: true,
            timeout: default_trigger_timeout(),
            debounce: None,
            on_failure: None,
            on_success: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TriggerAction {
    #[serde(default)]
    pub set_xattr: Option<String>,
}

// ============================================================
// QUERY RULES
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRule {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub query: String,
}

// ============================================================
// PLUGINS
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub name: String,
    pub wasm: String,
    #[serde(default)]
    pub hooks: Vec<PluginHook>,
    #[serde(default)]
    pub provides: Option<PluginProvides>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginHook {
    pub on: String,
    #[serde(default)]
    pub languages: Option<Vec<String>>,
    #[serde(default = "default_plugin_priority")]
    pub priority: u32,
}

impl Default for PluginHook {
    fn default() -> Self {
        Self {
            on: String::new(),
            languages: None,
            priority: default_plugin_priority(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PluginProvides {
    #[serde(default)]
    pub edge_types: Vec<String>,
    #[serde(default)]
    pub metadata_namespaces: Vec<String>,
}

// ============================================================
// DISCOVERY
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct Discovery {
    #[serde(default)]
    pub feature_inference: FeatureInference,
    #[serde(default)]
    pub test_association: TestAssociation,
    #[serde(default)]
    pub generated_detection: GeneratedDetection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureInference {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_fi_strategy")]
    pub strategy: String,
    #[serde(default)]
    pub override_file: Option<String>,
}

impl Default for FeatureInference {
    fn default() -> Self {
        Self {
            enabled: true,
            strategy: default_fi_strategy(),
            override_file: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestAssociation {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_test_patterns")]
    pub patterns: Vec<String>,
}

impl Default for TestAssociation {
    fn default() -> Self {
        Self {
            enabled: true,
            patterns: default_test_patterns(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedDetection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub markers: Vec<GeneratedMarker>,
}

impl Default for GeneratedDetection {
    fn default() -> Self {
        Self {
            enabled: true,
            markers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct GeneratedMarker {
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

// ============================================================
// SANDBOX
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Sandbox {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub isolate_network: bool,
    #[serde(default)]
    pub isolate_pid: bool,
    #[serde(default)]
    pub read_only_root: bool,
    #[serde(default)]
    pub writable_paths: Vec<String>,
}

// ============================================================
// PERFORMANCE
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct Performance {
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub fuse: FusePerf,
    #[serde(default)]
    pub duckdb: DuckDbPerf,
    #[serde(default)]
    pub triggers: TriggerPerf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    #[serde(default = "default_cache_path")]
    pub path: String,
    #[serde(default = "default_cache_max_size")]
    pub max_size: String,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            path: default_cache_path(),
            max_size: default_cache_max_size(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FusePerf {
    #[serde(default = "default_attr_timeout")]
    pub attr_timeout: f64,
    #[serde(default = "default_entry_timeout")]
    pub entry_timeout: f64,
    #[serde(default = "default_max_read")]
    pub max_read: u32,
    #[serde(default = "default_max_write")]
    pub max_write: u32,
}

impl Default for FusePerf {
    fn default() -> Self {
        Self {
            attr_timeout: default_attr_timeout(),
            entry_timeout: default_entry_timeout(),
            max_read: default_max_read(),
            max_write: default_max_write(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuckDbPerf {
    #[serde(default = "default_duckdb_threads")]
    pub threads: u32,
    #[serde(default = "default_duckdb_memory")]
    pub memory_limit: String,
}

impl Default for DuckDbPerf {
    fn default() -> Self {
        Self {
            threads: default_duckdb_threads(),
            memory_limit: default_duckdb_memory(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerPerf {
    #[serde(default = "default_debounce")]
    pub debounce_default: String,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
}

impl Default for TriggerPerf {
    fn default() -> Self {
        Self {
            debounce_default: default_debounce(),
            max_concurrent: default_max_concurrent(),
        }
    }
}

// ============================================================
// MANIFEST IMPL
// ============================================================

impl Manifest {
    pub fn from_file(path: &str) -> Result<Manifest, ManifestError> {
        let contents = std::fs::read_to_string(path)?;
        Self::parse(&contents)
    }

    pub fn parse(yaml: &str) -> Result<Manifest, ManifestError> {
        let manifest: Manifest = serde_yaml::from_str(yaml).map_err(|e| {
            let msg = e.to_string();
            if msg.contains("unknown field") || msg.contains("unknown variant") {
                ManifestError::InvalidField(msg)
            } else {
                ManifestError::Parse(e)
            }
        })?;
        Ok(manifest)
    }
}

// ============================================================
// TESTS
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    mod defaults {
        use super::*;

        #[test]
        fn true_returns_true() {
            assert_eq!(default_true(), true);
        }
        #[test]
        fn version_is_2() {
            assert_eq!(default_version(), 2);
        }
        #[test]
        fn mount_point() {
            assert_eq!(default_mount_point(), "/mnt/vfs/project");
        }
        #[test]
        fn ninep_listen() {
            assert_eq!(default_ninep_listen(), "0.0.0.0:5640");
        }
        #[test]
        fn mcp_transport() {
            assert_eq!(default_mcp_transport(), "stdio");
        }
        #[test]
        fn mcp_port() {
            assert_eq!(default_mcp_port(), 8766);
        }
        #[test]
        fn repo_ref() {
            assert_eq!(default_repo_ref(), "main");
        }
        #[test]
        fn ttl_is_3600() {
            assert_eq!(default_ttl(), 3600);
        }
        #[test]
        fn max_edges() {
            assert_eq!(default_max_edges(), 100_000);
        }
        #[test]
        fn impact_depth() {
            assert_eq!(default_impact_depth(), 5);
        }
        #[test]
        fn default_mode() {
            assert_eq!(default_default_mode(), "0644");
        }
        #[test]
        fn trigger_timeout() {
            assert_eq!(default_trigger_timeout(), "5s");
        }
        #[test]
        fn plugin_priority() {
            assert_eq!(default_plugin_priority(), 10);
        }
        #[test]
        fn fi_strategy() {
            assert_eq!(default_fi_strategy(), "directory");
        }
        #[test]
        fn cache_path() {
            assert_eq!(default_cache_path(), ".vfs/cache/");
        }
        #[test]
        fn cache_max_size() {
            assert_eq!(default_cache_max_size(), "1GB");
        }
        #[test]
        fn attr_timeout_positive() {
            assert!(default_attr_timeout() > 0.0);
        }
        #[test]
        fn entry_timeout_positive() {
            assert!(default_entry_timeout() > 0.0);
        }
        #[test]
        fn max_read() {
            assert_eq!(default_max_read(), 131_072);
        }
        #[test]
        fn max_write() {
            assert_eq!(default_max_write(), 131_072);
        }
        #[test]
        fn duckdb_threads() {
            assert_eq!(default_duckdb_threads(), 4);
        }
        #[test]
        fn duckdb_memory() {
            assert_eq!(default_duckdb_memory(), "512MB");
        }
        #[test]
        fn debounce() {
            assert_eq!(default_debounce(), "500ms");
        }
        #[test]
        fn max_concurrent() {
            assert_eq!(default_max_concurrent(), 8);
        }

        #[test]
        fn languages_has_all_9() {
            let v = default_languages();
            assert_eq!(v.len(), 9);
            for lang in &[
                "go",
                "python",
                "typescript",
                "rust",
                "javascript",
                "java",
                "c",
                "cpp",
                "ruby",
            ] {
                assert!(v.contains(&lang.to_string()), "missing: {}", lang);
            }
        }

        #[test]
        fn test_patterns_has_all_4() {
            let v = default_test_patterns();
            assert_eq!(v.len(), 4);
            for pat in &["*_test.go", "test_*.py", "*.test.ts", "*.spec.ts"] {
                assert!(v.contains(&pat.to_string()), "missing: {}", pat);
            }
        }
    }

    mod serde_helpers {
        use super::*;

        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "string_or_vec")]
            items: Vec<String>,
        }

        #[test]
        fn string_or_vec_single() {
            let v: Wrapper = serde_yaml::from_str("items: hello").unwrap();
            assert_eq!(v.items, vec!["hello"]);
        }

        #[test]
        fn string_or_vec_multi() {
            let v: Wrapper = serde_yaml::from_str("items: [a, b, c]").unwrap();
            assert_eq!(v.items, vec!["a", "b", "c"]);
        }

        #[derive(Deserialize)]
        struct IntWrapper {
            #[serde(deserialize_with = "string_or_int")]
            value: String,
        }

        #[test]
        fn string_or_int_string() {
            let v: IntWrapper = serde_yaml::from_str("value: \"0444\"").unwrap();
            assert_eq!(v.value, "0444");
        }

        #[test]
        fn string_or_int_integer() {
            let v: IntWrapper = serde_yaml::from_str("value: 444").unwrap();
            assert_eq!(v.value, "444");
        }
    }
}
