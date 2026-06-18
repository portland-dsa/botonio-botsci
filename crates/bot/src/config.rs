//! Bot configuration, read once from the environment at startup - the bot's single
//! point of environment access, mirroring `Clients::from_env`.

use std::time::Duration;

use secrecy::SecretString;

/// Default index refresh cadence when `BOT_INDEX_REFRESH_SECS` is unset/invalid.
pub const DEFAULT_REFRESH: Duration = Duration::from_secs(900);

/// Floor on the refresh cadence. `BOT_INDEX_REFRESH_SECS=0` is clamped up to this:
/// `tokio::time::interval(0)` panics, and refreshing more than once a minute would
/// hammer Solidarity Tech for no gain.
const MIN_REFRESH: Duration = Duration::from_secs(60);

/// Validated bot configuration.
pub struct BotConfig {
    /// The Discord bot token, for the gateway connection.
    pub token: SecretString,
    /// The Discord guild (server) the bot serves. Commands are registered only to this
    /// guild, and every invocation is re-checked against the guild allow-list.
    pub guild_id: u64,
    /// The role id that marks a moderator (gates the help "For moderators" topic).
    pub moderator_role_id: u64,
    /// Embed accent colour as a 0xRRGGBB integer.
    pub accent_color: u32,
    /// How often to rebuild the member index.
    pub refresh_interval: Duration,
    /// The Solidarity Tech user-list id whose members form the bot's index
    /// (where the list is pre-filtered to members with a Discord handle/id).
    pub discord_list_id: String,
    /// The runtime database connection string (peer auth over the Unix socket, no
    /// password): `postgres:///bot_<env>?host=/var/run/postgresql&user=bot_<env>_app`.
    /// Local development supplies it via `.env`; production via the unit's `Environment=`.
    pub db_runtime_dsn: String,
}

/// Everything that can go wrong reading config.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing env var: {0}")]
    Missing(&'static str),
    #[error("invalid {0}: {1}")]
    Invalid(&'static str, String),
}

impl BotConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        // The gateway and the HTTP backend share one Discord bot token; both read it from
        // `DISCORD_BOT_TOKEN` (credential `discord_bot_token`), so the unit ships one secret.
        let token = SecretString::from(
            engine::backends::from_credstore_or_env("discord_bot_token", "DISCORD_BOT_TOKEN")
                .ok_or(ConfigError::Missing("DISCORD_BOT_TOKEN"))?,
        );
        let guild_id = read("DISCORD_GUILD_ID")?
            .parse()
            .map_err(|_| ConfigError::Invalid("DISCORD_GUILD_ID", "not a u64".into()))?;
        let moderator_role_id = read("DISCORD_MODERATOR_ROLE_ID")?
            .parse()
            .map_err(|_| ConfigError::Invalid("DISCORD_MODERATOR_ROLE_ID", "not a u64".into()))?;
        let accent_color = match std::env::var("BOT_ACCENT_COLOR") {
            Ok(s) => parse_accent(&s).map_err(|e| ConfigError::Invalid("BOT_ACCENT_COLOR", e))?,
            Err(_) => 0xc8_10_2e, // placeholder DSA red
        };
        let refresh_interval =
            parse_refresh_secs(std::env::var("BOT_INDEX_REFRESH_SECS").ok().as_deref());
        let discord_list_id = read("SOLIDARITY_TECH_DISCORD_LIST_ID")?;
        let db_runtime_dsn = read("DB_RUNTIME_DSN")?;
        Ok(Self {
            token,
            guild_id,
            moderator_role_id,
            accent_color,
            refresh_interval,
            discord_list_id,
            db_runtime_dsn,
        })
    }
}

fn read(key: &'static str) -> Result<String, ConfigError> {
    std::env::var(key).map_err(|_| ConfigError::Missing(key))
}

/// Parse `#rrggbb` or `rrggbb` into a 0xRRGGBB integer.
fn parse_accent(s: &str) -> Result<u32, String> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 {
        return Err(format!("expected 6 hex digits, got {s:?}"));
    }
    u32::from_str_radix(hex, 16).map_err(|e| e.to_string())
}

/// Parse a seconds string into a `Duration`, falling back to [`DEFAULT_REFRESH`].
fn parse_refresh_secs(raw: Option<&str>) -> Duration {
    raw.and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_REFRESH)
        .max(MIN_REFRESH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_accent_color_hex() {
        assert_eq!(parse_accent("#3ba55d").unwrap(), 0x3b_a5_5d);
        assert_eq!(parse_accent("3ba55d").unwrap(), 0x3b_a5_5d);
    }

    #[test]
    fn rejects_bad_accent_color() {
        assert!(parse_accent("nope").is_err());
    }

    #[test]
    fn refresh_interval_falls_back_to_default() {
        assert_eq!(parse_refresh_secs(None), DEFAULT_REFRESH);
        assert_eq!(
            parse_refresh_secs(Some("300")),
            std::time::Duration::from_secs(300)
        );
        // A garbage value falls back rather than panicking.
        assert_eq!(parse_refresh_secs(Some("xyz")), DEFAULT_REFRESH);
    }

    #[test]
    fn refresh_interval_clamps_to_minimum() {
        // 0 would panic `tokio::time::interval`; sub-minute values would hammer the API.
        assert_eq!(parse_refresh_secs(Some("0")), MIN_REFRESH);
        assert_eq!(parse_refresh_secs(Some("5")), MIN_REFRESH);
    }
}
