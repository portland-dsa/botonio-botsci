//! Pure builder for the membership-card embed. No I/O; takes everything it renders.

use chrono::NaiveDate;
use serenity::all::{CreateEmbed, CreateEmbedFooter};

use domain::{MigsStatus, Role};
use engine::store::{GraceOverride, MemberRecord, OverrideRecord};

pub const COLOR_GREEN: u32 = 0x3b_a5_5d;
pub const COLOR_AMBER: u32 = 0xfa_a6_1a;
pub const COLOR_RED: u32 = 0xed_42_45;

/// Days-before-expiry that flips the card amber (matches the planned renewal-reminder window).
const SOON_DAYS: i64 = 30;

/// Build the self-card embed. `display_name` is the Discord nickname/global name;
/// `pronouns` is appended to the title only when present. `today` is injected for
/// deterministic colour tests. `grace` adds a banner field when the member is under
/// a moderator grace window - shown alongside (not replacing) the Solidarity Tech
/// status. `show_reason` gates the mod-supplied grace reason: it is drawn only on a
/// moderator-facing view (a lookup of another member), never on the member's own card.
pub fn membership_card(
    rec: &MemberRecord,
    display_name: &str,
    pronouns: Option<&str>,
    today: NaiveDate,
    grace: Option<&GraceOverride>,
    show_reason: bool,
) -> CreateEmbed {
    let title = match pronouns {
        Some(p) => format!("{display_name} · {p}"),
        None => display_name.to_string(),
    };
    let color = card_color(rec, today);
    let status_line = status_line(rec, today);

    let mut embed = CreateEmbed::new()
        .title(title)
        .colour(color)
        .description(status_line);

    if let Some(name) = &rec.full_name {
        embed = embed.field("Name", name, false);
    }
    embed = embed.field("Role", role_label(rec), false);
    if let Some(j) = rec.join_date {
        embed = embed.field("Join Date", j.format("%b %-d, %Y").to_string(), true);
    }
    if let Some(x) = rec.expires {
        embed = embed.field("Expires", x.format("%b %-d, %Y").to_string(), true);
    }
    embed = embed.field("Email", rec.email.as_str(), false);
    if let Some(g) = grace {
        embed = embed.field("Grace", grace_banner_value(g, show_reason), false);
    }
    embed.footer(serenity::all::CreateEmbedFooter::new(
        "Pulled from Solidarity Tech · PDX DSA",
    ))
}

/// Format the grace banner field value: "On grace until <date>, granted by <@id>"
/// with an optional reason appended on the next line when `show_reason` is true.
/// The base line (date + grantor) is always shown; the reason is moderator-only context.
fn grace_banner_value(g: &GraceOverride, show_reason: bool) -> String {
    let date = g.until.format("%b %-d, %Y");
    let base = format!("On grace until {date}, granted by <@{}>", g.granted_by.0);
    if show_reason {
        match g.reason.as_deref() {
            Some(r) if !r.is_empty() => format!("{base}\n{r}"),
            _ => base,
        }
    } else {
        base
    }
}

/// Build the card for a manually-verified member - one Solidarity Tech does not know,
/// whom a moderator hand-approved. Pure: it draws only the approval stamp and the
/// member's display name, with no Solidarity Tech fields (there are none). The
/// approving moderator renders as a `<@id>` mention, which Discord shows as their live
/// handle without an extra lookup and without storing a handle that could go stale.
///
/// `show_note` gates the optional moderator-supplied reason: it is drawn only on a
/// moderator-facing view (a lookup of another member), never on the member's own card.
pub fn override_card(display_name: &str, stamp: &OverrideRecord, show_note: bool) -> CreateEmbed {
    let approve_date = stamp
        .approved_at
        .date_naive()
        .format("%b %-d, %Y")
        .to_string();
    let approver = format!("<@{}>", stamp.approved_by.0);
    let mut embed = CreateEmbed::new()
        .title(display_name.to_string())
        .colour(COLOR_GREEN)
        .description("\u{26a0}\u{fe0f} \u{2611}\u{fe0f} Manually Verified as Member")
        .field("Role", "Member; Manual Verify", false);
    // The reason is moderator-only context: shown when a moderator looks up another
    // member, hidden on the member's own card.
    if show_note
        && let Some(note) = stamp.note.as_deref()
        && !note.is_empty()
    {
        embed = embed.field("Reason", note, false);
    }
    embed
        .field("Approve Date", approve_date, true)
        .field("Approving Mod", approver, true)
        .footer(CreateEmbedFooter::new(
            "Manually verified by a moderator \u{b7} PDX DSA",
        ))
}

fn role_label(rec: &MemberRecord) -> &'static str {
    Role::try_from(rec.membership())
        .map(Role::as_str)
        .unwrap_or("Unknown")
}

// The membership *standing* is authoritative for the status line and colour, so they
// agree with the "Role" field (which derives from standing alone). A good-standing
// member whose `xdate` is past is NOT shown as "Expired"/red - that contradiction is
// exactly what we avoid; the future `Verification Override` mode is how a mod corrects
// a member whose records are wrong. Expiry only adds a friendly "renewing soon"
// heads-up while a *future* xdate sits inside the reminder window.
fn status_line(rec: &MemberRecord, today: NaiveDate) -> String {
    match rec.standing {
        Some(MigsStatus::MemberInGoodStanding) => match rec.expires {
            Some(x) if x >= today && (x - today).num_days() <= SOON_DAYS => {
                let days = (x - today).num_days();
                match days {
                    0 => "⏳ Member in Good Standing - expires today".to_string(),
                    1 => "⏳ Member in Good Standing - expires in 1 day".to_string(),
                    n => format!("⏳ Member in Good Standing - expires in {n} days"),
                }
            }
            _ => "✅ Member in Good Standing".to_string(),
        },
        Some(MigsStatus::Lapsed) => "⚠️ Lapsed".to_string(),
        None => "❔ Status unknown".to_string(),
    }
}

fn card_color(rec: &MemberRecord, today: NaiveDate) -> u32 {
    match rec.standing {
        Some(MigsStatus::MemberInGoodStanding) => match rec.expires {
            // Amber only as a heads-up for a *future* xdate in the reminder window; a
            // past xdate does not turn a good-standing member red - standing prevails.
            Some(x) if x >= today && (x - today).num_days() <= SOON_DAYS => COLOR_AMBER,
            _ => COLOR_GREEN,
        },
        Some(MigsStatus::Lapsed) | None => COLOR_RED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use domain::MigsStatus;
    use engine::backends::util::{Email, StUserId};
    use engine::store::MemberRecord;

    fn rec(standing: Option<MigsStatus>, expires: Option<NaiveDate>) -> MemberRecord {
        MemberRecord {
            st_user_id: StUserId("st-card".into()),
            discord_user_id: None,
            discord_handle: None,
            email: Email("a@b.test".into()),
            full_name: Some("Zoop Goop".into()),
            standing,
            join_date: NaiveDate::from_ymd_opt(2021, 3, 1),
            expires,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        }
    }

    fn json(e: serenity::all::CreateEmbed) -> serde_json::Value {
        serde_json::to_value(&e).unwrap()
    }

    /// Pull the value of the named field out of an embed's serialized `fields` array.
    fn field_value(v: &serde_json::Value, name: &str) -> Option<String> {
        v["fields"]
            .as_array()?
            .iter()
            .find(|f| f["name"] == name)
            .map(|f| f["value"].as_str().unwrap().to_string())
    }

    #[test]
    fn good_standing_far_off_is_green() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let e = membership_card(
            &rec(
                Some(MigsStatus::MemberInGoodStanding),
                NaiveDate::from_ymd_opt(2026, 12, 31),
            ),
            "rose",
            Some("she/her"),
            today,
            None,
            false,
        );
        assert_eq!(json(e)["color"].as_u64(), Some(COLOR_GREEN as u64));
    }

    #[test]
    fn expiring_soon_is_amber() {
        let today = NaiveDate::from_ymd_opt(2026, 12, 10).unwrap();
        let e = membership_card(
            &rec(
                Some(MigsStatus::MemberInGoodStanding),
                NaiveDate::from_ymd_opt(2026, 12, 31),
            ),
            "rose",
            None,
            today,
            None,
            false,
        );
        assert_eq!(json(e)["color"].as_u64(), Some(COLOR_AMBER as u64));
    }

    #[test]
    fn lapsed_is_red() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let e = membership_card(
            &rec(Some(MigsStatus::Lapsed), None),
            "rose",
            None,
            today,
            None,
            false,
        );
        assert_eq!(json(e)["color"].as_u64(), Some(COLOR_RED as u64));
    }

    #[test]
    fn good_standing_with_past_xdate_stays_member_not_expired() {
        // Standing is authoritative: a past xdate must NOT flip a good-standing member
        // to red/"Expired" - that would contradict the "Role" field, which reads "Member".
        let today = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let past = NaiveDate::from_ymd_opt(2026, 1, 1);
        let e = membership_card(
            &rec(Some(MigsStatus::MemberInGoodStanding), past),
            "rose",
            None,
            today,
            None,
            false,
        );
        let v = json(e);
        assert_eq!(v["color"].as_u64(), Some(COLOR_GREEN as u64));
        assert_eq!(v["description"], "✅ Member in Good Standing");
        assert_eq!(field_value(&v, "Role").as_deref(), Some("Member"));
    }

    #[test]
    fn pronouns_absent_keeps_bare_nickname() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let e = membership_card(
            &rec(Some(MigsStatus::MemberInGoodStanding), None),
            "rose",
            None,
            today,
            None,
            false,
        );
        assert_eq!(json(e)["title"], "rose");
    }

    #[test]
    fn pronouns_present_append_to_nickname() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let e = membership_card(
            &rec(Some(MigsStatus::MemberInGoodStanding), None),
            "rose",
            Some("she/her"),
            today,
            None,
            false,
        );
        assert_eq!(json(e)["title"], "rose · she/her");
    }

    #[test]
    fn role_field_uses_guild_facing_label_not_debug() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        // Lapsed standing -> role() == DuesExpired; the field must read "Dues Expired".
        let e = membership_card(
            &rec(Some(MigsStatus::Lapsed), None),
            "rose",
            None,
            today,
            None,
            false,
        );
        assert_eq!(
            field_value(&json(e), "Role").as_deref(),
            Some("Dues Expired")
        );
    }

    #[test]
    fn expires_in_one_day_is_singular() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let tomorrow = NaiveDate::from_ymd_opt(2026, 1, 2);
        let e = membership_card(
            &rec(Some(MigsStatus::MemberInGoodStanding), tomorrow),
            "rose",
            None,
            today,
            None,
            false,
        );
        assert_eq!(
            json(e)["description"],
            "⏳ Member in Good Standing - expires in 1 day"
        );
    }

    #[test]
    fn expires_today_on_the_expiry_day() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let e = membership_card(
            &rec(Some(MigsStatus::MemberInGoodStanding), Some(today)),
            "rose",
            None,
            today,
            None,
            false,
        );
        let v = json(e);
        let desc = v["description"].as_str().unwrap();
        assert!(
            desc.contains("expires today"),
            "expected 'expires today', got: {desc}"
        );
        assert!(
            !desc.contains("0 days"),
            "must not say '0 days', got: {desc}"
        );
    }

    fn stamp() -> engine::store::OverrideRecord {
        engine::store::OverrideRecord {
            approved_by: engine::backends::util::DiscordUserId(123),
            approved_at: chrono::DateTime::from_naive_utc_and_offset(
                NaiveDate::from_ymd_opt(2026, 6, 21)
                    .unwrap()
                    .and_hms_opt(12, 0, 0)
                    .unwrap(),
                chrono::Utc,
            ),
            note: None,
        }
    }

    fn stamp_with_note(note: &str) -> engine::store::OverrideRecord {
        engine::store::OverrideRecord {
            note: Some(note.to_string()),
            ..stamp()
        }
    }

    #[test]
    fn override_card_shows_manual_verify_role_and_mention() {
        let e = override_card("rose", &stamp(), false);
        let v = json(e);
        assert_eq!(v["color"].as_u64(), Some(COLOR_GREEN as u64));
        assert_eq!(
            field_value(&v, "Role").as_deref(),
            Some("Member; Manual Verify")
        );
        assert_eq!(field_value(&v, "Approving Mod").as_deref(), Some("<@123>"));
        assert_eq!(
            field_value(&v, "Approve Date").as_deref(),
            Some("Jun 21, 2026")
        );
        assert!(
            v["description"]
                .as_str()
                .unwrap()
                .contains("Manually Verified as Member")
        );
    }

    #[test]
    fn override_card_shows_reason_for_a_moderator_view() {
        let e = override_card(
            "rose",
            &stamp_with_note("vouched at the branch meeting"),
            true,
        );
        assert_eq!(
            field_value(&json(e), "Reason").as_deref(),
            Some("vouched at the branch meeting")
        );
    }

    #[test]
    fn override_card_hides_reason_on_a_self_view() {
        let e = override_card(
            "rose",
            &stamp_with_note("vouched at the branch meeting"),
            false,
        );
        assert_eq!(field_value(&json(e), "Reason"), None);
    }

    #[test]
    fn override_card_omits_reason_when_absent() {
        let e = override_card("rose", &stamp(), true);
        assert_eq!(field_value(&json(e), "Reason"), None);
    }

    #[test]
    fn override_card_dates_share_an_inline_row() {
        let e = override_card("rose", &stamp(), false);
        let v = json(e);
        let inline_of = |name: &str| {
            v["fields"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == name)
                .map(|f| f["inline"].as_bool().unwrap_or(false))
        };
        assert_eq!(inline_of("Approve Date"), Some(true));
        assert_eq!(inline_of("Approving Mod"), Some(true));
        assert_eq!(inline_of("Role"), Some(false));
    }

    #[test]
    fn very_long_name_is_not_truncated() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let long = "x".repeat(200);
        let e = membership_card(
            &rec(Some(MigsStatus::MemberInGoodStanding), None),
            &long,
            None,
            today,
            None,
            false,
        );
        assert_eq!(json(e)["title"].as_str().unwrap().len(), 200);
    }

    #[test]
    fn join_date_and_expires_share_an_inline_row() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let e = membership_card(
            &rec(
                Some(MigsStatus::MemberInGoodStanding),
                NaiveDate::from_ymd_opt(2026, 12, 31),
            ),
            "rose",
            None,
            today,
            None,
            false,
        );
        let v = json(e);
        let fields = v["fields"].as_array().unwrap();
        let inline_of = |name: &str| {
            fields
                .iter()
                .find(|f| f["name"] == name)
                .map(|f| f["inline"].as_bool().unwrap_or(false))
        };
        assert_eq!(inline_of("Join Date"), Some(true));
        assert_eq!(inline_of("Expires"), Some(true));
        assert_eq!(inline_of("Name"), Some(false));
        assert_eq!(inline_of("Role"), Some(false));
    }

    #[test]
    fn malformed_record_role_field_reads_unknown() {
        // A record with no standing (malformed) must show "Unknown" in the Role field,
        // not a misleading "Unverified" from the old shim default.
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let e = membership_card(&rec(None, None), "rose", None, today, None, false);
        assert_eq!(field_value(&json(e), "Role").as_deref(), Some("Unknown"));
    }

    #[test]
    fn grace_banner_appears_alongside_st_status() {
        use chrono::{DateTime, Utc};
        use engine::backends::util::DiscordUserId;
        let today = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let until = NaiveDate::from_ymd_opt(2026, 7, 31).unwrap();
        let grace = GraceOverride {
            until,
            granted_by: DiscordUserId(99),
            granted_at: DateTime::<Utc>::from_naive_utc_and_offset(
                today.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            reason: Some("annual hardship waiver".to_string()),
        };
        let e = membership_card(
            &rec(Some(MigsStatus::Lapsed), None),
            "amy",
            None,
            today,
            Some(&grace),
            true,
        );
        let v = json(e);
        // The ST-derived status is still present in the description.
        assert!(v["description"].as_str().unwrap().contains("Lapsed"));
        // The grace banner field exists and contains the date, the grantor mention,
        // and the reason (show_reason = true, the mod-facing path).
        let banner = field_value(&v, "Grace").expect("Grace field must be present");
        assert!(banner.contains("Jul 31, 2026"), "must show until date");
        assert!(banner.contains("<@99>"), "must mention the grantor");
        assert!(
            banner.contains("annual hardship waiver"),
            "must include the reason"
        );
    }

    #[test]
    fn grace_banner_omits_reason_when_absent() {
        use chrono::{DateTime, Utc};
        use engine::backends::util::DiscordUserId;
        let today = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let until = NaiveDate::from_ymd_opt(2026, 7, 31).unwrap();
        let grace = GraceOverride {
            until,
            granted_by: DiscordUserId(99),
            granted_at: DateTime::<Utc>::from_naive_utc_and_offset(
                today.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            reason: None,
        };
        let e = membership_card(
            &rec(Some(MigsStatus::Lapsed), None),
            "amy",
            None,
            today,
            Some(&grace),
            false,
        );
        let banner = field_value(&json(e), "Grace").expect("Grace field must be present");
        assert!(!banner.contains('\n'), "no newline when reason is absent");
    }

    #[test]
    fn grace_banner_hides_reason_on_self_card() {
        // The reason is moderator-only context: a member viewing their own card
        // (show_reason = false) must not see the reason text, even when one is present.
        // The base "On grace until ..." line must still appear.
        use chrono::{DateTime, Utc};
        use engine::backends::util::DiscordUserId;
        let today = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let until = NaiveDate::from_ymd_opt(2026, 7, 31).unwrap();
        let grace = GraceOverride {
            until,
            granted_by: DiscordUserId(99),
            granted_at: DateTime::<Utc>::from_naive_utc_and_offset(
                today.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            reason: Some("annual hardship waiver".to_string()),
        };
        let e = membership_card(
            &rec(Some(MigsStatus::Lapsed), None),
            "amy",
            None,
            today,
            Some(&grace),
            false,
        );
        let banner = field_value(&json(e), "Grace").expect("Grace field must be present");
        assert!(
            banner.contains("Jul 31, 2026"),
            "base date must still appear"
        );
        assert!(
            banner.contains("<@99>"),
            "grantor mention must still appear"
        );
        assert!(
            !banner.contains("annual hardship waiver"),
            "reason must be absent on self-card"
        );
        assert!(!banner.contains('\n'), "no newline when reason is hidden");
    }
}
