# hilo-backends — Storage Backends

Virtual storage backends for Git, S3, and local paths. Handles caching, write-through, and auto-pull. All backends implement a common resolution interface.

**Crate:** `hilo-backends`  
**Public modules:** 3

## Public API Surface

### Types

| Type | Description |
|------|-------------|
| `Backend` | Enum: `S3`, `Git`, `Remote`, `Local` — resolved backend type |
| `BackendInfo` | Resolution result — `{backend, real_path, cached, cache_path, sync_status}` |
| `S3Client` | S3 read-only / write-through client with local cache |
| `S3Error` | S3 errors: `NotFound`, `BucketError`, `ReadOnly`, `Io`, `Aws` |
| `S3Result<T>` | Result alias for S3 operations |
| `WriteResult` | Write-through result — `{cache_path, sha256, etag}` |
| `GitBackend` | Git repository backend with worktree support |
| `GitBackendConfig` | Git backend config — `{url, ref_name, worktree, writable}` |
| `GitError` | Git backend errors |
| `GitResult<T>` | Result alias for Git operations |

### S3 Backend

```rust
pub struct S3Client {
    pub writable: bool,
}

impl S3Client {
    pub async fn new(bucket: &str, region: &str, cache_dir: impl Into<PathBuf>, ttl: u32) -> Self;

    // Read from S3 (cached locally)
    pub async fn read(&self, key: &str) -> S3Result<Vec<u8>>;
    pub async fn read_to_file(&self, key: &str, dest: impl AsRef<Path>) -> S3Result<()>;

    // Write through to S3 (cache → upload → blob index)
    pub async fn write(&self, key: &str, data: &[u8]) -> S3Result<WriteResult>;
    pub async fn write_file(&self, key: &str, src: impl AsRef<Path>) -> S3Result<WriteResult>;

    // Blob index management
    pub async fn append_blob_index(...) -> S3Result<()>;
    pub async fn read_blob_index(...) -> Vec<BlobEntry>;
}
```

### Git Backend

```rust
pub struct GitBackend;

impl GitBackend {
    pub async fn open(config: &GitBackendConfig) -> GitResult<Self>;
    pub async fn checkout(&self) -> GitResult<PathBuf>;
    pub async fn pull(&self) -> GitResult<()>;
    pub async fn read(&self, path: impl AsRef<Path>) -> GitResult<Vec<u8>>;
    pub async fn write(&self, path: impl AsRef<Path>, data: &[u8]) -> GitResult<()>;
    pub fn worktree(&self) -> &Path;
}
```

## Usage Example

```rust
use hilo_backends::{S3Client, WriteResult};

// S3 read-only
let s3 = S3Client::new("my-bucket", "us-east-1", "/tmp/hilo-cache", 3600).await?;
let data = s3.read("path/to/file.txt").await?;

// S3 write-through
let result: WriteResult = s3.write("uploads/report.json", json_data).await?;
println!("Uploaded: {} (SHA-256: {})", result.cache_path.display(), result.sha256);
```
