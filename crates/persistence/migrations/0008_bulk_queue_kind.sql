-- Why a member is in the bulk-verify wizard queue: a true miss (ST does not know
-- them) or a malformed record (matched, but no usable standing). Existing rows and
-- the common case default to 'miss'.
ALTER TABLE bulk_verify_miss
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'miss'
        CHECK (kind IN ('miss', 'malformed'));
