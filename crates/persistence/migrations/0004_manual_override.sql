-- Adds the Manual Override marker role to guild_config and the permanent record of
-- hand-approved members. The marker role is granted in addition to Member when a
-- moderator vouches for someone Solidarity Tech does not know; manual_override is the
-- durable, queryable note of who approved them and when - keyed on the immutable Discord
-- id, since an overridden member has no Solidarity Tech id to key on. SELECT and INSERT
-- only: the stamp is permanent and immutable for the runtime role.
ALTER TABLE guild_config ADD COLUMN manual_override_role_id BIGINT;

CREATE TABLE manual_override (
    discord_user_id BIGINT PRIMARY KEY,
    approved_by     BIGINT NOT NULL,
    approved_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

GRANT SELECT, INSERT ON manual_override TO botonio_app;
