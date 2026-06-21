-- Adds an optional moderator-supplied reason to the hand-approval record. The note sits
-- in manual_override alongside who approved and when - the durable "why", shown to
-- moderators on a lookup. It is deliberately not copied into the audit log, which stays
-- free of operator-entered free text. Nullable: a hand approval with no reason stores
-- NULL. The column inherits manual_override's existing SELECT, INSERT grant.
ALTER TABLE manual_override ADD COLUMN note TEXT;
