-- Whole-guild channel-permission snapshots for the terraform save/restore.
-- History is kept (one row per save), so successive saves form an undo stack.
create table channel_perms_snapshot (
    id             bigserial primary key,
    guild_id       bigint      not null,
    saved_at       timestamptz not null,
    format_version integer     not null,
    channels       jsonb       not null
);

create index channel_perms_snapshot_guild_saved
    on channel_perms_snapshot (guild_id, saved_at desc);
