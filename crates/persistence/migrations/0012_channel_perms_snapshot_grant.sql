-- Backfill the DML grant migration 0010 omitted. channel_perms_snapshot was created
-- without granting the runtime role any rights, so /channels save's INSERT permission-denies
-- against the DML-only botonio_app role. A separate forward-only migration because 0010 is
-- already applied to staging and production, where editing it in place would break the
-- recorded migration checksum. GRANT is idempotent, so this is safe everywhere 0010 ran.
--
-- SELECT + INSERT + DELETE, no UPDATE: save_snapshot inserts a row and reaps the bounded
-- undo stack, latest/list only read - snapshots are append-then-trim, never edited in place
-- (the same no-UPDATE shape member_cache uses). The BIGSERIAL id default calls nextval, so
-- the runtime role also needs usage on the implicit sequence.
GRANT SELECT, INSERT, DELETE ON channel_perms_snapshot TO botonio_app;
GRANT USAGE, SELECT ON SEQUENCE channel_perms_snapshot_id_seq TO botonio_app;
