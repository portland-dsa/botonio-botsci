//! Parse the `SOLIDARITY_TECH_MOCK_PERSONAS` map and build the served roster.

use chrono::NaiveDate;
use serde_json::Value;

use crate::persona::Persona;

/// How a persona is keyed in the `SOLIDARITY_TECH_MOCK_PERSONAS` map: by a real
/// test-server Discord id, or by an email address. An email-keyed entry serves a member
/// found only by email - with no linked Discord id - for the manual verify-by-email path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MockKey {
    /// A real test-server Discord user id; stamped into the `discord-user-id` property.
    Discord(u64),
    /// An email address; stamped as the record's email, with no Discord id linked.
    Email(String),
}

/// Parse `"111=good_standing,by-email@x.test=email_verify"` into `(key, persona)` pairs.
/// The key is an [`MockKey::Email`] when it contains `@`, otherwise a numeric
/// [`MockKey::Discord`]. A blank entry is ignored; an unknown persona or a key that is
/// neither a number nor an email is warned and skipped, so one typo never sinks the
/// whole map.
pub fn parse_map(raw: &str) -> Vec<(MockKey, Persona)> {
    let mut out = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let Some((key, name)) = entry.split_once('=') else {
            tracing::warn!(entry, "mock persona map: entry has no '='");
            continue;
        };
        let Some(persona) = Persona::parse(name) else {
            tracing::warn!(entry, "mock persona map: unknown persona name");
            continue;
        };
        let key = key.trim();
        let mock_key = if key.contains('@') {
            MockKey::Email(key.to_string())
        } else if let Ok(discord_id) = key.parse::<u64>() {
            MockKey::Discord(discord_id)
        } else {
            tracing::warn!(
                entry,
                "mock persona map: key is neither a Discord id nor an email"
            );
            continue;
        };
        out.push((mock_key, persona));
    }
    out
}

/// Build the served `/users` records from an already-parsed persona map, dating
/// date-relative personas against `today`. Solidarity Tech ids are assigned
/// sequentially. The server calls this per request, so dates stay current as a
/// long-lived staging server ages rather than freezing at startup.
pub fn records(map: &[(MockKey, Persona)], today: NaiveDate) -> Vec<Value> {
    map.iter()
        .enumerate()
        .map(|(i, (key, persona))| {
            let st_id = i as u64 + 1;
            match key {
                MockKey::Discord(id) => persona.user_json(st_id, Some(*id), None, today),
                MockKey::Email(email) => persona.user_json(st_id, None, Some(email), today),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_skips_bad_entries() {
        // "333=bogus" is an unknown persona; "nope=amber" has a key that is neither a
        // number nor an email - both are skipped.
        let map = parse_map("111=good_standing, 222=lapsed,,333=bogus,nope=amber");
        assert_eq!(
            map,
            vec![
                (MockKey::Discord(111), Persona::GoodStanding),
                (MockKey::Discord(222), Persona::Lapsed)
            ]
        );
    }

    #[test]
    fn parses_an_email_keyed_entry() {
        let map = parse_map("by-email@persona.test=email_verify");
        assert_eq!(
            map,
            vec![(
                MockKey::Email("by-email@persona.test".to_string()),
                Persona::EmailVerify
            )]
        );
    }

    #[test]
    fn empty_map_is_empty_roster() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        assert!(records(&parse_map(""), today).is_empty());
    }

    #[test]
    fn records_stamp_each_id_and_assign_sequential_st_ids() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let roster = records(&parse_map("111=good_standing,222=lapsed"), today);
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0]["id"], 1);
        assert_eq!(roster[1]["id"], 2);
        assert_eq!(
            roster[0]["custom_user_properties"]["discord-user-id"],
            "111"
        );
        assert_eq!(
            roster[1]["custom_user_properties"]["discord-user-id"],
            "222"
        );
    }

    #[test]
    fn email_keyed_record_stamps_email_and_omits_discord_id() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let roster = records(&parse_map("by-email@persona.test=email_verify"), today);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0]["email"], "by-email@persona.test");
        assert!(
            roster[0]["custom_user_properties"]
                .get("discord-user-id")
                .is_none(),
            "an email-keyed record must not carry a discord-user-id"
        );
    }
}
