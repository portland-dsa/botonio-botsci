//! The three membership status tiers the bot manages on the guild.

/// The three status roles the bot manages on the guild.
///
/// Every member ends up holding exactly one of these once the bot has set their
/// status role. This enum is the shared membership vocabulary; the
/// Discord-specific bits (the `DISCORD_ROLE_*_ID` override names) live with the
/// Discord backend, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Role {
    /// Active, dues-current member in good standing.
    Member,
    /// Member whose dues have lapsed.
    DuesExpired,
    /// New or unmatched user awaiting verification.
    Unverified,
}

impl Role {
    /// All three roles, highest-priority first.
    ///
    /// The order is load-bearing: tie-breaking when a member erroneously holds
    /// more than one status role relies on it, so `Member` wins over
    /// `DuesExpired` wins over `Unverified`. Reordering changes that tie-break.
    pub const ALL: [Role; 3] = [Role::Member, Role::DuesExpired, Role::Unverified];

    /// The exact role name as it must appear on the guild.
    ///
    /// Matched case-sensitively against the guild's role names when no env-var
    /// override supplies the id directly.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Member => "Member",
            Role::DuesExpired => "Dues Expired",
            Role::Unverified => "Unverified",
        }
    }
}
