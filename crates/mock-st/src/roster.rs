//! Parse the `SOLIDARITY_TECH_MOCK_PERSONAS` map and build the served roster.

use chrono::NaiveDate;
use serde_json::Value;

use crate::persona::Persona;

/// Parse `"111=good_standing,222=lapsed"` into `(discord_user_id, persona)` pairs.
/// A blank entry is ignored; a bad id or unknown persona is warned and skipped, so
/// one typo never sinks the whole map.
pub fn parse_map(raw: &str) -> Vec<(u64, Persona)> {
    let mut out = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let Some((id, name)) = entry.split_once('=') else {
            tracing::warn!(entry, "mock persona map: entry has no '='");
            continue;
        };
        let Ok(discord_id) = id.trim().parse::<u64>() else {
            tracing::warn!(entry, "mock persona map: discord id is not a number");
            continue;
        };
        let Some(persona) = Persona::parse(name) else {
            tracing::warn!(entry, "mock persona map: unknown persona name");
            continue;
        };
        out.push((discord_id, persona));
    }
    out
}

/// Build the served `/users` records from an already-parsed persona map, dating
/// date-relative personas against `today`. Solidarity Tech ids are assigned
/// sequentially. The server calls this per request, so dates stay current as a
/// long-lived staging server ages rather than freezing at startup.
pub fn records(map: &[(u64, Persona)], today: NaiveDate) -> Vec<Value> {
    map.iter()
        .enumerate()
        .map(|(i, &(discord_id, persona))| persona.user_json(i as u64 + 1, discord_id, today))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_skips_bad_entries() {
        let map = parse_map("111=good_standing, 222=lapsed,,333=bogus,nope=amber");
        assert_eq!(
            map,
            vec![(111, Persona::GoodStanding), (222, Persona::Lapsed)]
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
}
