-- Dues-renewal reminders + the manual grace override (feature: dues reminders, completed).

-- Guild config: the channel the reminder threads are parented off (must be visible to
-- both Member and Dues Expired so a thread survives a lapse demotion), the external
-- sign-up URL the Renew button links to, and the reminders_enabled toggle (off by
-- default, like scan_enabled).
ALTER TABLE guild_config
    ADD COLUMN dues_reminder_channel_id BIGINT,
    ADD COLUMN dues_signup_url          TEXT,
    ADD COLUMN reminders_enabled        BOOLEAN NOT NULL DEFAULT false;

-- Permanent, member-scoped opt-out: row presence is the fact.
CREATE TABLE dues_reminder_optout (
    guild_id        BIGINT      NOT NULL,
    discord_user_id BIGINT      NOT NULL,
    opted_out_at    TIMESTAMPTZ NOT NULL,
    source          TEXT        NOT NULL,
    PRIMARY KEY (guild_id, discord_user_id)
);

-- Moderator grace stamp: hold the member at Member until grace_until (inclusive).
CREATE TABLE dues_grace_override (
    guild_id        BIGINT      NOT NULL,
    discord_user_id BIGINT      NOT NULL,
    grace_until     DATE        NOT NULL,
    granted_by      BIGINT      NOT NULL,
    granted_at      TIMESTAMPTZ NOT NULL,
    reason          TEXT,
    PRIMARY KEY (guild_id, discord_user_id)
);

-- Per-member cycle bookkeeping. last_sent is the ordered milestone token (NULL = none);
-- snoozed is scoped to cycle_xdate; thread_id is the lifecycle thread (survives a reset).
CREATE TABLE dues_reminder_state (
    guild_id        BIGINT  NOT NULL,
    discord_user_id BIGINT  NOT NULL,
    cycle_xdate     DATE    NOT NULL,
    last_sent       TEXT,
    snoozed         BOOLEAN NOT NULL DEFAULT false,
    thread_id       BIGINT,
    updated_at      TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (guild_id, discord_user_id)
);

-- Editable per-type reminder bodies. kind in (monthly|yearly|one_time|income_based|expired).
CREATE TABLE dues_reminder_template (
    guild_id BIGINT NOT NULL,
    kind     TEXT   NOT NULL,
    body     TEXT   NOT NULL,
    PRIMARY KEY (guild_id, kind)
);

-- The sweep's last-run marker, for the timely-vs-delayed (catch-up) decision.
CREATE TABLE dues_reminder_run (
    guild_id    BIGINT      NOT NULL PRIMARY KEY,
    last_run_at TIMESTAMPTZ NOT NULL
);

-- The runtime role holds DML only and owns no schema; grant it CRUD on the new tables
-- (the migrate role that runs this migration owns them). Mirrors the per-table grants in
-- the earlier migrations.
GRANT SELECT, INSERT, UPDATE, DELETE ON dues_reminder_optout   TO botonio_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON dues_grace_override    TO botonio_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON dues_reminder_state    TO botonio_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON dues_reminder_template TO botonio_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON dues_reminder_run      TO botonio_app;
