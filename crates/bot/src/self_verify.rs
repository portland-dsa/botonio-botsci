//! Self-service verification: the standing-button + modal flow that lets a member
//! verify themselves against Solidarity Tech with no moderator approval. This file
//! holds the pure name double-check; the gateway interaction handler is below it.

// allow(dead_code): the pure name double-check below has no in-binary caller of its own -
// the interaction handler that uses it is wired in the bot's gateway layer.
#![allow(dead_code)]

/// Whether the name a member typed lines up with their Solidarity Tech record.
#[derive(Debug, PartialEq, Eq)]
pub enum NameCheck {
    /// First name and last initial both line up.
    Match,
    /// At least one does not - the log post is flagged, but the grant still stands.
    Mismatch,
    /// The record has no name to compare against; not a mismatch.
    Unchecked,
}

/// Compare a submitted first name + last initial against the record's full name.
///
/// Deliberately blunt - no fuzzy or nickname matching. The first whitespace token
/// of the record name must equal the submitted first name (case-insensitive), and
/// the first character of the record name's last token must equal the submitted
/// last initial (case-insensitive). A record with no usable name is
/// [`Unchecked`](NameCheck::Unchecked), never a mismatch.
pub fn name_check(record_name: Option<&str>, first: &str, last_initial: &str) -> NameCheck {
    let Some(name) = record_name else {
        return NameCheck::Unchecked;
    };
    let mut tokens = name.split_whitespace();
    let Some(rec_first) = tokens.next() else {
        return NameCheck::Unchecked;
    };
    let rec_last = name.split_whitespace().last().unwrap_or(rec_first);

    let first_ok = rec_first.eq_ignore_ascii_case(first.trim());
    let last_ok = match (last_initial.trim().chars().next(), rec_last.chars().next()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(&b),
        _ => false,
    };
    if first_ok && last_ok {
        NameCheck::Match
    } else {
        NameCheck::Mismatch
    }
}

#[cfg(test)]
mod name_check_tests {
    use super::*;

    #[test]
    fn matches_first_and_last_initial_case_insensitively() {
        assert_eq!(
            name_check(Some("Rosy Rascal"), "rosy", "R"),
            NameCheck::Match
        );
        assert_eq!(
            name_check(Some("rosy rascal"), " ROSY ", "r"),
            NameCheck::Match
        );
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", "R."),
            NameCheck::Match
        );
    }

    #[test]
    fn first_name_or_initial_off_is_a_mismatch() {
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Shadow", "R"),
            NameCheck::Mismatch
        );
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", "Z"),
            NameCheck::Mismatch
        );
    }

    #[test]
    fn an_absent_record_name_is_unchecked() {
        assert_eq!(name_check(None, "Rosy", "R"), NameCheck::Unchecked);
        assert_eq!(name_check(Some("   "), "Rosy", "R"), NameCheck::Unchecked);
    }

    #[test]
    fn a_single_token_record_name_compares_first_against_last() {
        // "Cher" -> first and last token are the same; first "Cher", initial "C".
        assert_eq!(name_check(Some("Cher"), "Cher", "C"), NameCheck::Match);
    }

    #[test]
    fn multi_token_name_uses_the_last_token_for_the_initial() {
        // "Amy Rose Hedgehog" -> first "Amy", last token "Hedgehog"; middle token "Rose" is ignored.
        assert_eq!(
            name_check(Some("Amy Rose Hedgehog"), "Amy", "H"),
            NameCheck::Match
        );
        // "R" matches the middle token "Rose", not the last - must be a mismatch.
        assert_eq!(
            name_check(Some("Amy Rose Hedgehog"), "Amy", "R"),
            NameCheck::Mismatch
        );
    }

    #[test]
    fn empty_or_blank_last_initial_is_a_mismatch() {
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", ""),
            NameCheck::Mismatch
        );
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", "   "),
            NameCheck::Mismatch
        );
    }
}
