//! Bot configuration, read once from the environment at startup - the bot's single
//! point of environment access, mirroring `Clients::from_env`.

use std::time::Duration;

use domain::DiscordGuildId;
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
    /// HMAC key for hashing the ids written to the audit log. Loaded from the
    /// credential store or `AUDIT_HASH_KEY`.
    pub audit_hash_key: SecretString,
    /// Names the audit HMAC key, stored beside each row so the key can be rotated.
    pub audit_key_id: String,
    /// Per-moderator ceiling on member-card lookups of *other* members, per minute.
    pub lookup_rate_per_min: u32,
    /// Per-member ceiling on self-service verification attempts, per minute.
    /// Generous - it bounds abuse and Solidarity Tech read volume, not real members.
    pub self_verify_rate_per_min: u32,
    /// How often the scheduled scan runs (when enabled in /setup).
    pub scan_interval: Duration,
    /// Tripwire: abort a pass when demotions reach this percentage of scanned members.
    pub scan_tripwire_percent: u8,
    /// Tripwire: ...and reach this absolute floor (so small guilds don't trip on churn).
    pub scan_tripwire_floor: usize,
    /// Pause between role writes during the scan's apply phase.
    pub scan_pace: Duration,
}

impl BotConfig {
    /// The home guild as a typed id. The bot serves exactly one guild; this is the wrapped form
    /// the command and sweep call sites use instead of re-wrapping `guild_id` at each one.
    pub fn guild(&self) -> DiscordGuildId {
        DiscordGuildId(self.guild_id)
    }
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
        let accent_color = match std::env::var("BOT_ACCENT_COLOR") {
            Ok(s) => parse_accent(&s).map_err(|e| ConfigError::Invalid("BOT_ACCENT_COLOR", e))?,
            Err(_) => 0xc8_10_2e, // placeholder DSA red
        };
        let refresh_interval =
            parse_refresh_secs(std::env::var("BOT_INDEX_REFRESH_SECS").ok().as_deref());
        let discord_list_id = read("SOLIDARITY_TECH_DISCORD_LIST_ID")?;
        let db_runtime_dsn = read("DB_RUNTIME_DSN")?;
        let audit_hash_key = SecretString::from(
            engine::backends::from_credstore_or_env("audit_hash_key", "AUDIT_HASH_KEY")
                .ok_or(ConfigError::Missing("AUDIT_HASH_KEY"))?,
        );
        let audit_key_id = std::env::var("AUDIT_KEY_ID").unwrap_or_else(|_| "v1".to_owned());
        let lookup_rate_per_min =
            parse_lookup_rate(std::env::var("BOT_LOOKUP_RATE_PER_MIN").ok().as_deref());
        let self_verify_rate_per_min = parse_self_verify_rate(
            std::env::var("BOT_SELF_VERIFY_RATE_PER_MIN")
                .ok()
                .as_deref(),
        );
        let scan_interval =
            parse_scan_interval(std::env::var("BOT_SCAN_INTERVAL_SECS").ok().as_deref());
        let scan_tripwire_percent =
            parse_tripwire_percent(std::env::var("BOT_SCAN_TRIPWIRE_PERCENT").ok().as_deref());
        let scan_tripwire_floor =
            parse_tripwire_floor(std::env::var("BOT_SCAN_TRIPWIRE_FLOOR").ok().as_deref());
        let scan_pace = parse_scan_pace(std::env::var("BOT_SCAN_PACE_MS").ok().as_deref());
        Ok(Self {
            token,
            guild_id,
            accent_color,
            refresh_interval,
            discord_list_id,
            db_runtime_dsn,
            audit_hash_key,
            audit_key_id,
            lookup_rate_per_min,
            self_verify_rate_per_min,
            scan_interval,
            scan_tripwire_percent,
            scan_tripwire_floor,
            scan_pace,
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

/// Default per-moderator lookup ceiling when `BOT_LOOKUP_RATE_PER_MIN` is
/// unset or invalid.
const DEFAULT_LOOKUP_RATE: u32 = 10;

/// Default per-member self-verify ceiling when `BOT_SELF_VERIFY_RATE_PER_MIN` is
/// unset or invalid.
const DEFAULT_SELF_VERIFY_RATE: u32 = 5;

fn parse_self_verify_rate(raw: Option<&str>) -> u32 {
    raw.and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_SELF_VERIFY_RATE)
}

/// Parse the per-minute lookup ceiling, falling back to [`DEFAULT_LOOKUP_RATE`].
/// A `0` is clamped up to 1: a ceiling of zero would refuse every moderator lookup,
/// which is never the intent of setting the variable.
fn parse_lookup_rate(raw: Option<&str>) -> u32 {
    raw.and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_LOOKUP_RATE)
        .max(1)
}

/// Parse a seconds string into a `Duration`, falling back to [`DEFAULT_REFRESH`].
fn parse_refresh_secs(raw: Option<&str>) -> Duration {
    raw.and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_REFRESH)
        .max(MIN_REFRESH)
}

/// Default scan cadence (4h) - matches Solidarity Tech's own ~4-hour record sync; a
/// quicker manual edit is covered by /refresh-cache.
const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_secs(14_400);
/// Floor on the scan cadence (`interval(0)` panics; sub-5-minute scans waste work).
const MIN_SCAN_INTERVAL: Duration = Duration::from_secs(300);
const DEFAULT_TRIPWIRE_PERCENT: u8 = 20;
const DEFAULT_TRIPWIRE_FLOOR: usize = 5;
const DEFAULT_SCAN_PACE: Duration = Duration::from_millis(250);

fn parse_scan_interval(raw: Option<&str>) -> Duration {
    raw.and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_SCAN_INTERVAL)
        .max(MIN_SCAN_INTERVAL)
}

/// Parse the tripwire percentage, clamped to 1..=100 (0 would abort on a single demotion;
/// >100 can never trip).
fn parse_tripwire_percent(raw: Option<&str>) -> u8 {
    raw.and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TRIPWIRE_PERCENT)
        .clamp(1, 100)
}

fn parse_tripwire_floor(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TRIPWIRE_FLOOR)
}

fn parse_scan_pace(raw: Option<&str>) -> Duration {
    raw.and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_SCAN_PACE)
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

    #[test]
    fn self_verify_rate_defaults_and_rejects_zero() {
        assert_eq!(parse_self_verify_rate(None), DEFAULT_SELF_VERIFY_RATE);
        assert_eq!(parse_self_verify_rate(Some("0")), DEFAULT_SELF_VERIFY_RATE);
        assert_eq!(parse_self_verify_rate(Some("20")), 20);
    }

    #[test]
    fn lookup_rate_falls_back_and_clamps() {
        assert_eq!(parse_lookup_rate(None), DEFAULT_LOOKUP_RATE);
        assert_eq!(parse_lookup_rate(Some("25")), 25);
        assert_eq!(parse_lookup_rate(Some("xyz")), DEFAULT_LOOKUP_RATE);
        // Zero would refuse every lookup; clamp up to 1.
        assert_eq!(parse_lookup_rate(Some("0")), 1);
    }

    #[test]
    fn scan_interval_defaults_and_clamps() {
        assert_eq!(parse_scan_interval(None), DEFAULT_SCAN_INTERVAL);
        assert_eq!(
            parse_scan_interval(Some("28800")),
            Duration::from_secs(28_800)
        );
        assert_eq!(parse_scan_interval(Some("0")), MIN_SCAN_INTERVAL);
        assert_eq!(parse_scan_interval(Some("xyz")), DEFAULT_SCAN_INTERVAL);
    }

    #[test]
    fn tripwire_percent_defaults_and_clamps() {
        assert_eq!(parse_tripwire_percent(None), DEFAULT_TRIPWIRE_PERCENT);
        assert_eq!(parse_tripwire_percent(Some("35")), 35);
        assert_eq!(parse_tripwire_percent(Some("0")), 1);
        assert_eq!(parse_tripwire_percent(Some("250")), 100);
    }

    #[test]
    fn tripwire_floor_and_pace_default() {
        assert_eq!(parse_tripwire_floor(None), DEFAULT_TRIPWIRE_FLOOR);
        assert_eq!(parse_tripwire_floor(Some("10")), 10);
        assert_eq!(parse_scan_pace(None), DEFAULT_SCAN_PACE);
        assert_eq!(parse_scan_pace(Some("500")), Duration::from_millis(500));
    }
}
