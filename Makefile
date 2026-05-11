# Mirror of .github/workflows/test.yml. Run `make ci` before pushing to
# catch what CI would catch. Both CI and local invoke the same targets,
# so the two can never silently diverge.
#
# Assumes already installed: rustup, cargo-nextest, sqlx-cli, docker.

.PHONY: ci ci-sqlx ci-test ci-build ci-docker setup help

setup:  ## install dev tools CI uses (cargo-nextest, sqlx-cli) — run once
	cargo install cargo-nextest --locked
	cargo install sqlx-cli --no-default-features --features rustls,postgres --locked

ci: ci-sqlx ci-test ci-build ci-docker  ## run every CI check

ci-sqlx:  ## apply migrations + verify .sqlx offline data is in sync (needs DATABASE_URL + running postgres)
	cargo sqlx migrate run --source bridge_validator/migrations
	cd bridge_validator && cargo sqlx prepare --check -- --tests

ci-test:  ## run test suite (needs Docker daemon for testcontainers)
	SQLX_OFFLINE=true cargo nextest run --workspace --locked

ci-build:  ## build release binary the same way the Dockerfile does
	SQLX_OFFLINE=true cargo build --release --locked --bin worker

ci-docker:  ## build the prod Docker image end-to-end
	docker build -f bridge_validator/Dockerfile -t bridge-validator:ci .

help:  ## list targets
	@grep -hE '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) | awk -F':.*?## ' '{printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'
