//! Pure embed builders for the bulk-verify flow: the preview (role counts + misses),
//! the apply-phase progress bar, the wizard card shown per miss, the resume prompt for
//! an in-progress session, and the final summary.
//!
//! Every function is network-free and takes only value types so each can be
//! unit-tested by serialising its output to JSON.

use serenity::all::CreateEmbed;

use domain::Role;
use engine::backends::util::DiscordUserId;
use engine::bulk::PreviewTally;
use engine::store::{BulkScope, MissCounts};

use crate::render::card::{COLOR_AMBER, COLOR_GREEN};

/// Discord blurple: used for neutral/in-progress states.
const COLOR_BLURPLE: u32 = 0x58_65_f2;

/// Render a human-readable scope line for use in embed descriptions.
fn scope_line(scope: BulkScope) -> &'static str {
    match scope {
        BulkScope::UnmanagedOnly => "Scope: unmanaged members only",
        BulkScope::WholeGuild => "Scope: the whole server",
    }
}

/// Build the resume-session embed shown when a moderator enters `/bulk-verify` and
/// an in-progress session already exists. Names the moderator who started it and
/// the number of members still queued.
pub fn resume_embed(
    scope: BulkScope,
    started_by: DiscordUserId,
    counts: MissCounts,
) -> CreateEmbed {
    let desc = format!(
        "{}\nStarted by <@{}>\n\n\
         {pending} member{s} still pending review ({verified} verified, {skipped} skipped).\n\
         You can continue where this left off or start over.",
        scope_line(scope),
        started_by.0,
        pending = counts.pending,
        s = if counts.pending == 1 { "" } else { "s" },
        verified = counts.verified,
        skipped = counts.skipped,
    );
    CreateEmbed::new()
        .colour(COLOR_BLURPLE)
        .title("Bulk verify \u{2022} In progress")
        .description(desc)
}

/// Build the preview embed shown before the moderator confirms a bulk-verify run.
/// Lists each role that has a non-zero change count (members already in their correct role
/// are summarised separately, never as a change), then misses and (when non-zero) conflicts.
pub fn preview_embed(scope: BulkScope, tally: &PreviewTally) -> CreateEmbed {
    let mut lines: Vec<String> = Vec::new();
    lines.push(scope_line(scope).to_string());
    lines.push(format!(
        "Scanned: {} member{}",
        tally.scanned,
        if tally.scanned == 1 { "" } else { "s" }
    ));
    lines.push(String::new());

    for (role, count) in &tally.matched {
        if *count > 0 {
            lines.push(format!("{}: {}", role.as_str(), count));
        }
    }

    if tally.unchanged > 0 {
        lines.push(format!("Already in the right role: {}", tally.unchanged));
    }

    lines.push(format!("Not matched (wizard queue): {}", tally.misses));

    if tally.conflicts > 0 {
        lines.push(format!(
            "Conflicts: {} \u{2014} resolve these with /verify",
            tally.conflicts
        ));
    }

    let color = if tally.misses > 0 || tally.conflicts > 0 {
        COLOR_AMBER
    } else {
        COLOR_GREEN
    };

    CreateEmbed::new()
        .colour(color)
        .title("Bulk verify \u{2022} Preview")
        .description(lines.join("\n"))
}

/// Build the progress embed shown while the bot is applying role assignments in bulk.
/// Shows `"{done} / {total}"` so the moderator can see how far along the run is.
pub fn progress_embed(done: usize, total: usize) -> CreateEmbed {
    CreateEmbed::new()
        .colour(COLOR_BLURPLE)
        .title("Bulk verify \u{2022} Applying")
        .description(format!("{done} / {total}"))
}

/// Build the wizard embed shown for each unmatched (miss) member during the
/// manual-review phase. Identifies the member by their display name and handle, and
/// shows their position in the queue.
pub fn wizard_embed(
    display_name: &str,
    handle: &str,
    avatar_url: &str,
    position: usize,
    total: usize,
) -> CreateEmbed {
    CreateEmbed::new()
        .colour(COLOR_AMBER)
        .title(format!("Bulk verify \u{2022} {position} of {total}"))
        .thumbnail(avatar_url)
        .description(format!(
            "`@{handle}`\n\n\
             **{display_name}** is not matched in our records.\n\
             Use the buttons below to verify them by email or skip.",
        ))
}

/// Build the final summary embed shown when a bulk-verify session completes.
/// Mirrors the preview layout for assigned roles, then adds the reviewed/skipped/
/// still-pending tally from `counts`.
pub fn summary_embed(assigned: &[(Role, usize)], counts: MissCounts) -> CreateEmbed {
    let mut lines: Vec<String> = Vec::new();

    for (role, count) in assigned {
        if *count > 0 {
            lines.push(format!("{}: {}", role.as_str(), count));
        }
    }

    lines.push(String::new());
    lines.push(format!("Verified from queue: {}", counts.verified));
    lines.push(format!("Skipped: {}", counts.skipped));

    if counts.pending > 0 {
        lines.push(format!("Still pending: {}", counts.pending));
    }

    let color = if counts.pending > 0 {
        COLOR_AMBER
    } else {
        COLOR_GREEN
    };

    CreateEmbed::new()
        .colour(color)
        .title("Bulk verify \u{2022} Complete")
        .description(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(v: &serde_json::Value) -> u64 {
        v["color"].as_u64().unwrap()
    }

    fn desc(v: &serde_json::Value) -> String {
        v["description"].as_str().unwrap().to_string()
    }

    fn title(v: &serde_json::Value) -> String {
        v["title"].as_str().unwrap().to_string()
    }

    fn json(e: CreateEmbed) -> serde_json::Value {
        serde_json::to_value(&e).unwrap()
    }

    #[test]
    fn resume_embed_is_blurple_and_names_starter_and_pending() {
        let counts = MissCounts {
            pending: 5,
            verified: 2,
            skipped: 1,
        };
        let v = json(resume_embed(
            BulkScope::UnmanagedOnly,
            DiscordUserId(42),
            counts,
        ));
        assert_eq!(color(&v), COLOR_BLURPLE as u64);
        let d = desc(&v);
        assert!(d.contains("<@42>"), "must mention the starter");
        assert!(
            d.contains("5 members still pending"),
            "must show pending count"
        );
        assert!(d.contains("unmanaged members only"), "must show scope");
    }

    #[test]
    fn preview_embed_mixed_tally_skips_zero_rows_and_shows_conflicts() {
        let tally = PreviewTally {
            scanned: 16,
            matched: vec![
                (Role::Member, 4),
                (Role::DuesExpired, 0),
                (Role::Unverified, 2),
            ],
            unchanged: 6,
            misses: 3,
            conflicts: 1,
        };
        let v = json(preview_embed(BulkScope::WholeGuild, &tally));
        assert_eq!(color(&v), COLOR_AMBER as u64);
        let d = desc(&v);
        assert!(d.contains("Member: 4"), "Member count present");
        assert!(!d.contains("Dues Expired: 0"), "zero row must be skipped");
        assert!(d.contains("Unverified: 2"), "Unverified count present");
        assert!(
            d.contains("Already in the right role: 6"),
            "unchanged count present"
        );
        assert!(d.contains("Not matched"), "miss line present");
        assert!(d.contains("Conflicts: 1"), "conflict line present");
        assert!(d.contains("/verify"), "conflict note present");
        assert!(d.contains("the whole server"), "scope present");
    }

    #[test]
    fn preview_embed_all_matched_is_green() {
        let tally = PreviewTally {
            scanned: 5,
            matched: vec![
                (Role::Member, 5),
                (Role::DuesExpired, 0),
                (Role::Unverified, 0),
            ],
            unchanged: 0,
            misses: 0,
            conflicts: 0,
        };
        let v = json(preview_embed(BulkScope::UnmanagedOnly, &tally));
        assert_eq!(color(&v), COLOR_GREEN as u64);
        assert!(
            !desc(&v).contains("Conflicts"),
            "no conflict line when zero"
        );
        assert!(
            !desc(&v).contains("Already in the right role"),
            "no unchanged line when zero"
        );
    }

    #[test]
    fn progress_embed_shows_fraction() {
        let v = json(progress_embed(3, 8));
        assert_eq!(color(&v), COLOR_BLURPLE as u64);
        assert!(desc(&v).contains("3 / 8"), "must show done / total");
        assert!(title(&v).contains("Applying"), "title must say Applying");
    }

    #[test]
    fn wizard_embed_is_amber_and_carries_handle_and_position() {
        let v = json(wizard_embed(
            "Sonic",
            "sonic_hedgehog",
            "http://a/s.png",
            2,
            7,
        ));
        assert_eq!(color(&v), COLOR_AMBER as u64);
        let d = desc(&v);
        assert!(d.contains("@sonic_hedgehog"), "handle present");
        assert!(d.contains("Sonic"), "display name present");
        assert!(title(&v).contains("2 of 7"), "position in title");
    }

    #[test]
    fn summary_embed_all_done_is_green_and_shows_counts() {
        let assigned = vec![
            (Role::Member, 8),
            (Role::DuesExpired, 2),
            (Role::Unverified, 0),
        ];
        let counts = MissCounts {
            pending: 0,
            verified: 3,
            skipped: 1,
        };
        let v = json(summary_embed(&assigned, counts));
        assert_eq!(color(&v), COLOR_GREEN as u64);
        let d = desc(&v);
        assert!(d.contains("Member: 8"), "Member count");
        assert!(d.contains("Dues Expired: 2"), "DuesExpired count");
        assert!(!d.contains("Unverified: 0"), "zero row skipped");
        assert!(d.contains("Verified from queue: 3"), "verified tally");
        assert!(d.contains("Skipped: 1"), "skipped tally");
        assert!(!d.contains("Still pending"), "no pending line when zero");
    }

    #[test]
    fn summary_embed_with_pending_is_amber() {
        let assigned = vec![
            (Role::Member, 3),
            (Role::DuesExpired, 0),
            (Role::Unverified, 0),
        ];
        let counts = MissCounts {
            pending: 2,
            verified: 1,
            skipped: 0,
        };
        let v = json(summary_embed(&assigned, counts));
        assert_eq!(color(&v), COLOR_AMBER as u64);
        assert!(desc(&v).contains("Still pending: 2"), "pending tally shown");
    }
}
