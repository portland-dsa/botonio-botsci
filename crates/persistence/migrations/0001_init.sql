-- member_cache: one row per Discord-linked member, a flat projection of the
-- engine MemberRecord. discord_user_id is the only lookup key (immutable snowflake,
-- never the handle). The Role is derived from `standing`, never stored.
CREATE TABLE member_cache (
    discord_user_id BIGINT PRIMARY KEY,
    discord_handle  TEXT,
    email           TEXT NOT NULL,
    full_name       TEXT,
    standing        TEXT,          -- MigsStatus token; NULL when absent
    join_date       DATE,
    expires         DATE,          -- the xdate
    membership_type TEXT,          -- MembershipType token
    monthly_dues    TEXT,          -- DuesStatus token
    yearly_dues     TEXT,          -- DuesStatus token
    refreshed_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- audit_log: append-only mod-action record. Empty until a later slice; created now
-- so the INSERT-only privilege model is correct from the first migration.
CREATE TABLE audit_log (
    id           BIGSERIAL PRIMARY KEY,
    occurred_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    actor_hash   TEXT NOT NULL,    -- hashed Discord id, never raw PII
    subject_hash TEXT NOT NULL,
    action       TEXT NOT NULL,
    detail       JSONB NOT NULL DEFAULT '{}'::jsonb,
    key_id       TEXT NOT NULL     -- names the hashing key, for rotation
);

-- Privileges go to the cluster-wide botonio_app group role (the per-environment runtime
-- roles are members of it; per-database CONNECT + pg_hba keep each boxed to its own
-- database). The botonio_app role MUST already exist when this migration runs - it is
-- created by scripts/setup/db-bootstrap.py in production and by the dev/CI cluster setup.
-- No UPDATE: the cache is replaced wholesale with DELETE + INSERT, never updated in place.
GRANT SELECT, INSERT, DELETE ON member_cache TO botonio_app;
GRANT INSERT                         ON audit_log     TO botonio_app;
-- BIGSERIAL needs sequence usage for the INSERTs a later slice will do.
GRANT USAGE, SELECT ON SEQUENCE audit_log_id_seq TO botonio_app;
