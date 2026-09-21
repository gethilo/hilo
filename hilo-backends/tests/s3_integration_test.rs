// Integration tests for the S3 backend against a REAL S3-compatible
// endpoint (MinIO, moto, SeaweedFS, …), driven by AWS_ENDPOINT_URL.
//
// Prerequisites:
//   1. Start MinIO:  ./tests/integration/start.sh
//   2. Export env:   eval "$(./tests/integration/start.sh)"
//   3. Run tests:    cargo test -p hilo_backends --test s3_integration_test
//
// Environment variables required:
//   AWS_ENDPOINT_URL      — S3 endpoint (e.g. http://localhost:9000)
//   AWS_ACCESS_KEY_ID     — access key (default: hilo_test)
//   AWS_SECRET_ACCESS_KEY — secret key (default: hilo_test)
//   AWS_REGION            — AWS region (default: us-east-1)
//
// Skips are HONEST (DF-WARPFS-13): when AWS_ENDPOINT_URL is unset the S3
// tests register as `#[ignore]`d, so they surface in the "N ignored" count
// instead of being scored as passes. When the variable IS set but the
// endpoint is unusable, the tests FAIL — a configured-but-broken endpoint is
// a misconfiguration, not a skip. The readiness probe makes a real S3
// request (ListObjectsV2 with the configured credentials) and accepts any
// endpoint that answers; it never keys off a vendor-specific health path
// like MinIO's /minio/health/live, which a non-MinIO S3 server (correctly)
// does not serve.
//
// A skip's reason is still observable for humans: each guarded test prints
// `SKIP (ignored): <reason>` when it is ignored, and `cargo test --
// --ignored` prints nothing but the ignore annotations when no endpoint is
// configured.
//
// The production `S3Client` honors AWS_ENDPOINT_URL explicitly with
// path-style addressing (required by MinIO). A raw `aws_sdk_s3::Client`
// is used for head/delete/bucket operations that the backend does not expose.

use aws_sdk_s3 as s3;
use aws_sdk_s3::error::DisplayErrorContext;
use hilo_backends::{S3Client, WriteResult};
use std::env;
use tempfile::TempDir;

const BUCKET: &str = "hilo-test-bucket";

/// Probe the configured S3 endpoint with a REAL S3 call and decide whether
/// the endpoint tests should run.
///
/// - AWS_ENDPOINT_URL unset/empty → Skip::NoEndpoint (tests stay ignored).
/// - Set but the ListBuckets probe errors → Skip::Unusable (and the
///   guarded tests, which call this again, panic with the reason).
/// - A live S3 endpoint (any vendor) → runs the tests.
///
/// This is deliberately NOT a vendor health path: the probe tests the
/// protocol, not the vendor. `ListBuckets` is the one call every S3
/// implementation answers without needing a bucket to exist first.
enum Probe {
    Run(String),
    NoEndpoint,
    Unusable(String),
}

async fn probe_s3() -> Probe {
    let Some(endpoint) = env::var("AWS_ENDPOINT_URL").ok().filter(|e| !e.is_empty()) else {
        return Probe::NoEndpoint;
    };

    let probe = raw_s3_client().list_buckets().send().await;
    match probe {
        Ok(_) => Probe::Run(endpoint),
        Err(err) => Probe::Unusable(format!(
            "AWS_ENDPOINT_URL={endpoint} is set but did not answer a real S3 request \
             (ListBuckets): {}",
            DisplayErrorContext(&err)
        )),
    }
}

/// Only meaningful when AWS_ENDPOINT_URL is set: the guarded tests build
/// clients and objects against the configured endpoint + credentials.
fn require_endpoint() -> String {
    env::var("AWS_ENDPOINT_URL").expect("AWS_ENDPOINT_URL must be set for ignored tests")
}

/// Skip helper: prints the skip reason for humans. The harness-level skip
/// signal is the `#[ignore]` attribute on each guarded test — a bare early
/// `return` is scored as a PASS and is exactly the DF-WARPFS-13 defect.
macro_rules! skip_note {
    ($reason:expr) => {
        eprintln!("SKIP (ignored): {}", $reason);
    };
}

/// Guard macro: the test body only runs against a live S3 endpoint.
/// Without AWS_ENDPOINT_URL the test prints its skip reason (it was already
/// `#[ignore]`d, so it shows up in the ignore count, not the pass count).
/// With AWS_ENDPOINT_URL set but the endpoint unusable, the test FAILS —
/// a configured-but-dead endpoint is an error, not a skip.
macro_rules! require_s3 {
    () => {
        match probe_s3().await {
            Probe::Run(endpoint) => endpoint,
            Probe::NoEndpoint => {
                skip_note!("AWS_ENDPOINT_URL not set — S3 endpoint tests skipped");
                return;
            }
            Probe::Unusable(reason) => {
                panic!("{}", reason);
            }
        }
    };
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

/// Build a low-level aws_sdk_s3::Client against the configured endpoint with
/// static credentials and path-style addressing (required by MinIO).
fn raw_s3_client() -> s3::Client {
    let endpoint = require_endpoint();
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

/// Ensure the test bucket exists (create when missing; a pre-existing bucket
/// we own is fine). Makes every test self-contained on a fresh endpoint.
async fn ensure_bucket(raw: &s3::Client, bucket: &str) {
    match raw.create_bucket().bucket(bucket).send().await {
        Ok(_) => {}
        Err(err) => {
            let ctx = format!("{}", DisplayErrorContext(&err));
            assert!(
                ctx.contains("BucketAlreadyOwnedByYou"),
                "create_bucket {bucket} failed and was not a benign already-exists: {ctx}"
            );
        }
    }
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
#[ignore = "requires a live S3 endpoint (set AWS_ENDPOINT_URL)"]
async fn test_put_and_get_object() {
    let endpoint = require_s3!();
    eprintln!("Using S3 endpoint {endpoint}");

    let (client, _cache) = create_test_client().await;
    ensure_bucket(&raw_s3_client(), BUCKET).await;
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
    let raw = raw_s3_client();
    raw.delete_object()
        .bucket(BUCKET)
        .key(key)
        .send()
        .await
        .expect("delete_object cleanup");
}

/// put two objects under a prefix → list_objects returns both.
#[tokio::test]
#[ignore = "requires a live S3 endpoint (set AWS_ENDPOINT_URL)"]
async fn test_list_objects() {
    let endpoint = require_s3!();
    eprintln!("Using S3 endpoint {endpoint}");

    let (client, _cache) = create_test_client().await;
    ensure_bucket(&raw_s3_client(), BUCKET).await;
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
    let raw = raw_s3_client();
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
#[ignore = "requires a live S3 endpoint (set AWS_ENDPOINT_URL)"]
async fn test_get_object_not_found() {
    let endpoint = require_s3!();
    eprintln!("Using S3 endpoint {endpoint}");

    let (client, _cache) = create_test_client().await;
    ensure_bucket(&raw_s3_client(), BUCKET).await;
    let key = "integration/does-not-exist.txt";

    let result = client.get_object(BUCKET, key).await;
    assert!(
        result.is_err(),
        "expected NotFound error for nonexistent key"
    );
}

/// head_object reports the correct content_length for an uploaded object.
#[tokio::test]
#[ignore = "requires a live S3 endpoint (set AWS_ENDPOINT_URL)"]
async fn test_head_object() {
    let endpoint = require_s3!();
    eprintln!("Using S3 endpoint {endpoint}");

    let (client, _cache) = create_test_client().await;
    ensure_bucket(&raw_s3_client(), BUCKET).await;
    let key = "integration/head-test.txt";
    let content = b"head me";

    put_test_object(&client, BUCKET, key, content).await;

    let raw = raw_s3_client();
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
#[ignore = "requires a live S3 endpoint (set AWS_ENDPOINT_URL)"]
async fn test_delete_object() {
    let endpoint = require_s3!();
    eprintln!("Using S3 endpoint {endpoint}");

    let (client, _cache) = create_test_client().await;
    ensure_bucket(&raw_s3_client(), BUCKET).await;
    let key = "integration/delete-test.txt";

    put_test_object(&client, BUCKET, key, b"delete me").await;

    let raw = raw_s3_client();
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
#[ignore = "requires a live S3 endpoint (set AWS_ENDPOINT_URL)"]
async fn test_put_object_content_integrity() {
    let endpoint = require_s3!();
    eprintln!("Using S3 endpoint {endpoint}");

    let (client, _cache) = create_test_client().await;
    ensure_bucket(&raw_s3_client(), BUCKET).await;
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
    let raw = raw_s3_client();
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

/// DF-WARPFS-13 (would have caught DF-WARPFS-10): the FULL sync-driver path
/// — SyncEngine::plan + sync, not just the S3Client primitives — against an
/// EMPTY bucket. A fresh bucket starts with no objects and no prefix; the
/// first sync must list an empty remote cleanly and upload the local files.
/// This is exactly the surface DF-WARPFS-10 broke ("first push to a fresh
/// bucket always fails") while the primitive-level tests stayed green.
#[tokio::test]
#[ignore = "requires a live S3 endpoint (set AWS_ENDPOINT_URL)"]
async fn test_sync_engine_first_push_to_empty_bucket() {
    use hilo_backends::{IgnoreMatcher, SyncEngine};

    let endpoint = require_s3!();
    eprintln!("Using S3 endpoint {endpoint}");

    let raw = raw_s3_client();

    // A bucket that is guaranteed EMPTY: unique per run, created and drained
    // by this test — never pre-seeded by the environment.
    let bucket = format!("hilo-sync-empty-{}", std::process::id());
    raw.create_bucket().bucket(&bucket).send().await.expect(
        "create_bucket for the fresh-bucket sync test (endpoint must allow bucket creation)",
    );

    // Local workspace with two small files and nothing else.
    let ws = TempDir::new().expect("tempdir workspace");
    std::fs::write(ws.path().join("alpha.txt"), b"alpha").expect("write alpha");
    std::fs::create_dir(ws.path().join("sub")).expect("mkdir sub");
    std::fs::write(ws.path().join("sub/beta.txt"), b"beta").expect("write beta");

    let (client, _cache) = create_test_client().await;
    let engine = SyncEngine::new(
        client,
        bucket.clone(),
        String::new(),
        ws.path().to_path_buf(),
        IgnoreMatcher::empty(),
    );

    // Plan against the empty bucket: everything local must be an upload,
    // nothing remote to download.
    let plan = engine.plan().await.expect("plan against empty bucket");
    assert!(
        plan.downloads.is_empty(),
        "an empty bucket must yield no downloads: {plan:?}"
    );
    let mut planned: Vec<&str> = plan.uploads.iter().map(|u| u.rel_path.as_str()).collect();
    planned.sort();
    assert_eq!(planned, vec!["alpha.txt", "sub/beta.txt"]);

    // Execute the first push.
    let plan = engine.sync().await.expect("first sync to empty bucket");
    assert_eq!(plan.uploads.len(), 2, "both files uploaded: {plan:?}");

    // Verify the objects actually landed (HeadObject, protocol-level proof).
    for key in ["alpha.txt", "sub/beta.txt"] {
        let head = raw
            .head_object()
            .bucket(&bucket)
            .key(key)
            .send()
            .await
            .unwrap_or_else(|e| panic!("head_object {key} after first sync: {e}"));
        assert!(
            head.content_length().unwrap_or(0) > 0,
            "{key} uploaded with empty body"
        );
    }

    // A second sync must be a no-op (uploads+downloads empty, 2 unchanged).
    let plan2 = engine.sync().await.expect("second sync");
    assert!(
        plan2.uploads.is_empty() && plan2.downloads.is_empty(),
        "second sync must be a no-op: {plan2:?}"
    );
    assert_eq!(plan2.unchanged, 2, "both files unchanged on second sync");

    // Cleanup: drain then delete the bucket so reruns start empty.
    let keys = raw
        .list_objects_v2()
        .bucket(&bucket)
        .send()
        .await
        .expect("list for cleanup");
    for obj in keys.contents() {
        raw.delete_object()
            .bucket(&bucket)
            .key(obj.key().expect("object key"))
            .send()
            .await
            .expect("cleanup delete");
    }
    raw.delete_bucket().bucket(&bucket).send().await.ok();
}
