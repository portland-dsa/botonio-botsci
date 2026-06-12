//! Pure builder for the help navigator's embeds, one per topic.

use serenity::all::CreateEmbed;

/// Help topics. The moderator topic is only ever offered to moderators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    GettingVerified,
    MyMembership,
    ForModerators,
}

impl Topic {
    pub fn label(self) -> &'static str {
        match self {
            Topic::GettingVerified => "Getting verified",
            Topic::MyMembership => "My membership",
            Topic::ForModerators => "For moderators",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Topic::GettingVerified => "getting_verified",
            Topic::MyMembership => "my_membership",
            Topic::ForModerators => "for_moderators",
        }
    }
    pub fn from_id(s: &str) -> Option<Self> {
        [
            Topic::GettingVerified,
            Topic::MyMembership,
            Topic::ForModerators,
        ]
        .into_iter()
        .find(|t| t.id() == s)
    }
}

/// The topics a given invoker may see - the moderator topic only when `is_moderator`.
pub fn topics_for(is_moderator: bool) -> Vec<Topic> {
    let mut t = vec![Topic::GettingVerified, Topic::MyMembership];
    if is_moderator {
        t.push(Topic::ForModerators);
    }
    t
}

/// Build the embed for one topic.
pub fn help_embed(topic: Topic, accent: u32) -> CreateEmbed {
    let (title, body) = match topic {
        Topic::GettingVerified => (
            "📋 Help · Getting verified",
            "If I don't recognise you yet, head to the verification channel and tap **Verify**.",
        ),
        Topic::MyMembership => (
            "📋 Help · My membership",
            "**/membership-card** - your status, dues, and renewal date.\nOr right-click your name → *Apps* → **Membership Card**.",
        ),
        Topic::ForModerators => (
            "📋 Help · For moderators",
            "Moderator lookup tools are coming soon - you'll be able to pull up any current member's card from here.",
        ),
    };
    CreateEmbed::new()
        .title(title)
        .description(body)
        .colour(accent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_moderators_never_see_the_moderator_topic() {
        assert_eq!(
            topics_for(false),
            vec![Topic::GettingVerified, Topic::MyMembership]
        );
        assert!(topics_for(true).contains(&Topic::ForModerators));
    }

    #[test]
    fn topic_id_round_trips() {
        for t in [
            Topic::GettingVerified,
            Topic::MyMembership,
            Topic::ForModerators,
        ] {
            assert_eq!(Topic::from_id(t.id()), Some(t));
        }
        assert_eq!(Topic::from_id("nope"), None);
    }
}
