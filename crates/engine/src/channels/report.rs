//! Pure string formatters for a [`ChannelPlan`]: the embed summary rows, the
//! safety list of channels the Unverified role can still view, and the full
//! markdown attachment body a moderator gets when they preview or apply the
//! terraform.
//!
//! Nothing here touches the network or any IO path. Three public functions and
//! one private helper:
//!
//! - [`summary_lines`] - (label, value) pairs for the embed's count rows.
//! - [`unverified_visibility`] - names of channels the Unverified role can still
//!   see after the plan is applied. The headline safety list shown in the embed.
//! - [`detail_markdown`] - the full attachment body, in four sections:
//!   1. Changing categories (each one names how many synced children follow).
//!   2. Desynced/orphan channels that change directly (NOT synced children -
//!      those are folded under their category in section 1).
//!   3. The two restricted channel kinds (unverified-only, dues-expired-only).
//!   4. The Unverified-visibility safety section.
//!
//! The "synced children are not listed individually" rule is the load-bearing
//! constraint: a SyncedToParent channel is counted once under its category, never
//! emitted as its own line, so the detail stays readable for large servers.

use domain::DiscordRoleId;

use super::model::SetupConfig;
use super::model::role_can_view;
use super::plan::{ChannelAction, ChannelPlan, PlannedChannel};

/// The embed's count rows as (label, value) pairs. Values come directly from
/// [`plan.counts`](ChannelPlan::counts); no network or IO.
pub fn summary_lines(plan: &ChannelPlan) -> Vec<(String, String)> {
    let c = &plan.counts;
    vec![
        ("Will write".into(), c.writes.to_string()),
        (
            "Categories -> Member-only".into(),
            c.member_only.to_string(),
        ),
        (
            "Synced children following".into(),
            c.synced_children.to_string(),
        ),
        (
            "Restricted (unverified/dues)".into(),
            (c.unverified_only + c.expired_only).to_string(),
        ),
        ("Left unchanged".into(), (c.unchanged + c.no_op).to_string()),
        ("Excluded".into(), c.excluded.to_string()),
    ]
}

/// Names of every channel the Unverified role can still VIEW after the plan is
/// applied. An empty list means Unverified can see nothing - the ideal result.
/// The embed shows this as the headline safety line; the detail attachment
/// repeats it in its own section.
///
/// Uses the same [`role_can_view`] function the plan uses for breach detection,
/// with the guild's actual `everyone_base_view` from the plan so that a
/// base-private channel is not incorrectly listed as visible.
pub fn unverified_visibility(plan: &ChannelPlan, cfg: &SetupConfig) -> Vec<String> {
    plan.channels
        .iter()
        .filter(|c| {
            role_can_view(
                &c.final_overwrites,
                cfg.everyone,
                plan.everyone_base_view,
                &[cfg.unverified_role],
            )
        })
        .map(|c| c.name.clone())
        .collect()
}

/// The full attachment body in four sections. See module docs for the section
/// order and the synced-children rule.
pub fn detail_markdown(plan: &ChannelPlan, cfg: &SetupConfig) -> String {
    let mut out = String::new();

    // --- Section 1: changing categories ---
    let changing_cats: Vec<&PlannedChannel> = plan
        .channels
        .iter()
        .filter(|c| c.kind.is_category() && c.writes)
        .collect();

    if !changing_cats.is_empty() {
        out.push_str("## Categories that change\n\n");
        for cat in &changing_cats {
            let synced_count = plan
                .channels
                .iter()
                .filter(|c| {
                    c.action == ChannelAction::SyncedToParent && c.parent_id == Some(cat.id)
                })
                .count();
            let child_note = if synced_count == 1 {
                "1 synced child follows".into()
            } else {
                format!("{} synced children follow", synced_count)
            };
            out.push_str(&format!(
                "- #{} (category): -@everyone view, +{} view  [{}]\n",
                cat.name,
                role_label(cfg.member_role, cfg),
                child_note,
            ));
        }
        out.push('\n');
    }

    // --- Section 2: desynced/orphan changing channels (not categories, not synced children) ---
    let desynced_orphans: Vec<&PlannedChannel> = plan
        .channels
        .iter()
        .filter(|c| {
            c.writes
                && !c.kind.is_category()
                && c.action != ChannelAction::SyncedToParent
                && c.action != ChannelAction::UnverifiedOnly
                && c.action != ChannelAction::ExpiredOnly
        })
        .collect();

    if !desynced_orphans.is_empty() {
        out.push_str("## Standalone channels that change\n\n");
        for ch in &desynced_orphans {
            let roles_added: Vec<String> = ch
                .allow_roles
                .iter()
                .map(|&r| format!("+{} view", role_label(r, cfg)))
                .collect();
            let roles_str = if roles_added.is_empty() {
                String::new()
            } else {
                format!(", {}", roles_added.join(", "))
            };
            out.push_str(&format!(
                "- #{}: {} ({}{})\n",
                ch.name,
                ch.action.label(),
                "-@everyone view",
                roles_str,
            ));
        }
        out.push('\n');
    }

    // --- Section 3: restricted channels (unverified-only and dues-expired-only) ---
    let restricted: Vec<&PlannedChannel> = plan
        .channels
        .iter()
        .filter(|c| {
            c.writes
                && matches!(
                    c.action,
                    ChannelAction::UnverifiedOnly | ChannelAction::ExpiredOnly
                )
        })
        .collect();

    if !restricted.is_empty() {
        out.push_str("## Restricted channels\n\n");
        for ch in &restricted {
            // Each restricted channel: @everyone loses view+send; the named role
            // gets view (read-only); Moderator gets view+send.
            let role_name = if let Some(&r) = ch.allow_roles.first() {
                role_label(r, cfg)
            } else {
                "(none)".into()
            };
            out.push_str(&format!(
                "- #{}: -@everyone view+send, +{} view, +{} view+send\n",
                ch.name,
                role_name,
                role_label(cfg.moderator_role, cfg),
            ));
        }
        out.push('\n');
    }

    // --- Section 4: safety list ---
    let visible = unverified_visibility(plan, cfg);
    out.push_str("## Unverified can still see:\n\n");
    if visible.is_empty() {
        out.push_str("(none)\n");
    } else {
        for name in &visible {
            out.push_str(&format!("- #{}\n", name));
        }
    }

    out
}

/// Map a role id to a human-readable label using the config's known roles,
/// falling back to `role {id}` for anything not recognised. This is a display
/// helper for bot-output strings only - not a domain operation.
fn role_label(id: DiscordRoleId, cfg: &SetupConfig) -> String {
    if id == cfg.everyone {
        "@everyone".into()
    } else if id == cfg.member_role {
        "Member".into()
    } else if id == cfg.dues_expired_role {
        "Dues Expired".into()
    } else if id == cfg.unverified_role {
        "Unverified".into()
    } else if id == cfg.moderator_role {
        "Moderator".into()
    } else {
        format!("role {}", id.0)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Utc;
    use domain::{DiscordChannelId, DiscordGuildId, DiscordRoleId, DiscordUserId};

    use crate::backends::discord::{ChannelKind, OverwriteTarget, PermOverwrite, Permissions};

    use super::super::model::SetupConfig;
    use super::super::model::{VIEW, lockdown_member};
    use super::super::plan::{ChannelAction, ChannelPlan, PlanCounts, PlannedChannel};
    use super::*;

    fn ow(target: OverwriteTarget, allow: Permissions, deny: Permissions) -> PermOverwrite {
        PermOverwrite {
            target,
            allow,
            deny,
        }
    }

    fn cfg() -> SetupConfig {
        SetupConfig {
            everyone: DiscordRoleId(1),
            member_role: DiscordRoleId(10),
            dues_expired_role: DiscordRoleId(11),
            unverified_role: DiscordRoleId(12),
            moderator_role: DiscordRoleId(40),
            bot_user: DiscordUserId(99),
            unverified_channels: BTreeSet::new(),
            dues_expired_channels: BTreeSet::new(),
            exclude_channels: BTreeSet::new(),
        }
    }

    fn empty_plan() -> ChannelPlan {
        ChannelPlan {
            guild_id: DiscordGuildId(1),
            channels: vec![],
            counts: PlanCounts::default(),
            resolved_at: Utc::now(),
            everyone_base_view: true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn planned(
        id: u64,
        name: &str,
        kind: ChannelKind,
        parent_id: Option<u64>,
        action: ChannelAction,
        final_overwrites: Vec<PermOverwrite>,
        allow_roles: Vec<DiscordRoleId>,
        writes: bool,
    ) -> PlannedChannel {
        PlannedChannel {
            id: DiscordChannelId(id),
            name: name.into(),
            kind,
            parent_id: parent_id.map(DiscordChannelId),
            position: 0,
            action,
            everyone_view_before: true,
            current_overwrites: vec![],
            final_overwrites,
            allow_roles,
            writes,
        }
    }

    // --- summary_lines_counts_match_plan ---

    #[test]
    fn summary_lines_counts_match_plan() {
        let mut plan = empty_plan();
        // Use distinct values so each assertion is unambiguous.
        plan.counts.writes = 7;
        plan.counts.member_only = 3;
        plan.counts.synced_children = 5;
        plan.counts.unverified_only = 1;
        plan.counts.expired_only = 2;
        plan.counts.unchanged = 4;
        plan.counts.no_op = 6;
        plan.counts.excluded = 8;

        let rows = summary_lines(&plan);
        assert_eq!(rows.len(), 6, "summary must have exactly six rows");

        let find = |label: &str| {
            rows.iter()
                .find(|(l, _)| l == label)
                .unwrap_or_else(|| panic!("must have '{}' row", label))
                .1
                .clone()
        };

        // Row 1: counts.writes
        assert_eq!(find("Will write"), "7", "Will write = counts.writes");

        // Row 2: counts.member_only
        assert_eq!(
            find("Categories -> Member-only"),
            "3",
            "member-only = counts.member_only"
        );

        // Row 3: counts.synced_children
        assert_eq!(
            find("Synced children following"),
            "5",
            "synced children = counts.synced_children"
        );

        // Row 4: counts.unverified_only + counts.expired_only  (1 + 2 = 3)
        assert_eq!(
            find("Restricted (unverified/dues)"),
            "3",
            "restricted = unverified_only + expired_only"
        );

        // Row 5: counts.unchanged + counts.no_op  (4 + 6 = 10)
        assert_eq!(
            find("Left unchanged"),
            "10",
            "unchanged = counts.unchanged + counts.no_op"
        );

        // Row 6: counts.excluded
        assert_eq!(find("Excluded"), "8", "excluded = counts.excluded");
    }

    // --- detail_lists_categories_and_orphans_but_not_synced_children ---

    #[test]
    fn detail_lists_categories_and_orphans_but_not_synced_children() {
        let cfg = cfg();
        // A category that changes
        let cat_ows = lockdown_member(&[], cfg.everyone, cfg.member_role);
        let cat = planned(
            100,
            "general-category",
            ChannelKind::Category,
            None,
            ChannelAction::MemberOnly,
            cat_ows.clone(),
            vec![cfg.member_role],
            true,
        );

        // A synced child - must NOT appear as its own line
        let synced_child = planned(
            101,
            "synced-child-channel",
            ChannelKind::Text,
            Some(100),
            ChannelAction::SyncedToParent,
            cat_ows.clone(),
            vec![cfg.member_role],
            true,
        );

        // A desynced orphan that changes directly - must appear
        let orphan_ows = lockdown_member(&[], cfg.everyone, cfg.member_role);
        let orphan = planned(
            102,
            "orphan-channel",
            ChannelKind::Text,
            None,
            ChannelAction::MemberOnly,
            orphan_ows,
            vec![cfg.member_role],
            true,
        );

        let mut plan = empty_plan();
        plan.counts.member_only = 2;
        plan.counts.synced_children = 1;
        plan.counts.writes = 3;
        plan.channels = vec![cat, synced_child, orphan];

        let md = detail_markdown(&plan, &cfg);

        // Category line must appear
        assert!(
            md.contains("general-category"),
            "markdown must mention the category name"
        );

        // "1 synced child follows" note must appear (singular, grammatical)
        assert!(
            md.contains("1 synced child follows"),
            "markdown must say '1 synced child follows' for a single synced child"
        );

        // Orphan must appear
        assert!(
            md.contains("orphan-channel"),
            "markdown must mention the orphan channel"
        );

        // Synced child must NOT appear as a standalone line. We check that
        // "synced-child-channel" only appears inside the category's synced-count
        // note and NOT as a named bullet.
        assert!(
            !md.contains("synced-child-channel"),
            "synced child must not appear as a standalone line"
        );
    }

    // --- detail_includes_unverified_visibility_section ---

    #[test]
    fn detail_includes_unverified_visibility_section() {
        let cfg = cfg();

        // Build a channel that the Unverified role can still view - no @everyone
        // deny, and an explicit Unverified allow, so role_can_view returns true.
        let uv_ows = vec![ow(
            OverwriteTarget::Role(cfg.unverified_role),
            VIEW,
            Permissions::empty(),
        )];
        let visible_chan = planned(
            200,
            "welcome-channel",
            ChannelKind::Text,
            None,
            ChannelAction::UnverifiedOnly,
            uv_ows,
            vec![cfg.unverified_role],
            true,
        );

        let mut plan = empty_plan();
        plan.counts.unverified_only = 1;
        plan.counts.writes = 1;
        plan.channels = vec![visible_chan];

        let md = detail_markdown(&plan, &cfg);

        // The safety section header must be present
        assert!(
            md.contains("## Unverified can still see:"),
            "markdown must have the safety section header"
        );

        // The channel visible to Unverified must be listed
        assert!(
            md.contains("welcome-channel"),
            "markdown must list the channel Unverified can still see"
        );
    }
}
