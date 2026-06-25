-- Dues reminder redesign: one notice, the Dues Expiring marker, generalized templates.

-- Guild config: add the additive Dues Expiring marker role; the lifecycle thread now
-- parents off dues_expired_channel, so the separate reminder channel is retired.
ALTER TABLE guild_config
    ADD COLUMN dues_expiring_role_id BIGINT,
    DROP COLUMN dues_reminder_channel_id;

-- Cycle state: snooze is gone; track whether the Dues Expiring marker is currently held
-- so the sweep grants/removes it once rather than every pass. Map the old ordered
-- milestone tokens onto the new two-value scheme so a mid-cycle member is not re-notified.
ALTER TABLE dues_reminder_state
    DROP COLUMN snoozed,
    ADD COLUMN expiring_marked BOOLEAN NOT NULL DEFAULT false;
UPDATE dues_reminder_state SET last_sent = 'renewal'
    WHERE last_sent IN ('days30', 'days14', 'day1');
UPDATE dues_reminder_state SET last_sent = 'lapse'
    WHERE last_sent = 'expired';

-- Generalized editable copy: the table now holds every editable body, not just dues
-- reminders. The per-state "expired" kind is gone (reminder and lapse share the per-type
-- paragraph); unverified + dues_banner rows are created on first edit.
ALTER TABLE dues_reminder_template RENAME TO message_template;
DELETE FROM message_template WHERE kind = 'expired';

-- The catch-up timestamp is no longer needed (no round-to-nearest decision).
DROP TABLE dues_reminder_run;
