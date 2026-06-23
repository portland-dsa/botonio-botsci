-- The moderator-private channel where every successful self-service verification
-- is logged (the member, the email that matched, the granted role). Nullable like
-- the other channel settings: unset until a moderator picks it in /setup. Without
-- it the self-verify grant still happens; only the audit post is skipped.
ALTER TABLE guild_config
    ADD COLUMN verification_log_channel_id BIGINT;
