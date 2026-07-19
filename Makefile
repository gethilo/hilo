.PHONY: test test-integration test-integration-up test-integration-down fmt clippy check

test:
	cargo test --workspace

# Integration tests against a live MinIO (docker compose).
# Brings the stack up, creates the test bucket, runs the integration tests
# with the AWS_* environment exported, and always tears the stack down.
test-integration: test-integration-up
	@status=0; \
	eval "$$(./tests/integration/start.sh)" && \
	cargo test -p hilo_backends --test s3_integration_test || status=$$?; \
	./tests/integration/stop.sh; \
	exit $$status

test-integration-up:
	docker compose up -d

test-integration-down:
	./tests/integration/stop.sh

check:
	cargo check --workspace

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace -- -D warnings
