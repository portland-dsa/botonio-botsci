-- Per-guild moderator toggle for the SSO role-check endpoint: the runtime half of the
-- two-gate model. The deploy flag BOT_SSO_ENABLED binds the socket at boot; this column
-- decides, live, whether the bot actually answers an SSO check for the guild. Both must be
-- set; this can only further-restrict, never override the deploy flag. Off by default.
--
-- No GRANT statement: this is a column addition to an existing table, and guild_config
-- already carries a table-level grant to the runtime role (botonio_app), which covers
-- columns added later. New *tables* are the case that needs an explicit grant.
ALTER TABLE guild_config
    ADD COLUMN sso_enabled BOOLEAN NOT NULL DEFAULT false;
