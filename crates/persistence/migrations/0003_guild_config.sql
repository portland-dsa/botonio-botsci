-- guild_config: one row per guild of the bot's /setup-managed Discord-resource
-- configuration - the moderator role, the three managed status roles, and the
-- three verification channels. Every setting is nullable: a freshly deployed
-- guild has nothing set until a moderator runs /setup. Edited in place (UPSERT),
-- unlike member_cache which is replaced wholesale - hence the table-wide UPDATE.
CREATE TABLE guild_config (
    guild_id                BIGINT PRIMARY KEY,
    moderator_role_id       BIGINT,
    member_role_id          BIGINT,
    dues_expired_role_id    BIGINT,
    unverified_role_id      BIGINT,
    mod_approval_channel_id BIGINT,
    unverified_channel_id   BIGINT,
    dues_expired_channel_id BIGINT,
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

GRANT SELECT, INSERT, UPDATE ON guild_config TO botonio_app;
