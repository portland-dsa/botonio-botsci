-- The resumable per-guild bulk-verify session and its frozen miss queue. A guild has
-- at most one session (PK guild_id); any moderator resumes it. The miss queue is the
-- members a sweep could not match, walked one at a time by the wizard - keyed on the
-- immutable Discord id, never the handle (which is a display snapshot only). DELETE is
-- granted here, unlike guild_config: "Start over" and the staleness purge replace the
-- queue wholesale, the same DML-only pattern replace_roster already uses.
CREATE TABLE bulk_verify_session (
    guild_id   BIGINT PRIMARY KEY,
    scope      TEXT NOT NULL,           -- 'unmanaged' | 'whole_guild'
    status     TEXT NOT NULL,           -- 'in_progress' | 'complete' | 'abandoned'
    started_by BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE bulk_verify_miss (
    guild_id        BIGINT NOT NULL REFERENCES bulk_verify_session(guild_id) ON DELETE CASCADE,
    discord_user_id BIGINT NOT NULL,
    handle          TEXT,               -- display snapshot only, never used for matching
    position        INT  NOT NULL,
    state           TEXT NOT NULL DEFAULT 'pending',  -- 'pending' | 'verified' | 'skipped'
    PRIMARY KEY (guild_id, discord_user_id)
);

CREATE INDEX bulk_verify_miss_pending ON bulk_verify_miss (guild_id, position)
    WHERE state = 'pending';

GRANT SELECT, INSERT, UPDATE, DELETE ON bulk_verify_session TO botonio_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON bulk_verify_miss    TO botonio_app;
