-- Re-key member_cache on the Solidarity Tech user id so a member known to Solidarity
-- Tech by handle but not yet linked to a Discord id can still be cached and found.
-- discord_user_id and discord_handle become nullable, indexed lookup columns; the
-- Solidarity Tech id is the stable key. The table is a rebuildable cache, so it is
-- recreated rather than migrated row by row: existing rows carry no Solidarity Tech id
-- to key on, and the next roster sweep repopulates it.
DROP TABLE member_cache;

CREATE TABLE member_cache (
    st_user_id      TEXT PRIMARY KEY,
    discord_user_id BIGINT,
    discord_handle  TEXT,
    email           TEXT NOT NULL,
    full_name       TEXT,
    standing        TEXT,
    join_date       DATE,
    expires         DATE,
    membership_type TEXT,
    monthly_dues    TEXT,
    yearly_dues     TEXT,
    refreshed_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The two identity lookups: by immutable id first, by handle as the repair fallback.
CREATE INDEX member_cache_discord_user_id_idx ON member_cache (discord_user_id);
CREATE INDEX member_cache_discord_handle_idx  ON member_cache (discord_handle);

-- The same DML grants, plus a column-scoped UPDATE for the single-row identity
-- write-back. UPDATE is limited to the two identity columns; the rest of a row is still
-- only ever replaced wholesale by the roster refresh.
GRANT SELECT, INSERT, DELETE ON member_cache TO botonio_app;
GRANT UPDATE (discord_user_id, discord_handle) ON member_cache TO botonio_app;
