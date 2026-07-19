// Integration tests for S3 backend against real MinIO infrastructure.
//
// Prerequisites:
//   1. Start MinIO:  ./tests/integration/start.sh
//   2. Export env:   eval "$(./tests/integration/start.sh)"
//   3. Run tests:    cargo test -p hilo_backends --test s3_integration_test
//
// Environment variables required:
//   AWS_ENDPOINT_URL      — MinIO endpoint (e.g. http://localhost:9000)
//   AWS_ACCESS_KEY_ID     — MinIO access key (default: hilo_test)
//   AWS_SECRET_ACCESS_KEY — MinIO secret key (default: hilo_test)
//   AWS_REGION            — AWS region (default: us-east-1)
//
// If AWS_ENDPOINT_URL is not set, all tests are skipped gracefully (they
// return early and pass), so `cargo test` never fails on machines without
// Docker/MinIO running.
//
// The production `S3Client` picks up the endpoint and credentials from the
// AWS_* environment via aws_config's default chain (aws-config >= 1.7
// honors AWS_ENDPOINT_URL). A raw `aws_sdk_s3::Client` with
// force_path_style(true) is used for head/delete operations that the
// backend does not expose.

use aws_sdk_s3 as s3;
use hilo_backends::{S3Client, WriteResult};
use std::env;
use tempfile::TempDir;

const BUCKET: &str = "hilo-test-bucket";

/// Check whether MinIO is configured. Returns None if not.
fn check_minio_configured() -> Option<String> {
    env::var("AWS_ENDPOINT_URL").ok().filter(|e| !e.is_empty())
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

/// Build a low-level aws_sdk_s3::Client against the MinIO endpoint with
/// static credentials and path-style addressing (required by MinIO).
async fn raw_s3_client() -> s3::Client {
    let endpoint = env::var("AWS_ENDPOINT_URL").expect("AWS_ENDPOINT_URL");
    let access_key = env::var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "hilo_test".into());
    let secret_key = env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_else(|_| "hilo_test".into());
    let region = env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into());

    let creds = s3::config::Credentials::new(access_key, secret_key, None, None, "static");
    let config = s3::Config::builder()
        .behavior_version(s3::config::BehaviorVersion::latest())
        .region(s3::config::Region::new(region))
        .endpoint_url(endpoint)
        .credentials_provider(creds)
        .force_path_style(true)
        .build();
    s3::Client::from_conf(config)
}

/// Helper: put a small object and return the WriteResult.
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

/// put_object → get_object round-trip; verifies content matches.
#[tokio::test]
async fn test_put_and_get_object() {
    let Some(endpoint) = check_minio_configured() else {
        eprintln!("SKIP: AWS_ENDPOINT_URL not set");
        return;
    };
    eprintln!("Using MinIO at {endpoint}");

    let (client, _cache) = create_test_client().await;
    let key = "integration/put-get-test.txt";
    let content = b"Hello from Hilo S3 integration test!";

    // Write.
    let result = put_test_object(&client, BUCKET, key, content).await;
    assert!(!result.sha256.is_empty());
    assert!(result.sha256.starts_with("sha256:"));

    // Drop the local cache so get_object is forced to fetch from MinIO.
    if result.cache_path.exists() {
        std::fs::remove_file(&result.cache_path).expect("remove cache file");
    }

    // Read back.
    let cached_path = client.get_object(BUCKET, key).await.expect("get_object");

    let roundtrip = std::fs::read(&cached_path).expect("read cache");
    assert_eq!(roundtrip, content);

    // Cleanup.
    let raw = raw_s3_client().await;
    raw.delete_object()
        .bucket(BUCKET)
        .key(key)
        .send()
        .await
        .expect("delete_object cleanup");
}

/// put two objects under a prefix → list_objects returns both.
#[tokio::test]
async fn test_list_objects() {
    if check_minio_configured().is_none() {
        eprintln!("SKIP: AWS_ENDPOINT_URL not set");
        return;
    }

    let (client, _cache) = create_test_client().await;
    let prefix = "integration/list-test/";

    put_test_object(&client, BUCKET, &format!("{prefix}a.txt"), b"aaa").await;
    put_test_object(&client, BUCKET, &format!("{prefix}b.txt"), b"bbb").await;

    let keys = client
        .list_objects(BUCKET, prefix)
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

    // Cleanup.
    let raw = raw_s3_client().await;
    for suffix in ["a.txt", "b.txt"] {
        raw.delete_object()
            .bucket(BUCKET)
            .key(format!("{prefix}{suffix}"))
            .send()
            .await
            .expect("delete_object cleanup");
    }
}

/// get_object on a missing key returns an error (NotFound).
#[tokio::test]
async fn test_get_object_not_found() {
    if check_minio_configured().is_none() {
        eprintln!("SKIP: AWS_ENDPOINT_URL not set");
        return;
    }

    let (client, _cache) = create_test_client().await;
    let key = "integration/does-not-exist.txt";

    let result = client.get_object(BUCKET, key).await;
    assert!(
        result.is_err(),
        "expected NotFound error for nonexistent key"
    );
}

/// head_object reports the correct content_length for an uploaded object.
#[tokio::test]
async fn test_head_object() {
    if check_minio_configured().is_none() {
        eprintln!("SKIP: AWS_ENDPOINT_URL not set");
        return;
    }

    let (client, _cache) = create_test_client().await;
    let key = "integration/head-test.txt";
    let content = b"head me";

    put_test_object(&client, BUCKET, key, content).await;

    let raw = raw_s3_client().await;
    let head = raw
        .head_object()
        .bucket(BUCKET)
        .key(key)
        .send()
        .await
        .expect("head_object");

    assert_eq!(
        head.content_length(),
        Some(content.len() as i64),
        "head_object content_length mismatch"
    );

    // Cleanup.
    raw.delete_object()
        .bucket(BUCKET)
        .key(key)
        .send()
        .await
        .expect("delete_object cleanup");
}

/// delete_object removes the object; a subsequent get fails.
#[tokio::test]
async fn test_delete_object() {
    if check_minio_configured().is_none() {
        eprintln!("SKIP: AWS_ENDPOINT_URL not set");
        return;
    }

    let (client, _cache) = create_test_client().await;
    let key = "integration/delete-test.txt";

    put_test_object(&client, BUCKET, key, b"delete me").await;

    let raw = raw_s3_client().await;
    raw.delete_object()
        .bucket(BUCKET)
        .key(key)
        .send()
        .await
        .expect("delete_object");

    let get_after_delete = raw.get_object().bucket(BUCKET).key(key).send().await;
    assert!(
        get_after_delete.is_err(),
        "expected error fetching deleted object"
    );
}

/// 64 KiB payload round-trip verifies content integrity end to end.
#[tokio::test]
async fn test_put_object_content_integrity() {
    if check_minio_configured().is_none() {
        eprintln!("SKIP: AWS_ENDPOINT_URL not set");
        return;
    }

    let (client, _cache) = create_test_client().await;
    let key = "integration/large-object.bin";

    // Write 64 KB of pseudo-random data.
    let data: Vec<u8> = (0..65536u32).flat_map(|i| i.to_be_bytes()).collect();

    let result = put_test_object(&client, BUCKET, key, &data).await;
    assert_eq!(result.sha256.len(), 71); // "sha256:" + 64 hex chars

    // Drop the local cache so get_object is forced to fetch from MinIO.
    if result.cache_path.exists() {
        std::fs::remove_file(&result.cache_path).expect("remove cache file");
    }

    // Read back and verify.
    let cached_path = client.get_object(BUCKET, key).await.expect("get_object");
    let roundtrip = std::fs::read(&cached_path).expect("read cache");
    assert_eq!(roundtrip, data);

    // Cleanup.
    let raw = raw_s3_client().await;
    raw.delete_object()
        .bucket(BUCKET)
        .key(key)
        .send()
        .await
        .expect("delete_object cleanup");
}

/// Read-only client test does NOT need MinIO — exercises the local
/// write-enabled guard only.
#[tokio::test]
async fn test_read_only_client_rejects_writes() {
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
