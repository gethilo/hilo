// S3 read-only + write-through backend implementation
use aws_config;
use aws_sdk_s3 as s3;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;

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
}

impl From<s3::Error> for S3Error {
    fn from(e: s3::Error) -> Self {
        S3Error::Aws(e.to_string())
    }
}

impl<E, R> From<s3::error::SdkError<E, R>> for S3Error
where
    E: std::fmt::Display,
    R: std::fmt::Debug,
{
    fn from(e: s3::error::SdkError<E, R>) -> Self {
        S3Error::Aws(e.to_string())
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

/// Entry in .vfs/blobs/index.jsonl tracking an uploaded blob.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BlobEntry {
    pub path: String,
    pub hash: String,
    pub backend: String,
    pub uploaded_at: u64,
}

/// S3 read-only / write-through client with local cache.
pub struct S3Client {
    client: s3::Client,
    cache_dir: PathBuf,
    ttl_seconds: u32,
    pub writable: bool,
}

impl S3Client {
    /// Create a new S3 client for a specific bucket/prefix.
    /// cache_dir: local directory for cached files (e.g., .vfs/cache/)
    /// ttl_seconds: cache TTL; 0 means never expire
    pub async fn new(
        region: &str,
        cache_dir: &Path,
        ttl_seconds: u32,
        writable: bool,
    ) -> S3Result<Self> {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(s3::config::Region::new(region.to_string()))
            .load()
            .await;
        let client = s3::Client::new(&config);
        Ok(Self {
            client,
            cache_dir: cache_dir.to_path_buf(),
            ttl_seconds,
            writable,
        })
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
                    S3Error::from(e)
                }
            })?;

        // 3. Collect bytes
        let body = resp
            .body
            .collect()
            .await
            .map_err(|e| S3Error::Aws(e.to_string()))?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
        assert!(contents.contains("\"path\":\"test/file.bin\""));
        assert!(contents.contains("\"hash\":\"sha256:deadbeef\""));
        assert!(contents.contains("\"backend\":\"s3\""));
        assert!(contents.contains("\"uploaded_at\""));
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
        assert!(lines[0].contains("file1.txt"));
        assert!(lines[1].contains("file2.txt"));
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
