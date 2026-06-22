-- Opt-in flag for the scheduled membership scan. Off by default: a guild runs the
-- periodic role-sync / orphan sweep only after a server manager enables it in /setup.
ALTER TABLE guild_config
    ADD COLUMN scan_enabled BOOLEAN NOT NULL DEFAULT FALSE;
