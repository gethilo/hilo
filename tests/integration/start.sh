#!/usr/bin/env bash
# Start the MinIO test stack, wait for health, create the test bucket,
# and print the AWS_* environment exports for the integration tests.
#
# Usage:
#   ./tests/integration/start.sh          # start stack + bucket, print exports
#   eval "$(./tests/integration/start.sh)"  # start stack + set env in this shell
set -euo pipefail

cd "$(dirname "$0")/../.."

BUCKET_NAME="hilo-test-bucket"
ENDPOINT_URL="http://localhost:9000"
ACCESS_KEY="hilo_test"
SECRET_KEY="hilo_test"

echo "==> Starting MinIO via docker compose..." >&2
docker compose up -d

echo "==> Waiting for MinIO health check to return 200..." >&2
attempts=0
max_attempts=60
until [ "$(curl -s -o /dev/null -w '%{http_code}' "${ENDPOINT_URL}/minio/health/live")" = "200" ]; do
    attempts=$((attempts + 1))
    if [ "${attempts}" -ge "${max_attempts}" ]; then
        echo "ERROR: MinIO did not become healthy within ${max_attempts}s" >&2
        exit 1
    fi
    sleep 1
done
echo "==> MinIO is healthy" >&2

echo "==> Ensuring test bucket '${BUCKET_NAME}' exists..." >&2
AWS_ACCESS_KEY_ID="${ACCESS_KEY}" \
AWS_SECRET_ACCESS_KEY="${SECRET_KEY}" \
aws --endpoint-url "${ENDPOINT_URL}" s3api create-bucket \
    --bucket "${BUCKET_NAME}" >/dev/null 2>&1 || true

echo "==> Stack ready" >&2

# Exports (stdout — safe to eval)
echo "export AWS_ENDPOINT_URL=${ENDPOINT_URL}"
echo "export AWS_ACCESS_KEY_ID=${ACCESS_KEY}"
echo "export AWS_SECRET_ACCESS_KEY=${SECRET_KEY}"
