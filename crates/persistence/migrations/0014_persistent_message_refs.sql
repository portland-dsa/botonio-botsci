-- Edit standing /setup messages in place. Remember where the verification prompt and the
-- dues-expiring banner were last posted so re-publishing edits the existing message rather
-- than posting a duplicate. Each reference is a (channel, message) id pair, both null until
-- the message is first posted and written together thereafter.
--
-- No GRANT statement: these are column additions to an existing table, and guild_config
-- already carries a table-level grant to the runtime role (botonio_app), which covers
-- columns added later. New *tables* are the case that needs an explicit grant.
ALTER TABLE guild_config
    ADD COLUMN unverified_prompt_channel_id BIGINT,
    ADD COLUMN unverified_prompt_message_id BIGINT,
    ADD COLUMN dues_banner_channel_id       BIGINT,
    ADD COLUMN dues_banner_message_id       BIGINT;
