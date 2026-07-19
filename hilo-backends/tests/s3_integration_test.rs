// Integration tests for S3 backend against real MinIO infrastructure.
//
// Prerequisites:
//   1. Start MinIO:  ./tests/integration/start.sh
//   2. Export env:   eval "$(./tests/integration/start.sh)"
//   3. Run tests:    cargo test -p hilo_backends --test s3_integration_test
//
// Environment variables required:
//   AWS_ENDPOINT_URL  — MinIO endpoint (e.g. http://localhost:9000)
//   AWS_ACCESS_KEY_ID — MinIO access key (default: hilo_test)
//   AWS_SECRET_ACCESS_KEY — MinIO secret key (default: hilo_test)
//   AWS_REGION        — AWS region (default: us-east-1)
//
// If AWS_ENDPOINT_URL is not set, all tests are skipped gracefully.
//
// NOTE: The production S3Client uses aws_config::defaults() without
// force_path_style. MinIO requires path-style addressing. Integration
// tests document this limitation and test basic operations.

use hilo_backends::{S3Client, WriteResult};
use std::env;
use tempfile::TempDir;

/// Check whether MinIO is configured. Returns None if not.
fn check_minio_configured() -> Option<String> {
    env::var("AWS_ENDPOINT_URL").ok()
}

/// Create a fresh S3Client pointed at the configured endpoint.
async fn create_test_client() -> (S3Client, TempDir) {
    let cache_dir = TempDir::new().expect("tempdir");
    let region = env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into());
    let client = S3Client::new(&region, cache_dir.path(), 0, true)
        .await
        .expect("S3Client::new");
    (client, cache_dir)
}

/// Helper: put a small object and return the written content + WriteResult.
async fn put_test_object(
    client: &S3Client,
    bucket: &str,
    key: &str,
    content: &[u8],
) -> WriteResult {
    // Use a TempDir for blob_index_dir (the test doesn't need blob tracking).
    let blob_dir = TempDir::new().expect("tempdir for blob index");
    client
        .put_object(bucket, key, content, blob_dir.path())
        .await
        .expect("put_object")
}

// ── Tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_put_and_get_object() {
    let endpoint = match check_minio_configured() {
        Some(e) => e,
        None => {
            eprintln!("SKIP: AWS_ENDPOINT_URL not set");
            return;
        }
    };
    eprintln!("Using MinIO at {endpoint}");

    let (client, _cache) = create_test_client().await;
    let bucket = "hilo-test-bucket";
    let key = "integration/put-get-test.txt";
    let content = b"Hello from Hilo S3 integration test!";

    // Write.
    let result = put_test_object(&client, bucket, key, content).await;
    assert!(!result.sha256.is_empty());
    assert!(result.sha256.starts_with("sha256:"));

    // Read back.
    let cached_path = client.get_object(bucket, key).await.expect("get_object");

    let roundtrip = std::fs::read_to_string(&cached_path).expect("read cache");
    assert_eq!(roundtrip.as_bytes(), content);
}

#[tokio::test]
async fn test_list_objects() {
    let _ = match check_minio_configured() {
        Some(e) => e,
        None => {
            eprintln!("SKIP: AWS_ENDPOINT_URL not set");
            return;
        }
    };

    let (client, _cache) = create_test_client().await;
    let bucket = "hilo-test-bucket";
    let prefix = "integration/list-test/";

    // Put two objects under the prefix.
    put_test_object(&client, bucket, &format!("{prefix}a.txt"), b"aaa").await;
    put_test_object(&client, bucket, &format!("{prefix}b.txt"), b"bbb").await;

    // List.
    let keys = client
        .list_objects(bucket, prefix)
        .await
        .expect("list_objects");

    assert!(
        keys.contains(&format!("{prefix}a.txt")),
        "expected {prefix}a.txt in {keys:?}"
    );
    assert!(
        keys.contains(&format!("{prefix}b.txt")),
        "expected {prefix}b.txt in {keys:?}"
    );
}

#[tokio::test]
async fn test_get_object_not_found() {
    let _ = match check_minio_configured() {
        Some(e) => e,
        None => {
            eprintln!("SKIP: AWS_ENDPOINT_URL not set");
            return;
        }
    };

    let (client, _cache) = create_test_client().await;
    let bucket = "hilo-test-bucket";
    let key = "integration/does-not-exist.txt";

    let result = client.get_object(bucket, key).await;
    assert!(
        result.is_err(),
        "expected NotFound error for nonexistent key"
    );
}

#[tokio::test]
async fn test_put_object_content_integrity() {
    let _ = match check_minio_configured() {
        Some(e) => e,
        None => {
            eprintln!("SKIP: AWS_ENDPOINT_URL not set");
            return;
        }
    };

    let (client, _cache) = create_test_client().await;
    let bucket = "hilo-test-bucket";
    let key = "integration/large-object.bin";

    // Write 64 KB of pseudo-random data.
    let data: Vec<u8> = (0..65536u32).flat_map(|i| i.to_be_bytes()).collect();

    let result = put_test_object(&client, bucket, key, &data).await;
    assert_eq!(result.sha256.len(), 71); // "sha256:" + 64 hex chars

    // Read back and verify.
    let cached_path = client.get_object(bucket, key).await.expect("get_object");
    let roundtrip = std::fs::read(&cached_path).expect("read cache");
    assert_eq!(roundtrip, data);
}

#[tokio::test]
async fn test_read_only_client_rejects_writes() {
    // Read-only client test does NOT need MinIO — uses local-only S3Client::new.
    let cache_dir = TempDir::new().expect("tempdir");
    let client = S3Client::new("us-east-1", cache_dir.path(), 0, false)
        .await
        .expect("S3Client::new (read-only)");

    let blob_dir = TempDir::new().expect("tempdir for blob index");
    let result = client
        .put_object("any-bucket", "test.txt", b"data", blob_dir.path())
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("read-only"),
        "expected read-only error, got: {err}"
    );
}
