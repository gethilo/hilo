#!/usr/bin/env bash
# Tear down the MinIO test stack and remove its named volume.
set -euo pipefail

cd "$(dirname "$0")/../.."

echo "==> Stopping MinIO test stack..."
docker compose down -v

echo "==> Done"
