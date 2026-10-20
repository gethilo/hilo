// S3 read-only + write-through backend implementation
use aws_config;
use aws_sdk_s3 as s3;
use aws_sdk_s3::config::ProvideCredentials;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;

/// Pure decision core for [`head_error_is_not_found`] — unit-testable without
/// a live endpoint or SDK error plumbing.
///
/// Layered, strongest signal first:
/// 1. HTTP status 404 — unambiguous on every S3-compatible endpoint, and HEAD
///    has no body to confuse the parser.
/// 2. The modeled `HeadObjectError::NotFound` variant (typed).
/// 3. The wire error code `NotFound` — covers endpoints that answer a HEAD 404
///    with an EMPTY body (moto, some MinIO builds): the SDK then deserializes
///    `Unhandled` and only the code survives.
///
/// The old guard matched `format!("{e}").contains("NotFound")`, which missed
/// the empty-body rendering AND could false-positive on unrelated errors whose
/// text happened to mention NotFound (DF-WARPFS-10).
fn head_failure_means_absent(
    status: Option<u16>,
    typed_not_found: bool,
    code: Option<&str>,
) -> bool {
    status == Some(404) || typed_not_found || code == Some("NotFound")
}

/// Whether a HEAD-object failure means "the remote key does not exist"
/// (a normal state on a first sync, NOT a failure — DF-WARPFS-10).
fn head_error_is_not_found(
    e: &s3::error::SdkError<s3::operation::head_object::HeadObjectError>,
) -> bool {
    use s3::error::ProvideErrorMetadata;
    use s3::operation::head_object::HeadObjectError;

    let status = e.raw_response().map(|r| r.status().as_u16());
    let typed = e
        .as_service_error()
        .is_some_and(|se: &HeadObjectError| se.is_not_found());
    let code = e.as_service_error().and_then(|se| se.code());
    head_failure_means_absent(status, typed, code)
}

/// Pure decision core for [`aws_failure_to_s3_error`]: render the typed code
/// header from what survived the failure. A wire/model code wins; an HTTP
/// status alone (empty-body endpoints, moto et al.) still names the failure.
/// `None` = neither survived = a transport-level failure (DNS, timeout,
/// connection refused), which carries no service meaning.
///
/// (DF-WARPFS-24 — a bare `Display` string gave the user only
/// "aws error: service error" with no code, no endpoint, no bucket.)
fn aws_error_code_header(code: Option<&str>, status: Option<u16>) -> Option<String> {
    match (code, status) {
        (Some(c), Some(s)) => Some(format!("{c} ({s})")),
        (Some(c), None) => Some(c.to_string()),
        (None, Some(s)) => Some(format!("HTTP {s}")),
        (None, None) => None,
    }
}

/// Pure mapping of an AWS SDK failure into an [`S3Error`]: when a service
/// error code and/or HTTP status survived, the error becomes the typed
/// [`S3Error::Service`] with the code header FIRST (`NoSuchBucket (404)`);
/// the raw SDK Display string is kept as the detail line. Transport-level
/// failures (no code, no status) stay [`S3Error::Aws`] (DF-WARPFS-24).
fn aws_failure_to_s3_error(code: Option<&str>, status: Option<u16>, detail: String) -> S3Error {
    match aws_error_code_header(code, status) {
        Some(code) => S3Error::Service { code, detail },
        None => S3Error::Aws(detail),
    }
}

/// Attach the where-context (`bucket 'b' @ endpoint`, [`S3Client::op_ctx`])
/// to an S3 error (DF-WARPFS-24): for a typed [`S3Error::Service`] the
/// context is appended to the code header, so the user sees
/// `code — bucket 'x' @ endpoint` before the SDK detail line; for a plain
/// [`S3Error::Aws`] the context prefixes the detail. Untyped variants
/// (NotFound/ReadOnly/Io) carry no operation target and pass through.
fn s3error_ctx(err: S3Error, ctx: &str) -> S3Error {
    match err {
        S3Error::Service { mut code, detail } => {
            code.push_str(&format!(" — {ctx}"));
            S3Error::Service { code, detail }
        }
        S3Error::Aws(detail) => S3Error::Aws(format!("{ctx}: {detail}")),
        other => other,
    }
}

/// Errors specific to S3 backend operations.
#[derive(Debug, thiserror::Error)]
pub enum S3Error {
    #[error("s3: not found: {0}")]
    NotFound(String),
    #[error("s3: bucket operation failed: {0}")]
    BucketError(String),
    #[error("s3: read-only mount — writes rejected")]
    ReadOnly,
    #[error("s3: io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("s3: aws error: {0}")]
    Aws(String),
    /// Typed service failure carrying the extracted error code / HTTP status
    /// FIRST, then bucket + endpoint context, then the raw SDK Display string
    /// as the detail line (DF-WARPFS-24). Display shape:
    /// `s3: <code> — bucket 'b' @ <endpoint>: <sdk detail>`.
    #[error("s3: {code}: {detail}")]
    Service { code: String, detail: String },
}

impl From<s3::Error> for S3Error {
    fn from(e: s3::Error) -> Self {
        // s3::Error (error_meta::Error) implements ProvideErrorMetadata
        // directly; pull the wire code when one survived (DF-WARPFS-24).
        let detail = e.to_string();
        let code = s3::error::ProvideErrorMetadata::code(&e).map(|c| c.to_string());
        aws_failure_to_s3_error(code.as_deref(), None, detail)
    }
}

// R is pinned to the SDK default (HttpResponse) so the raw HTTP status is
// reachable — s3::error::SdkError<E> IS SdkError<E, HttpResponse> via the
// crate's type-alias default, which is what every `.send()` call site yields.
impl<E> From<s3::error::SdkError<E>> for S3Error
where
    E: std::fmt::Display + s3::error::ProvideErrorMetadata,
{
    fn from(e: s3::error::SdkError<E>) -> Self {
        let detail = e.to_string();
        let status = e.raw_response().map(|r| r.status().as_u16());
        let code = e.as_service_error().and_then(|se| se.code());
        aws_failure_to_s3_error(code, status, detail)
    }
}

/// Result type for S3 backend operations.
pub type S3Result<T> = Result<T, S3Error>;

/// Cached file metadata stored alongside the cached content.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CacheMeta {
    pub s3_key: String,
    pub etag: Option<String>,
    pub content_type: Option<String>,
    pub content_length: i64,
    pub cached_at: u64, // UNIX epoch seconds
}

/// Result of a write-through put_object operation.
#[derive(Debug, Clone)]
pub struct WriteResult {
    /// Local cache path where the file is stored.
    pub cache_path: PathBuf,
    /// Hex-encoded SHA-256 hash with "sha256:" prefix.
    pub sha256: String,
    /// S3 ETag from the PutObject response (if available).
    pub etag: Option<String>,
}

/// Metadata for one object returned by [`S3Client::list_objects_with_meta`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct S3ObjectMeta {
    pub key: String,
    pub size: i64,
    /// LastModified as UNIX epoch seconds (0 when unknown).
    pub last_modified_unix: u64,
}

/// Which endpoint an [`S3Client`] was constructed against, resolved once at
/// construction time and carried on the client (DF-WARPFS-12).
///
/// The CLI prints this on every sync plan header so "which store am I
/// writing to" is never a guess: with `AWS_ENDPOINT_URL` unset the AWS
/// config chain silently resolves the operator's ambient (possibly
/// production) endpoint — the old client gave no way to tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S3Endpoint {
    /// An explicit endpoint URL (flag/config field, or `AWS_ENDPOINT_URL`).
    Url(String),
    /// No endpoint anywhere: the client uses the ambient AWS config chain.
    DefaultChain,
}

impl S3Endpoint {
    /// The exact string the CLI prints in plan headers.
    pub fn display(&self) -> &str {
        match self {
            S3Endpoint::Url(u) => u.as_str(),
            S3Endpoint::DefaultChain => "default AWS config chain",
        }
    }

    /// Resolve from the environment: `AWS_ENDPOINT_URL` (non-empty) wins,
    /// anything else falls back to the default AWS config chain.
    pub fn from_env() -> Self {
        match std::env::var("AWS_ENDPOINT_URL") {
            Ok(ep) if !ep.is_empty() => S3Endpoint::Url(ep),
            _ => S3Endpoint::DefaultChain,
        }
    }
}

/// Entry in .vfs/blobs/index.jsonl tracking an uploaded blob.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BlobEntry {
    pub path: String,
    pub hash: String,
    pub backend: String,
    pub uploaded_at: u64,
}

/// Human-readable list of the credential sources the SDK's default provider
/// chain consults, expanded with the concrete profile / files that are in
/// play (DF-WARPFS-65).
///
/// Pure so the wording is unit-testable without touching the process
/// environment; [`aws_credential_source_env`] resolves the values from the
/// same env overrides the SDK honours.
fn aws_credential_sources_checked(
    profile: &str,
    credentials_file: &str,
    config_file: &str,
) -> String {
    format!(
        "env vars (AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY), \
         shared credentials profile '{profile}' ({credentials_file}), \
         shared config profile '{profile}' ({config_file}), \
         web identity token (AWS_WEB_IDENTITY_TOKEN_FILE), \
         container credentials (AWS_CONTAINER_CREDENTIALS_RELATIVE_URI/FULL_URI), \
         EC2 instance metadata (IMDS)"
    )
}

/// Resolve the `(profile, credentials_file, config_file)` triple the SDK's
/// chain will actually consult, honouring the same environment overrides the
/// provider itself honours (`AWS_PROFILE` / `AWS_DEFAULT_PROFILE`,
/// `AWS_SHARED_CREDENTIALS_FILE`, `AWS_CONFIG_FILE`).
fn aws_credential_source_env() -> (String, String, String) {
    fn non_empty(key: &str) -> Option<String> {
        std::env::var(key)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    let profile = non_empty("AWS_PROFILE")
        .or_else(|| non_empty("AWS_DEFAULT_PROFILE"))
        .unwrap_or_else(|| "default".to_string());
    let credentials_file = non_empty("AWS_SHARED_CREDENTIALS_FILE")
        .unwrap_or_else(|| "~/.aws/credentials".to_string());
    let config_file = non_empty("AWS_CONFIG_FILE").unwrap_or_else(|| "~/.aws/config".to_string());
    (profile, credentials_file, config_file)
}

/// Build the DF-WARPFS-65 failure: name every credential source that was
/// consulted so the operator sees *why* the client refuses to build, instead
/// of the empty-credential request failing later as
/// `InvalidArgument (400): service error`.
fn missing_credentials_error(endpoint: &str, sources: &str, detail: Option<&str>) -> S3Error {
    let detail = detail.map(str::trim).filter(|d| !d.is_empty());
    match detail {
        Some(d) => S3Error::Aws(format!(
            "no AWS credentials found for endpoint {endpoint} (checked: {sources}) \
             — provider chain said: {d}"
        )),
        None => S3Error::Aws(format!(
            "no AWS credentials found for endpoint {endpoint} (checked: {sources})"
        )),
    }
}

/// A resolved credential pair is only usable when BOTH parts are non-blank.
/// A provider handing back blanks is exactly the defect this ticket fixes:
/// signing with empty strings makes every request fail as a 400, so blank
/// pairs are reported as "no credentials found" instead.
fn credentials_are_usable(access_key_id: &str, secret_access_key: &str) -> bool {
    !access_key_id.trim().is_empty() && !secret_access_key.trim().is_empty()
}

/// S3 read-only / write-through client with local cache.
pub struct S3Client {
    client: s3::Client,
    cache_dir: PathBuf,
    ttl_seconds: u32,
    pub writable: bool,
    /// The endpoint this client was constructed against, resolved once at
    /// construction (DF-WARPFS-12). Surfaced on plan headers so a sync run
    /// always names the store it is about to write.
    pub endpoint: S3Endpoint,
}

impl S3Client {
    /// Create a new S3 client for a specific bucket/prefix.
    /// cache_dir: local directory for cached files (e.g., .vfs/cache/)
    /// ttl_seconds: cache TTL; 0 means never expire
    ///
    /// If AWS_ENDPOINT_URL is set (S3-compatible endpoint such as MinIO),
    /// the client is configured explicitly for that endpoint with
    /// path-style addressing, which MinIO requires, and credentials taken
    /// from the full AWS credential chain (`AWS_PROFILE`, env vars, SSO /
    /// web identity, container credentials, instance metadata). Otherwise
    /// the ambient AWS config chain is used. The resolved endpoint is
    /// recorded on the client ([`S3Client::endpoint`]) for disclosure.
    pub async fn new(
        region: &str,
        cache_dir: &Path,
        ttl_seconds: u32,
        writable: bool,
    ) -> S3Result<Self> {
        Self::with_endpoint(
            &S3Endpoint::from_env(),
            region,
            cache_dir,
            ttl_seconds,
            writable,
        )
        .await
    }

    /// Create a client bound to an EXPLICIT endpoint decision
    /// (DF-WARPFS-12). `S3Endpoint::Url(u)` configures that endpoint with
    /// static credentials and path-style addressing (MinIO et al.),
    /// bypassing the environment; `S3Endpoint::DefaultChain` is the ambient
    /// AWS config chain. The explicit flag/config field beats
    /// `AWS_ENDPOINT_URL` — a destructive path should not depend on an
    /// env-only switch.
    pub async fn with_endpoint(
        endpoint: &S3Endpoint,
        region: &str,
        cache_dir: &Path,
        ttl_seconds: u32,
        writable: bool,
    ) -> S3Result<Self> {
        let client = match endpoint {
            S3Endpoint::Url(u) if !u.is_empty() => Self::endpoint_client(region, u).await?,
            _ => Self::default_client(region).await,
        };
        Ok(Self {
            client,
            cache_dir: cache_dir.to_path_buf(),
            ttl_seconds,
            writable,
            endpoint: endpoint.clone(),
        })
    }

    /// Client built from the standard AWS config chain (real AWS).
    async fn default_client(region: &str) -> s3::Client {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(s3::config::Region::new(region.to_string()))
            .load()
            .await;
        s3::Client::new(&config)
    }

    /// Client for an explicit S3-compatible endpoint (MinIO et al.).
    /// Path-style addressing is required by MinIO.
    ///
    /// Credentials come from the FULL ambient AWS credential chain — env
    /// vars, the shared profile selected by `AWS_PROFILE`, SSO / web identity
    /// token, container credentials, instance metadata — resolved through
    /// `aws_config`'s loader (DF-WARPFS-65).
    ///
    /// The old implementation read ONLY `AWS_ACCESS_KEY_ID` /
    /// `AWS_SECRET_ACCESS_KEY` with `unwrap_or_default()`, so a profile-only
    /// setup silently produced a client with an empty key pair and the first
    /// request failed as `InvalidArgument (400): service error` (while
    /// `backend setup` reported "credentials: found", because it scans the
    /// shared credentials file). Two changes fix that:
    ///
    /// 1. the chain is resolved ONCE here, so missing credentials fail at
    ///    construction with a message naming every source that was checked,
    ///    instead of surfacing as an opaque service error later;
    /// 2. the resolved *provider* (not a credential snapshot) is handed to
    ///    the client, so rotating / expiring credentials refresh as usual.
    async fn endpoint_client(region: &str, endpoint: &str) -> S3Result<s3::Client> {
        let provider = Self::ambient_credentials_provider(region, endpoint).await?;

        let config = s3::Config::builder()
            .behavior_version(s3::config::BehaviorVersion::latest())
            .region(s3::config::Region::new(region.to_string()))
            .endpoint_url(endpoint)
            .credentials_provider(provider)
            .force_path_style(true)
            .build();
        Ok(s3::Client::from_conf(config))
    }

    /// Resolve the credential provider the explicit-endpoint client signs
    /// with, from the FULL ambient AWS chain (DF-WARPFS-65).
    ///
    /// Split out from [`S3Client::endpoint_client`] so the resolved identity is
    /// directly assertable in tests: "a client was constructed" cannot
    /// distinguish profile credentials from the empty pair the old env-only
    /// code produced, but "this provider yields the profile's access key" can.
    async fn ambient_credentials_provider(
        region: &str,
        endpoint: &str,
    ) -> S3Result<s3::config::SharedCredentialsProvider> {
        let base = aws_config::defaults(aws_config::BehaviorVersion::latest())
            // Region is honoured by the chain too: SSO / web-identity providers
            // build their STS endpoint from it.
            .region(s3::config::Region::new(region.to_string()))
            .load()
            .await;

        let (profile, credentials_file, config_file) = aws_credential_source_env();
        let sources = aws_credential_sources_checked(&profile, &credentials_file, &config_file);

        // The loader always installs a provider for the default chain; a
        // missing one means the chain could not be assembled at all.
        let provider = base
            .credentials_provider()
            .ok_or_else(|| missing_credentials_error(endpoint, &sources, None))?;

        // Resolve eagerly so "no credentials anywhere" is a clear error here
        // rather than a signing failure signed with blanks on the first call.
        let creds = provider
            .provide_credentials()
            .await
            .map_err(|e| missing_credentials_error(endpoint, &sources, Some(&e.to_string())))?;
        if !credentials_are_usable(creds.access_key_id(), creds.secret_access_key()) {
            return Err(missing_credentials_error(
                endpoint,
                &sources,
                Some("the chain resolved a blank access key / secret access key"),
            ));
        }

        Ok(provider)
    }

    /// Read an object from S3, caching it locally.
    /// Returns the local cache path on success.
    pub async fn get_object(&self, bucket: &str, key: &str) -> S3Result<PathBuf> {
        // 1. Check cache first
        let cache_path = self.cache_path(bucket, key);
        if self.is_cache_fresh(&cache_path) {
            return Ok(cache_path);
        }

        // 2. Fetch from S3
        let resp = self
            .client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                if format!("{}", e).contains("NoSuchKey") {
                    S3Error::NotFound(key.to_string())
                } else {
                    s3error_ctx(S3Error::from(e), &self.op_ctx(bucket))
                }
            })?;

        // 3. Collect bytes
        let body = resp
            .body
            .collect()
            .await
            .map_err(|e| s3error_ctx(S3Error::Aws(e.to_string()), &self.op_ctx(bucket)))?;
        let bytes = body.into_bytes();

        // 4. Ensure cache directory exists
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        // 5. Write to cache
        fs::write(&cache_path, &bytes).await?;

        // 6. Write cache metadata
        let meta = CacheMeta {
            s3_key: key.to_string(),
            etag: resp.e_tag.clone(),
            content_type: resp.content_type.clone(),
            content_length: resp.content_length.unwrap_or(0),
            cached_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        let meta_json = serde_json::to_string(&meta).unwrap();
        let meta_path = cache_path.with_extension("json");
        fs::write(&meta_path, meta_json).await?;

        Ok(cache_path)
    }

    /// List objects in S3 under the given prefix.
    pub async fn list_objects(&self, bucket: &str, prefix: &str) -> S3Result<Vec<String>> {
        let resp = self
            .client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .send()
            .await?;

        let keys: Vec<String> = resp
            .contents()
            .iter()
            .filter_map(|obj| obj.key().map(|k| k.to_string()))
            .collect();

        Ok(keys)
    }

    /// List objects in S3 under the given prefix with size + LastModified.
    pub async fn list_objects_with_meta(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> S3Result<Vec<S3ObjectMeta>> {
        let resp = self
            .client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .send()
            .await?;

        let mut out = Vec::new();
        for obj in resp.contents() {
            let Some(key) = obj.key() else {
                continue;
            };
            let last_modified_unix = obj.last_modified().map(|t| t.secs() as u64).unwrap_or(0);
            out.push(S3ObjectMeta {
                key: key.to_string(),
                size: obj.size().unwrap_or(0),
                last_modified_unix,
            });
        }
        Ok(out)
    }

    /// Download an object directly to an explicit destination path
    /// (no cache, no blob index — used by the sync engine, where the
    /// destination IS the workspace copy).
    pub async fn download_to(&self, bucket: &str, key: &str, dest: &Path) -> S3Result<()> {
        let resp = self
            .client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                if format!("{}", e).contains("NoSuchKey") {
                    S3Error::NotFound(key.to_string())
                } else {
                    s3error_ctx(S3Error::from(e), &self.op_ctx(bucket))
                }
            })?;

        let body = resp
            .body
            .collect()
            .await
            .map_err(|e| s3error_ctx(S3Error::Aws(e.to_string()), &self.op_ctx(bucket)))?;
        let bytes = body.into_bytes();
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(dest, &bytes).await?;
        Ok(())
    }

    /// Fetch an object's LastModified (UNIX epoch seconds) without
    /// downloading it. Returns None when the object does not exist.
    pub async fn head_object_last_modified(
        &self,
        bucket: &str,
        key: &str,
    ) -> S3Result<Option<u64>> {
        let resp = self
            .client
            .head_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await;
        match resp {
            Ok(r) => Ok(r.last_modified().map(|t| t.secs() as u64)),
            Err(e) if head_error_is_not_found(&e) => Ok(None),
            Err(e) => Err(s3error_ctx(S3Error::from(e), &self.op_ctx(bucket))),
        }
    }

    /// Upload bytes to S3 without cache/xattr/blob-index bookkeeping
    /// (used by the sync engine, where the source IS the workspace copy).
    /// Returns the ETag when available.
    pub async fn upload_bytes(
        &self,
        bucket: &str,
        key: &str,
        data: &[u8],
    ) -> S3Result<Option<String>> {
        if !self.writable {
            return Err(S3Error::ReadOnly);
        }
        let body = s3::primitives::ByteStream::from(data.to_vec());
        let resp = self
            .client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(body)
            .send()
            .await?;
        Ok(resp.e_tag)
    }

    /// Write an object to S3 with write-through semantics:
    /// 1. Write data to local cache
    /// 2. Compute SHA-256 hash
    /// 3. Upload to S3
    /// 4. Set xattrs on the cache file (user.vfs.backend, user.vfs.hash)
    /// 5. Append entry to .vfs/blobs/index.jsonl
    ///
    /// On upload failure, the local cache is preserved and the error is returned.
    /// Returns `S3Error::ReadOnly` if the client is not writable.
    pub async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        data: &[u8],
        blob_index_dir: &Path,
    ) -> S3Result<WriteResult> {
        if !self.writable {
            return Err(S3Error::ReadOnly);
        }

        // 1. Write to local cache
        let cache_path = self.cache_path(bucket, key);
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&cache_path, data).await?;

        // 2. Compute SHA-256 hash
        let mut hasher = Sha256::new();
        Digest::update(&mut hasher, data);
        let hash_hex = format!("{:x}", hasher.finalize());
        let hash = format!("sha256:{hash_hex}");

        // 3. Upload to S3 (if this fails, local cache is preserved)
        let body = s3::primitives::ByteStream::from(data.to_vec());
        let resp = self
            .client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(body)
            .send()
            .await?;

        // 4. Set xattrs on the cache file
        let _ = xattr::set(&cache_path, "user.vfs.backend", b"s3");
        let _ = xattr::set(&cache_path, "user.vfs.hash", hash.as_bytes());

        // 5. Append to blob index
        self.append_blob_index(blob_index_dir, key, &hash).await?;

        Ok(WriteResult {
            cache_path,
            sha256: hash,
            etag: resp.e_tag,
        })
    }

    /// Append a blob entry to .vfs/blobs/index.jsonl.
    async fn append_blob_index(
        &self,
        blob_index_dir: &Path,
        key: &str,
        hash: &str,
    ) -> S3Result<()> {
        let index_dir = blob_index_dir.join("blobs");
        fs::create_dir_all(&index_dir).await?;

        let index_path = index_dir.join("index.jsonl");
        let entry = BlobEntry {
            path: key.to_string(),
            hash: hash.to_string(),
            backend: "s3".to_string(),
            uploaded_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        let line = format!("{}\n", serde_json::to_string(&entry).unwrap());

        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&index_path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;

        Ok(())
    }

    /// Check if a cached file exists and is within TTL.
    fn is_cache_fresh(&self, cache_path: &Path) -> bool {
        if !cache_path.exists() {
            return false;
        }
        if self.ttl_seconds == 0 {
            return true; // TTL=0 means never expire
        }

        match cache_path.metadata() {
            Ok(meta) => match meta.modified() {
                Ok(mtime) => {
                    let elapsed = SystemTime::now().duration_since(mtime).unwrap_or_default();
                    elapsed.as_secs() < self.ttl_seconds as u64
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    /// Compute the local cache path for an S3 key.
    fn cache_path(&self, bucket: &str, key: &str) -> PathBuf {
        self.cache_dir
            .join(bucket)
            .join(key.trim_start_matches('/'))
    }

    /// Where-context for error disclosure (DF-WARPFS-24):
    /// `bucket 'x' @ <endpoint>` — the exact store this client is talking to.
    fn op_ctx(&self, bucket: &str) -> String {
        format!("bucket '{}' @ {}", bucket, self.endpoint.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // DF-WARPFS-10: a HEAD 404 must classify as "absent", never as a failure.
    // Each layer is asserted independently because they are exercised by
    // different endpoints: a status-only 404 (empty body, moto), a modeled
    // typed variant (real AWS), and a wire code (body present, code carried).
    #[test]
    fn head_failure_404_status_is_absent() {
        assert!(head_failure_means_absent(Some(404), false, None));
    }

    #[test]
    fn head_failure_typed_not_found_is_absent() {
        assert!(head_failure_means_absent(None, true, None));
    }

    #[test]
    fn head_failure_notfound_code_is_absent() {
        assert!(head_failure_means_absent(None, false, Some("NotFound")));
    }

    // Negative: a genuine error (500, untyped, no NotFound code) must stay an
    // error. The legacy Display-substring guard was over-broad; this pins the
    // boundary so a future "loosen it up" change cannot silently swallow
    // real failures as "absent".
    #[test]
    fn head_failure_real_error_is_not_absent() {
        assert!(!head_failure_means_absent(Some(500), false, None));
        assert!(!head_failure_means_absent(
            Some(403),
            false,
            Some("AccessDenied")
        ));
        assert!(!head_failure_means_absent(None, false, None));
        assert!(!head_failure_means_absent(
            Some(301),
            false,
            Some("PermanentRedirect")
        ));
    }

    // DF-WARPFS-12: the resolved endpoint is recorded on the client and the
    // explicit decision beats AWS_ENDPOINT_URL. One test holds the env lock
    // for the whole body — sibling tests in this binary also build clients.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn s3_endpoint_display_and_env_resolution() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // explicit URL wins over the env
        std::env::set_var("AWS_ENDPOINT_URL", "http://env-endpoint:9000");
        assert_eq!(
            S3Endpoint::from_env(),
            S3Endpoint::Url("http://env-endpoint:9000".into())
        );
        // empty env var = absent, falls to the default chain
        std::env::set_var("AWS_ENDPOINT_URL", "");
        assert_eq!(S3Endpoint::from_env(), S3Endpoint::DefaultChain);
        std::env::remove_var("AWS_ENDPOINT_URL");
        assert_eq!(S3Endpoint::from_env(), S3Endpoint::DefaultChain);

        // display strings are the exact plan-header disclosure strings
        assert_eq!(
            S3Endpoint::Url("http://minio:9000".into()).display(),
            "http://minio:9000"
        );
        assert_eq!(
            S3Endpoint::DefaultChain.display(),
            "default AWS config chain"
        );
    }

    // DF-WARPFS-65: env-var save/restore for the credential-chain tests. The
    // AWS SDK reads PROCESS-GLOBAL env, so every var that can supply
    // credentials is captured and restored on drop — a failed assertion must
    // not leak (or wipe) credentials for the sibling tests in this binary.
    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    /// Every env var the SDK's default chain can read a credential (or a
    /// credential pointer) from. Tests that assert "profile only" or "nothing
    /// at all" must clear ALL of them: one ambient value on the host turns the
    /// assertion into a tautology.
    const CREDENTIAL_ENV_KEYS: &[&str] = &[
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_DEFAULT_PROFILE",
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_CONFIG_FILE",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_ROLE_ARN",
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        "AWS_EC2_METADATA_DISABLED",
    ];

    impl EnvGuard {
        fn capture(keys: &[&'static str]) -> Self {
            Self {
                saved: keys.iter().map(|k| (*k, std::env::var(k).ok())).collect(),
            }
        }

        fn set(&self, key: &'static str, value: &str) {
            std::env::set_var(key, value);
        }

        fn unset(&self, key: &'static str) {
            std::env::remove_var(key);
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    // The client actually CONSTRUCTS for the explicit endpoint (path-style
    // client over the ambient credential chain — not the old env-only static
    // credentials) and records exactly what it was given; `new` records the
    // env resolution.
    // Holding the std-Mutex env lock across the awaits is the POINT: the env
    // must stay pinned for the whole construction body, and these tests run
    // on their own single-threaded runtime, so no other task can observe the
    // held guard.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn client_records_endpoint_it_was_constructed_with() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // DF-WARPFS-65: the explicit-endpoint client now resolves the ambient
        // credential chain and fails loudly when it finds nothing, so pin a
        // known credential pair instead of relying on the CI host's env.
        let _env = EnvGuard::capture(CREDENTIAL_ENV_KEYS);
        std::env::set_var("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE");
        std::env::set_var(
            "AWS_SECRET_ACCESS_KEY",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        );
        std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("AWS_ENDPOINT_URL", "http://env-endpoint:9000");
        let explicit = S3Client::with_endpoint(
            &S3Endpoint::Url("http://explicit:9000".into()),
            "us-east-1",
            &tmp.path().join("c1"),
            0,
            true,
        )
        .await
        .unwrap();
        assert_eq!(
            explicit.endpoint,
            S3Endpoint::Url("http://explicit:9000".into())
        );
        std::env::remove_var("AWS_ENDPOINT_URL");
        let env_client = S3Client::new("us-east-1", &tmp.path().join("c2"), 0, true)
            .await
            .unwrap();
        assert_eq!(env_client.endpoint, S3Endpoint::DefaultChain);
    }

    // DF-WARPFS-65 acceptance: `AWS_PROFILE` ALONE must produce a usable
    // client for an explicit endpoint. Every env-provided credential source is
    // cleared, so a client that still reads AWS_ACCESS_KEY_ID directly (the
    // old `unwrap_or_default()` behaviour) cannot pass this.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn endpoint_client_accepts_profile_only_credentials() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::capture(CREDENTIAL_ENV_KEYS);
        let tmp = tempfile::tempdir().unwrap();
        let creds_path = tmp.path().join("credentials");
        std::fs::write(
            &creds_path,
            // Sample keys from the AWS docs — never valid, never secret.
            "[acme]\naws_access_key_id = AKIAIOSFODNN7EXAMPLE\n\
             aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n",
        )
        .unwrap();
        let config_path = tmp.path().join("config");
        std::fs::write(&config_path, "[profile acme]\nregion = us-east-1\n").unwrap();

        for key in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
        ] {
            env.unset(key);
        }
        env.set("AWS_PROFILE", "acme");
        env.set(
            "AWS_SHARED_CREDENTIALS_FILE",
            creds_path.to_str().expect("utf-8 temp path"),
        );
        env.set(
            "AWS_CONFIG_FILE",
            config_path.to_str().expect("utf-8 temp path"),
        );
        env.set("AWS_EC2_METADATA_DISABLED", "true");

        // The provider the client signs with must carry the PROFILE's key, not
        // an empty string pair — `is_ok()` alone cannot tell those apart, and
        // the empty pair is exactly what produced
        // `InvalidArgument (400): service error` before DF-WARPFS-65.
        let provider = S3Client::ambient_credentials_provider("us-east-1", "http://127.0.0.1:9000")
            .await
            .expect("AWS_PROFILE-only credentials must resolve");
        let creds = provider
            .provide_credentials()
            .await
            .expect("resolved provider must yield credentials");
        assert_eq!(
            creds.access_key_id(),
            "AKIAIOSFODNN7EXAMPLE",
            "the shared profile's access key must reach the client"
        );
        assert!(credentials_are_usable(
            creds.access_key_id(),
            creds.secret_access_key()
        ));

        let client = S3Client::endpoint_client("us-east-1", "http://127.0.0.1:9000").await;
        assert!(
            client.is_ok(),
            "AWS_PROFILE-only credentials must build a client, got: {:?}",
            client.err().map(|e| e.to_string())
        );
    }

    // DF-WARPFS-65 acceptance: with no credentials ANYWHERE the failure names
    // the sources checked (and the profile it looked for), instead of
    // surfacing later as `InvalidArgument (400): service error`.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn endpoint_client_without_credentials_names_the_sources() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::capture(CREDENTIAL_ENV_KEYS);
        let tmp = tempfile::tempdir().unwrap();
        // Empty shared files: no profile can supply a key, and the IMDS /
        // container / web-identity sources are cleared below — so the chain
        // has literally nowhere left to look.
        let creds_path = tmp.path().join("credentials");
        std::fs::write(&creds_path, "").unwrap();
        let config_path = tmp.path().join("config");
        std::fs::write(&config_path, "").unwrap();

        for key in CREDENTIAL_ENV_KEYS.iter().copied() {
            env.unset(key);
        }
        env.set("AWS_PROFILE", "absent-lane");
        env.set(
            "AWS_SHARED_CREDENTIALS_FILE",
            creds_path.to_str().expect("utf-8 temp path"),
        );
        env.set(
            "AWS_CONFIG_FILE",
            config_path.to_str().expect("utf-8 temp path"),
        );
        env.set("AWS_EC2_METADATA_DISABLED", "true");

        let err = S3Client::endpoint_client("us-east-1", "http://127.0.0.1:9")
            .await
            .expect_err("no credentials must fail at construction, not at signing time");
        let msg = err.to_string();
        let creds_shown = creds_path.to_str().expect("utf-8 temp path");
        for needle in [
            "no AWS credentials found",
            "http://127.0.0.1:9",
            "env vars (AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY)",
            "profile 'absent-lane'",
            creds_shown,
            "container credentials",
            "instance metadata",
            // The SDK's own opaque wording is preserved as the detail line —
            // it is exactly what used to reach the user as
            // "InvalidArgument (400): service error" (DF-WARPFS-65).
            "provider chain said: the credential provider was not enabled",
        ] {
            assert!(msg.contains(needle), "missing {needle:?} in: {msg}");
        }
    }

    // DF-WARPFS-65 end-to-end: the request that actually leaves the client is
    // signed with the PROFILE's key. A loopback listener captures the request
    // head and answers a minimal ListObjectsV2 document, so this covers the
    // wiring the unit-level provider assertion cannot: endpoint_client must
    // install THAT provider on the client it returns. An empty key pair — the
    // pre-fix behaviour — shows up here as `Credential=/" or no auth at all.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn profile_credentials_sign_requests_end_to_end() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::capture(CREDENTIAL_ENV_KEYS);
        let tmp = tempfile::tempdir().unwrap();
        let creds_path = tmp.path().join("credentials");
        std::fs::write(
            &creds_path,
            "[acme]\naws_access_key_id = AKIAIOSFODNN7EXAMPLE\n\
             aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n",
        )
        .unwrap();
        let config_path = tmp.path().join("config");
        std::fs::write(&config_path, "[profile acme]\nregion = us-east-1\n").unwrap();
        for key in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
        ] {
            env.unset(key);
        }
        env.set("AWS_PROFILE", "acme");
        env.set(
            "AWS_SHARED_CREDENTIALS_FILE",
            creds_path.to_str().expect("utf-8 temp path"),
        );
        env.set(
            "AWS_CONFIG_FILE",
            config_path.to_str().expect("utf-8 temp path"),
        );
        env.set("AWS_EC2_METADATA_DISABLED", "true");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _peer) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut chunk = [0u8; 2048];
            // Read until the end of the request head; a single read can return
            // a partial line under load, and a truncated head would make the
            // Authorization assertion lie.
            loop {
                let n = sock.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                head.extend_from_slice(&chunk[..n]);
                if head.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let body = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                        <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                        <Name>b</Name><KeyCount>0</KeyCount><MaxKeys>1000</MaxKeys>\
                        <IsTruncated>false</IsTruncated></ListBucketResult>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/xml\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
            String::from_utf8_lossy(&head).to_string()
        });

        let client = S3Client::endpoint_client("us-east-1", &format!("http://{addr}"))
            .await
            .expect("profile credentials must build a client");
        // The response shape is not the point here; the captured request is.
        let _ = client.list_objects_v2().bucket("b").send().await;
        let head = server.await.unwrap();

        // hyper emits lower-case header names on the wire, so compare folded.
        let lower = head.to_lowercase();

        assert!(
            lower.contains("authorization: aws4-hmac-sha256"),
            "request must carry a SigV4 signature, got:\n{head}"
        );
        assert!(
            lower.contains("credential=akiaiosfodnn7example/"),
            "request must be signed with the PROFILE key, got:\n{head}"
        );
    }

    // DF-WARPFS-65: the failure text must be self-explanatory on its own —
    // every source listed, the concrete profile named, and the endpoint that
    // needed the credentials. No env access, so this pins the wording.
    #[test]
    fn missing_credentials_message_names_sources_and_profile() {
        let sources = aws_credential_sources_checked(
            "acme",
            "/home/u/.aws/credentials",
            "/home/u/.aws/config",
        );
        let msg = missing_credentials_error("http://minio:9000", &sources, None).to_string();
        for needle in [
            "no AWS credentials found",
            "http://minio:9000",
            "env vars (AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY)",
            "shared credentials profile 'acme' (/home/u/.aws/credentials)",
            "shared config profile 'acme' (/home/u/.aws/config)",
            "container credentials",
            "EC2 instance metadata (IMDS)",
        ] {
            assert!(msg.contains(needle), "missing {needle:?} in: {msg}");
        }
        // A detail-less call must not leave a dangling separator behind.
        assert!(
            !msg.contains("provider chain said"),
            "unexpected detail: {msg}"
        );

        let with_detail = missing_credentials_error(
            "http://minio:9000",
            &sources,
            Some("  the credential provider was not enabled  "),
        )
        .to_string();
        assert!(
            with_detail.contains("provider chain said: the credential provider was not enabled"),
            "detail must be carried (trimmed) verbatim: {with_detail}"
        );
    }

    // The blank-pair case is the original defect in miniature: signing with an
    // empty key/secret must never be accepted as "credentials found".
    #[test]
    fn blank_credential_pairs_are_not_usable() {
        assert!(credentials_are_usable("AKIAIOSFODNN7EXAMPLE", "secret"));
        assert!(!credentials_are_usable("", "secret"));
        assert!(!credentials_are_usable("AKIAIOSFODNN7EXAMPLE", ""));
        assert!(!credentials_are_usable("", ""));
        assert!(!credentials_are_usable("   ", "secret"));
        assert!(!credentials_are_usable("AKIAIOSFODNN7EXAMPLE", "\t\n"));
    }

    // DF-WARPFS-24: the SDK-error → S3Error mapping must preserve the typed
    // service code (or HTTP status) instead of flattening to an opaque
    // Display string, and attach the bucket/endpoint where-context.
    #[test]
    fn aws_error_code_header_matrix() {
        assert_eq!(
            aws_error_code_header(Some("NoSuchBucket"), Some(404)).as_deref(),
            Some("NoSuchBucket (404)")
        );
        assert_eq!(
            aws_error_code_header(Some("AccessDenied"), None).as_deref(),
            Some("AccessDenied")
        );
        // Status alone: empty-body endpoints (moto) carry no wire code.
        assert_eq!(
            aws_error_code_header(None, Some(404)).as_deref(),
            Some("HTTP 404")
        );
        // Neither survived = transport-level failure, no service meaning.
        assert_eq!(aws_error_code_header(None, None), None);
    }

    #[test]
    fn aws_failure_maps_to_typed_service_error() {
        // RED-side guard: code + detail survive separately; code FIRST.
        let err = aws_failure_to_s3_error(
            Some("NoSuchBucket"),
            Some(404),
            "service error, metadata: code=NoSuchBucket".into(),
        );
        match &err {
            S3Error::Service { code, detail } => {
                assert_eq!(code, "NoSuchBucket (404)");
                assert!(detail.contains("service error"), "detail kept: {detail}");
            }
            other => panic!("expected Service, got {other:?}"),
        }
        // No code/status at all → plain Aws (unchanged legacy shape).
        assert!(matches!(
            aws_failure_to_s3_error(None, None, "conn refused".into()),
            S3Error::Aws(_)
        ));
    }

    #[test]
    fn service_error_display_leads_with_code_then_context() {
        let err = s3error_ctx(
            aws_failure_to_s3_error(Some("NoSuchBucket"), Some(404), "the sdk detail".into()),
            "bucket 'nope' @ http://minio:9000",
        );
        let msg = err.to_string();
        assert!(
            msg.starts_with("s3: NoSuchBucket (404) — bucket 'nope' @ http://minio:9000:"),
            "user-facing shape, got: {msg}"
        );
        assert!(msg.ends_with("the sdk detail"), "sdk detail kept: {msg}");
    }

    #[test]
    fn plain_aws_error_gets_context_prefix() {
        let err = s3error_ctx(
            S3Error::Aws("service error".into()),
            "bucket 'b' @ default AWS config chain",
        );
        assert_eq!(
            err.to_string(),
            "s3: aws error: bucket 'b' @ default AWS config chain: service error"
        );
    }

    #[test]
    fn untyped_variants_pass_through_ctx() {
        let nf = s3error_ctx(S3Error::NotFound("k".into()), "bucket 'b' @ ep");
        assert_eq!(nf.to_string(), "s3: not found: k");
        let ro = s3error_ctx(S3Error::ReadOnly, "bucket 'b' @ ep");
        assert_eq!(ro.to_string(), "s3: read-only mount — writes rejected");
    }

    // The FULL user-facing chain for a sync failure (DF-WARPFS-24 acceptance
    // criterion 2): S3Error::Service → BackendError → SyncError must carry
    // code, bucket AND endpoint in what `hilo backend sync` prints.
    #[test]
    fn sync_failure_error_chain_names_code_bucket_and_endpoint() {
        let s3_err = s3error_ctx(
            aws_failure_to_s3_error(Some("NoSuchBucket"), Some(404), "service error".into()),
            "bucket 'ghost-bucket' @ http://127.0.0.1:9",
        );
        let backend_err: crate::backend::BackendError = s3_err.into();
        let sync_err: crate::planner::SyncError = backend_err.into();
        let msg = sync_err.to_string();
        for needle in ["NoSuchBucket (404)", "ghost-bucket", "http://127.0.0.1:9"] {
            assert!(msg.contains(needle), "missing {needle:?} in: {msg}");
        }
    }

    // Test: cache path computation
    fn cache_path(bucket: &str, key: &str) -> PathBuf {
        Path::new("/tmp/test-cache")
            .join(bucket)
            .join(key.trim_start_matches('/'))
    }

    #[test]
    fn test_cache_path_basic() {
        let p = cache_path("my-bucket", "foo/bar.txt");
        assert_eq!(p, Path::new("/tmp/test-cache/my-bucket/foo/bar.txt"));
    }

    #[test]
    fn test_cache_path_leading_slash() {
        let p = cache_path("my-bucket", "/foo/bar.txt");
        assert_eq!(p, Path::new("/tmp/test-cache/my-bucket/foo/bar.txt"));
    }

    // Test: CacheMeta serialization round-trip
    #[test]
    fn test_cache_meta_roundtrip() {
        let meta = CacheMeta {
            s3_key: "prod/models/checkpoint.pt".into(),
            etag: Some("\"abc123\"".into()),
            content_type: Some("application/octet-stream".into()),
            content_length: 524288000,
            cached_at: 1719000000,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: CacheMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.s3_key, meta.s3_key);
        assert_eq!(parsed.etag, meta.etag);
        assert_eq!(parsed.content_length, meta.content_length);
    }

    // Test: S3Error formatting
    #[test]
    fn test_s3_error_display() {
        assert_eq!(
            S3Error::ReadOnly.to_string(),
            "s3: read-only mount — writes rejected"
        );
        assert_eq!(
            S3Error::NotFound("key.txt".into()).to_string(),
            "s3: not found: key.txt"
        );
        assert_eq!(
            S3Error::BucketError("boom".into()).to_string(),
            "s3: bucket operation failed: boom"
        );
    }

    // Test: SHA-256 hash is deterministic
    #[test]
    fn test_sha256_deterministic() {
        let data = b"hello world";
        let mut hasher1 = Sha256::new();
        Digest::update(&mut hasher1, data);
        let hash1 = format!("{:x}", hasher1.finalize());

        let mut hasher2 = Sha256::new();
        Digest::update(&mut hasher2, data);
        let hash2 = format!("{:x}", hasher2.finalize());

        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64); // SHA-256 hex is 64 chars
        assert_ne!(hash1, "");

        // Verify known hash of "hello world"
        assert_eq!(
            hash1,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    // Test: SHA-256 of empty input
    #[test]
    fn test_sha256_empty() {
        let mut hasher = Sha256::new();
        Digest::update(&mut hasher, b"");
        let hash = format!("{:x}", hasher.finalize());
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    // Test: BlobEntry serialization round-trip
    #[test]
    fn test_blob_entry_roundtrip() {
        let entry = BlobEntry {
            path: "prod/models/checkpoint.pt".into(),
            hash: "sha256:abc123def456".into(),
            backend: "s3".into(),
            uploaded_at: 1719000000,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: BlobEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.path, entry.path);
        assert_eq!(parsed.hash, entry.hash);
        assert_eq!(parsed.backend, entry.backend);
        assert_eq!(parsed.uploaded_at, entry.uploaded_at);

        // Verify JSON shape matches AC
        assert!(json.contains("\"path\""));
        assert!(json.contains("\"hash\""));
        assert!(json.contains("\"backend\""));
        assert!(json.contains("\"uploaded_at\""));
    }

    // Test: put_object returns ReadOnly when writable=false
    #[tokio::test]
    async fn test_put_object_readonly_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let client = S3Client::new("us-east-1", &cache, 0, false).await.unwrap();
        let result = client
            .put_object("bucket", "key.txt", b"data", tmp.path())
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("read-only") || msg.contains("writes rejected"),
            "expected read-only error, got: {msg}"
        );
    }

    // Test: append_blob_index writes valid JSONL
    #[tokio::test]
    async fn test_append_blob_index_writes_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let index_dir = tmp.path().join("vfs");

        let client = S3Client::new("us-east-1", &cache, 0, true).await.unwrap();

        client
            .append_blob_index(&index_dir, "test/file.bin", "sha256:deadbeef")
            .await
            .unwrap();

        let index_path = index_dir.join("blobs/index.jsonl");
        assert!(index_path.exists(), "index.jsonl should exist");

        let contents = tokio::fs::read_to_string(&index_path).await.unwrap();
        // Parse the JSONL line and verify fields — robust against serde_json formatting variations
        let parsed: BlobEntry =
            serde_json::from_str(contents.trim()).expect("index.jsonl should be valid JSON");
        assert_eq!(parsed.path, "test/file.bin");
        assert_eq!(parsed.hash, "sha256:deadbeef");
        assert_eq!(parsed.backend, "s3");
        assert!(
            parsed.uploaded_at > 0,
            "uploaded_at should be a valid timestamp"
        );
    }

    // Test: append_blob_index appends (doesn't overwrite)
    #[tokio::test]
    async fn test_append_blob_index_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let index_dir = tmp.path().join("vfs");

        let client = S3Client::new("us-east-1", &cache, 0, true).await.unwrap();

        client
            .append_blob_index(&index_dir, "file1.txt", "sha256:aaa")
            .await
            .unwrap();
        client
            .append_blob_index(&index_dir, "file2.txt", "sha256:bbb")
            .await
            .unwrap();

        let index_path = index_dir.join("blobs/index.jsonl");
        let contents = tokio::fs::read_to_string(&index_path).await.unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "should have 2 lines");
        // Parse each line as JSON for robust verification
        let entry1: BlobEntry =
            serde_json::from_str(lines[0]).expect("line 1 should be valid JSON");
        let entry2: BlobEntry =
            serde_json::from_str(lines[1]).expect("line 2 should be valid JSON");
        assert_eq!(entry1.path, "file1.txt");
        assert_eq!(entry1.hash, "sha256:aaa");
        assert_eq!(entry2.path, "file2.txt");
        assert_eq!(entry2.hash, "sha256:bbb");
    }

    // Test: WriteResult fields are accessible
    #[test]
    fn test_write_result_fields() {
        let result = WriteResult {
            cache_path: PathBuf::from("/tmp/cache/bucket/key.txt"),
            sha256: "sha256:abc123".into(),
            etag: Some("\"etag-value\"".into()),
        };
        assert_eq!(result.cache_path, Path::new("/tmp/cache/bucket/key.txt"));
        assert_eq!(result.sha256, "sha256:abc123");
        assert_eq!(result.etag, Some("\"etag-value\"".into()));
    }
}

/// Additional S3Client surface used by S3Driver (spec §6): raw delete and a
/// HEAD that also returns the object size.
impl S3Client {
    /// Delete an object. Missing key → `S3Error::NotFound`.
    pub async fn delete_object(&self, bucket: &str, key: &str) -> S3Result<()> {
        self.client
            .delete_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                if format!("{}", e).contains("NotFound") {
                    S3Error::NotFound(key.to_string())
                } else {
                    s3error_ctx(S3Error::from(e), &self.op_ctx(bucket))
                }
            })?;
        Ok(())
    }

    /// HEAD object returning (size, last_modified_unix). `None` when missing.
    ///
    /// A 404 on HEAD means "the remote counterpart does not exist yet" — that
    /// is a normal state for a first sync, not a failure. The check uses the
    /// typed error predicate (`HeadObjectError::is_not_found`) instead of
    /// string-matching the error Display: several S3-compatible endpoints
    /// (moto, some MinIO versions) render a HEAD 404 whose Display does NOT
    /// spell "NotFound", which used to abort the whole sync (DF-WARPFS-10).
    pub async fn head_object_meta(&self, bucket: &str, key: &str) -> S3Result<Option<(i64, u64)>> {
        let resp = self
            .client
            .head_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await;
        match resp {
            Ok(r) => Ok(Some((
                r.content_length().unwrap_or(0),
                r.last_modified().map(|t| t.secs() as u64).unwrap_or(0),
            ))),
            Err(e) if head_error_is_not_found(&e) => Ok(None),
            Err(e) => Err(s3error_ctx(S3Error::from(e), &self.op_ctx(bucket))),
        }
    }
}

use crate::backend::{BackendEntry, BackendError};

/// S3Driver — [`Backend`](crate::backend::Backend) adapter over [`S3Client`]
/// (spec §6). ListObjectsV2 (list/walk), GetObject→dest (get), PutObject
/// (put, reuses WriteResult), DeleteObject (delete), HEAD (stat). Multipart is
/// handled by aws_sdk_s3; no custom chunking.
///
/// The trait is synchronous, so the driver owns a current-thread tokio runtime
/// and block_on's each call (same pattern as the CLI's sync engine wrapper).
pub struct S3Driver {
    runtime: Option<tokio::runtime::Runtime>,
    client: S3Client,
    bucket: String,
    /// Key prefix; all trait keys are relative to it.
    prefix: String,
    mode: crate::backend::SyncMode,
    /// The endpoint this driver's client was constructed against
    /// (DF-WARPFS-12 disclosure string for plan headers).
    endpoint_display: String,
}

impl std::fmt::Debug for S3Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Driver")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl Drop for S3Driver {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };

        if tokio::runtime::Handle::try_current().is_err() {
            drop(runtime);
            return;
        }

        // Tokio forbids the normal blocking shutdown when this destructor runs
        // on a runtime thread. Do the blocking drop on a plain OS thread and
        // join it so the runtime is fully destroyed before the driver is gone.
        // Keeping the runtime behind a mutex also lets the spawn-failure path
        // reclaim it for Tokio's non-blocking shutdown instead of leaking it.
        let runtime = std::sync::Arc::new(std::sync::Mutex::new(Some(runtime)));
        let thread_runtime = std::sync::Arc::clone(&runtime);
        match std::thread::Builder::new()
            .name("hilo-s3-driver-runtime-drop".into())
            .spawn(move || {
                let runtime = thread_runtime
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                drop(runtime);
            }) {
            Ok(handle) => {
                let _ = handle.join();
            }
            Err(_) => {
                if let Some(runtime) = runtime
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    runtime.shutdown_background();
                }
            }
        }
    }
}

impl S3Driver {
    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime
            .as_ref()
            .expect("S3Driver runtime must remain available while the driver is in use")
    }
}

fn build_runtime_and_client(
    endpoint: S3Endpoint,
    region: String,
    cache_dir: PathBuf,
) -> Result<(tokio::runtime::Runtime, S3Client), crate::backend::BackendError> {
    let build = move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| crate::backend::BackendError::BucketError(e.to_string()))?;
        let client = runtime.block_on(S3Client::with_endpoint(
            &endpoint, &region, &cache_dir, 0, true,
        ))?;
        Ok((runtime, client))
    };

    // `Runtime::block_on` panics when called from any Tokio runtime thread.
    // Build on a plain OS thread in that case; the returned runtime is moved
    // back to the synchronous driver and remains the owner of Backend calls.
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::Builder::new()
            .name("hilo-s3-driver-init".into())
            .spawn(build)
            .map_err(|e| {
                crate::backend::BackendError::BucketError(format!(
                    "failed to spawn S3 client initialization thread: {e}"
                ))
            })?
            .join()
            .map_err(|_| {
                crate::backend::BackendError::BucketError(
                    "S3 client initialization thread panicked".into(),
                )
            })?
    } else {
        build()
    }
}

impl S3Driver {
    pub fn new(cfg: &crate::backend::BackendConfig) -> Result<Self, crate::backend::BackendError> {
        let bucket = cfg.bucket.clone().ok_or_else(|| {
            crate::backend::BackendError::InvalidConfig("S3Driver needs `bucket`".into())
        })?;
        let region = cfg.region.clone().unwrap_or_else(|| "us-east-1".into());
        let prefix = cfg.prefix.clone().unwrap_or_default();
        let cache_dir = std::env::temp_dir().join(format!("hilo-s3driver-{}", cfg.name));
        // DF-WARPFS-12: an explicit config endpoint beats AWS_ENDPOINT_URL;
        // without one, resolve from the environment and DISCLOSE what was
        // resolved (default-chain = the operator's ambient AWS config).
        let endpoint = match cfg.endpoint.as_deref() {
            Some(ep) if !ep.is_empty() => S3Endpoint::Url(ep.to_string()),
            _ => S3Endpoint::from_env(),
        };
        let endpoint_display = endpoint.display().to_string();
        let (runtime, client) = build_runtime_and_client(endpoint, region, cache_dir)?;
        Ok(Self {
            runtime: Some(runtime),
            client,
            bucket,
            prefix,
            mode: cfg.mode,
            endpoint_display,
        })
    }

    /// The resolved endpoint this driver writes to (plan-header disclosure).
    pub fn endpoint(&self) -> &str {
        &self.endpoint_display
    }

    fn full_key(&self, key: &str) -> String {
        join_prefix(&self.prefix, key)
    }

    /// Strip the driver prefix from a full S3 key → trait-relative key.
    fn relative_key(&self, full: &str) -> String {
        strip_prefix_key(full, &self.prefix)
    }
}

/// `base` + `/` + `sub`, tolerant of missing/duplicate slashes.
fn join_prefix(base: &str, sub: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        sub.to_string()
    } else if sub.is_empty() {
        base.to_string()
    } else {
        format!("{}/{}", base, sub.trim_matches('/'))
    }
}

/// Remove `base/` from the front of `full`; returns `full` unchanged when the
/// prefix does not match (defensive — S3 always returns keys under the prefix).
fn strip_prefix_key(full: &str, base: &str) -> String {
    if base.is_empty() {
        return full.to_string();
    }
    let prefix = format!("{}/", base.trim_end_matches('/'));
    full.strip_prefix(&prefix).unwrap_or(full).to_string()
}

impl crate::backend::Backend for S3Driver {
    fn kind(&self) -> crate::backend::BackendKind {
        crate::backend::BackendKind::S3
    }

    fn name(&self) -> &str {
        "s3"
    }

    fn endpoint(&self) -> String {
        self.endpoint_display.clone()
    }

    fn list(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
        let full = self.full_key(prefix);
        let metas = self
            .runtime()
            .block_on(self.client.list_objects_with_meta(&self.bucket, &full))?;
        Ok(metas
            .into_iter()
            .map(|m| BackendEntry {
                key: self.relative_key(&m.key),
                size: m.size,
                modified: Some(m.last_modified_unix as i64),
                etag: None,
                is_dir: false,
            })
            .collect())
    }

    fn stat(&self, key: &str) -> Result<BackendEntry, BackendError> {
        let full = self.full_key(key);
        match self
            .runtime()
            .block_on(self.client.head_object_meta(&self.bucket, &full))?
        {
            Some((size, modified)) => Ok(BackendEntry {
                key: key.to_string(),
                size,
                modified: Some(modified as i64),
                etag: None,
                is_dir: false,
            }),
            None => Err(BackendError::NotFound(key.to_string())),
        }
    }

    fn get(&self, key: &str, dest: &std::path::Path) -> Result<(), BackendError> {
        let full = self.full_key(key);
        self.runtime()
            .block_on(self.client.download_to(&self.bucket, &full, dest))?;
        Ok(())
    }

    fn put(&self, local: &std::path::Path, key: &str) -> Result<WriteResult, BackendError> {
        use sha2::{Digest, Sha256};
        let full = self.full_key(key);
        let bytes = std::fs::read(local)?;
        let etag =
            self.runtime()
                .block_on(self.client.upload_bytes(&self.bucket, &full, &bytes))?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let sha256 = format!(
            "sha256:{}",
            hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        Ok(WriteResult {
            cache_path: local.to_path_buf(),
            sha256,
            etag,
        })
    }

    fn delete(&self, key: &str) -> Result<(), BackendError> {
        let full = self.full_key(key);
        self.runtime()
            .block_on(self.client.delete_object(&self.bucket, &full))?;
        Ok(())
    }

    fn walk(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
        // S3 listing is already recursive over the prefix — walk == list.
        self.list(prefix)
    }
}

#[cfg(test)]
mod s3driver_tests {
    use super::*;
    use crate::backend::Backend;

    #[test]
    fn join_and_strip_prefix_roundtrip() {
        for (base, sub) in [
            ("workspace", "a/b.txt"),
            ("workspace/", "a/b.txt"),
            ("", "a/b.txt"),
            ("workspace", ""),
            ("workspace/", ""),
        ] {
            let joined = join_prefix(base, sub);
            let stripped = strip_prefix_key(&joined, base);
            let expect_sub = sub.trim_matches('/');
            let expect = if base.is_empty() {
                expect_sub.to_string()
            } else if sub.is_empty() {
                base.trim_end_matches('/').to_string()
            } else {
                format!("{}/{}", base.trim_end_matches('/'), expect_sub)
            };
            assert_eq!(joined, expect, "join {base:?} + {sub:?}");
            let expect_stripped = if sub.is_empty() && !base.is_empty() {
                // stripping "workspace" from "workspace" → no trailing slash match
                "workspace".to_string()
            } else {
                expect_sub.to_string()
            };
            assert_eq!(stripped, expect_stripped, "strip {base:?} from {joined:?}");
        }
    }

    #[test]
    fn strip_prefix_leaves_unmatched_full_key_unchanged() {
        assert_eq!(strip_prefix_key("other/x.txt", "workspace"), "other/x.txt");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn s3driver_new_does_not_enter_nested_runtime() {
        let cfg = crate::backend::BackendConfig {
            kind: crate::backend::BackendKind::S3,
            name: "runtime-context-test".into(),
            bucket: Some("test-bucket".into()),
            endpoint: Some("http://127.0.0.1:1".into()),
            ..Default::default()
        };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| S3Driver::new(&cfg)));
        assert!(
            result.is_ok(),
            "S3Driver::new must not panic inside a Tokio runtime"
        );
        let driver = result
            .unwrap()
            .expect("explicit endpoint client construction should not require network access");
        assert_eq!(driver.endpoint(), "http://127.0.0.1:1");

        let drop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(driver)));
        assert!(
            drop_result.is_ok(),
            "S3Driver::drop must be safe inside a Tokio runtime"
        );
    }

    /// Live round-trip, gated on AWS env (spec §15). Skips when
    /// AWS_ACCESS_KEY_ID or AWS_TEST_BUCKET are absent.
    #[test]
    fn s3driver_live_roundtrip_gated_on_aws_env() {
        if std::env::var("AWS_ACCESS_KEY_ID").is_err() {
            eprintln!("skipping s3driver_live_roundtrip: AWS_ACCESS_KEY_ID not set");
            return;
        }
        let Ok(bucket) = std::env::var("AWS_TEST_BUCKET") else {
            eprintln!("skipping s3driver_live_roundtrip: AWS_TEST_BUCKET not set");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let cfg = crate::backend::BackendConfig {
            kind: crate::backend::BackendKind::S3,
            name: "live-test".into(),
            bucket: Some(bucket),
            prefix: Some(format!("warpfs-s3driver-test-{}", std::process::id())),
            region: Some("us-east-1".into()),
            ..Default::default()
        };
        let driver = S3Driver::new(&cfg).expect("driver builds from env chain");
        let key = "roundtrip.txt";
        let src = tmp.path().join("src.txt");
        std::fs::write(&src, b"live").unwrap();
        driver.put(&src, key).unwrap();
        let st = driver.stat(key).unwrap();
        assert_eq!(st.size, 4);
        let dst = tmp.path().join("dst.txt");
        driver.get(key, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"live");
        let listed = driver.list("").unwrap();
        assert!(listed.iter().any(|e| e.key == key), "listed: {listed:?}");
        driver.delete(key).unwrap();
        assert!(matches!(driver.stat(key), Err(BackendError::NotFound(_))));
    }
}
