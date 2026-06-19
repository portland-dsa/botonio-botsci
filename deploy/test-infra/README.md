# Test infrastructure: throwaway Postgres

A containerized PostgreSQL for local development and CI. It backs two things in the
`persistence` crate:

- generating the committed compile-time **sqlx query cache** (`crates/persistence/.sqlx/`), and
- running the **`live-db` conformance tests** (`PgStore` vs `InMemoryStore`).

It is deliberately throwaway: trust authentication, an in-memory (`tmpfs`) data
directory, and a non-standard host port. It holds no real data and is safe to wipe.
**It is not the production database** - that one is provisioned by hand with real
hardening and credentials.

## Bring it up

```sh
podman compose -f deploy/test-infra/compose.yaml up -d --build
export DATABASE_URL=postgres://postgres@localhost:55432/botonio_dev
```

The cluster listens on host port **55432** (mapped to the container's 5432) so it
never clashes with a Postgres already running on 5432. The `botonio_app` group role
the migrations grant to is created automatically on startup (baked into the image
from `initdb/`).

## Use it

```sh
# Regenerate the committed query cache after changing any query! / migration:
cargo sqlx prepare -p persistence

# Run the Postgres-backed conformance suite:
cargo test -p persistence --features live-db
```

`sqlx::test` provisions a fresh, migrated database per test from `DATABASE_URL`, so
the tests do not interfere with each other or with a cache-generation run.

## Tear it down

```sh
podman compose -f deploy/test-infra/compose.yaml down
```

(Docker users can substitute `docker compose` - the files are compatible.)
