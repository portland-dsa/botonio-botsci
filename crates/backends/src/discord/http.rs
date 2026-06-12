//! The live `DiscordHttp` client over `serenity::http::Http`.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serenity::http::Http;
use serenity::model::guild::Role as GuildRole;
use serenity::model::id::{GuildId, RoleId, UserId};

use crate::util::{DiscordGuildId, DiscordHandle, DiscordUserId, DryRun};

use super::channels::{DiscordChannel, is_thread, project_channel};
use super::client::DiscordClient;
use super::error::DiscordError;
use super::roles::{
    DiscordMember, ManagedRole, MemberRoles, Role, RoleExt, StatusDiff, diff_status_roles,
    display_name_from, pick_current_status, role_names_for,
};

/// Reason attached to every role add/remove, so the change is legible in the
/// guild's audit log.
const AUDIT_LOG_REASON: &str = "discord-bulk-update verification";
/// Members fetched per `get_guild_members` page; 1000 is the Discord API max.
const MEMBER_PAGE_SIZE: u64 = 1000;

/// Live [`DiscordClient`], REST-only - no gateway, no cache.
///
/// Wraps `serenity::http::Http` (which handles per-bucket rate-limit retries);
/// the gateway belongs to the bot binary, not this shared write path. Role ids
/// are resolved once in [`from_env`](DiscordHttp::from_env) so trait calls never
/// look them up.
pub struct DiscordHttp {
    http: Http,
    guild_id: GuildId,
    role_ids: HashMap<Role, RoleId>,
    /// The three managed roles as resolved at construction (id, name, override
    /// provenance), kept so [`managed_roles`](DiscordClient::managed_roles) can
    /// echo them without a network round-trip. In `Role::ALL` order.
    managed: Vec<ManagedRole>,
    /// Held only to keep the token's [`SecretString`] alive (and zeroed on drop)
    /// for the client's lifetime; never read after construction and never
    /// logged. The leading underscore is intentional.
    ///
    /// [`SecretString`]: secrecy::SecretString
    _token: SecretString,
}

impl DiscordHttp {
    /// Constructs the client from the environment.
    ///
    /// Reads `DISCORD_BOT_TOKEN` (wrapped immediately in a [`SecretString`]),
    /// `DISCORD_GUILD_ID`, and the three optional `DISCORD_ROLE_*_ID` overrides.
    /// Any role without an override is resolved by name via a single
    /// `get_guild_roles` call. Returns [`DiscordError::MissingEnv`] or
    /// [`DiscordError::BadEnv`] for env problems, or
    /// [`DiscordError::RoleNotFound`] if a role name is not present on the guild.
    ///
    /// [`SecretString`]: secrecy::SecretString
    pub async fn from_env() -> Result<Self, DiscordError> {
        let token_raw =
            crate::util::secret::from_credstore_or_env("discord_bot_token", "DISCORD_BOT_TOKEN")
                .ok_or(DiscordError::MissingEnv("DISCORD_BOT_TOKEN"))?;
        let token = SecretString::from(token_raw);
        let guild_id = GuildId::new(read_env_u64("DISCORD_GUILD_ID")?);

        let http = Http::new(token.expose_secret());

        // Fetch the guild's roles once. We need them to resolve any role without
        // an env override by name, *and* to label every resolved id - overrides
        // included - with its current guild name, so a caller can confirm which
        // role each id points at before writing.
        let guild_roles: Vec<GuildRole> = http.get_guild_roles(guild_id).await?;
        let name_by_id: HashMap<RoleId, String> = guild_roles
            .iter()
            .map(|gr| (gr.id, gr.name.clone()))
            .collect();

        let mut role_ids: HashMap<Role, RoleId> = HashMap::new();
        let mut managed: Vec<ManagedRole> = Vec::new();
        for role in Role::ALL {
            let (id, from_env_override) = match std::env::var(role.env_var()) {
                Ok(s) => {
                    let n = s
                        .parse::<u64>()
                        .map_err(|e| DiscordError::BadEnv(role.env_var(), e))?;
                    (RoleId::new(n), true)
                }
                Err(_) => {
                    let found = guild_roles
                        .iter()
                        .find(|gr| gr.name == role.as_str())
                        .ok_or(DiscordError::RoleNotFound(role.as_str()))?;
                    (found.id, false)
                }
            };
            role_ids.insert(role, id);
            // For a by-name match this is always the canonical name; for an
            // override it is whatever the (possibly mistaken) id actually points
            // at - exactly what makes a fat-fingered override visible.
            let name = name_by_id
                .get(&id)
                .cloned()
                .unwrap_or_else(|| format!("(no guild role with id {id})"));
            managed.push(ManagedRole {
                role,
                id: id.get(),
                name,
                from_env_override,
            });
        }

        Ok(Self {
            http,
            guild_id,
            role_ids,
            managed,
            _token: token,
        })
    }
}

#[allow(clippy::result_large_err)] // shares DiscordError with the trait methods
fn read_env(key: &'static str) -> Result<String, DiscordError> {
    std::env::var(key).map_err(|_| DiscordError::MissingEnv(key))
}

#[allow(clippy::result_large_err)] // shares DiscordError with the trait methods
fn read_env_u64(key: &'static str) -> Result<u64, DiscordError> {
    read_env(key)?
        .parse::<u64>()
        .map_err(|e| DiscordError::BadEnv(key, e))
}

#[async_trait]
impl DiscordClient for DiscordHttp {
    fn guild_id(&self) -> DiscordGuildId {
        DiscordGuildId(self.guild_id.get())
    }

    fn managed_roles(&self) -> Vec<ManagedRole> {
        self.managed.clone()
    }

    async fn list_members(&self) -> Result<Vec<DiscordMember>, DiscordError> {
        // Fetch the guild's roles once so every member's role ids can be rendered
        // as names for the preview, the same way `member_roles` does for one.
        let names: HashMap<RoleId, String> = self
            .http
            .get_guild_roles(self.guild_id)
            .await?
            .into_iter()
            .map(|r| (r.id, r.name))
            .collect();

        let mut out: Vec<DiscordMember> = Vec::new();
        let mut after: Option<u64> = None;
        loop {
            let page = self
                .http
                .get_guild_members(self.guild_id, Some(MEMBER_PAGE_SIZE), after)
                .await?;
            if page.is_empty() {
                break;
            }
            let last_id = page.last().expect("non-empty checked above").user.id.get();
            for m in &page {
                out.push(DiscordMember {
                    id: DiscordUserId(m.user.id.get()),
                    handle: DiscordHandle(m.user.name.clone()),
                    display_name: display_name_from(
                        m.nick.as_deref(),
                        m.user.global_name.as_deref(),
                        &m.user.name,
                    ),
                    current_status: pick_current_status(&m.roles, &self.role_ids),
                    role_names: role_names_for(&m.roles, &names),
                    bot: m.user.bot,
                });
            }
            if (page.len() as u64) < MEMBER_PAGE_SIZE {
                break;
            }
            after = Some(last_id);
        }
        Ok(out)
    }

    async fn set_role(
        &self,
        user: DiscordUserId,
        current: Option<Role>,
        target: Role,
        dry_run: DryRun,
    ) -> Result<(), DiscordError> {
        let (add, remove) = match diff_status_roles(current, target) {
            StatusDiff::NoOp => {
                tracing::debug!(
                    user = %user,
                    ?target,
                    "set_role: already in target state, skipping"
                );
                return Ok(());
            }
            StatusDiff::Apply { add, remove } => (add, remove),
        };

        if dry_run.is_dry() {
            tracing::info!(
                user = %user,
                ?add,
                ?remove,
                "dry-run: set_role"
            );
            return Ok(());
        }

        let user_id = UserId::new(user.0);
        let add_id = *self
            .role_ids
            .get(&add)
            .expect("role_ids populated for all Role::ALL");
        // Add first so the member is never roleless mid-call.
        self.http
            .add_member_role(self.guild_id, user_id, add_id, Some(AUDIT_LOG_REASON))
            .await?;
        if let Some(rm) = remove {
            let rm_id = *self
                .role_ids
                .get(&rm)
                .expect("role_ids populated for all Role::ALL");
            self.http
                .remove_member_role(self.guild_id, user_id, rm_id, Some(AUDIT_LOG_REASON))
                .await?;
        }
        Ok(())
    }

    async fn remove_roles(
        &self,
        user: DiscordUserId,
        roles: &[Role],
        dry_run: DryRun,
    ) -> Result<(), DiscordError> {
        if roles.is_empty() {
            tracing::debug!(user = %user, "remove_roles: nothing to remove");
            return Ok(());
        }
        if dry_run.is_dry() {
            tracing::info!(user = %user, ?roles, "dry-run: remove_roles");
            return Ok(());
        }
        let user_id = UserId::new(user.0);
        for role in roles {
            let role_id = *self
                .role_ids
                .get(role)
                .expect("role_ids populated for all Role::ALL");
            self.http
                .remove_member_role(self.guild_id, user_id, role_id, Some(AUDIT_LOG_REASON))
                .await?;
        }
        Ok(())
    }

    async fn member_roles(&self, user: DiscordUserId) -> Result<MemberRoles, DiscordError> {
        let member = self
            .http
            .get_member(self.guild_id, UserId::new(user.0))
            .await?;
        let names: HashMap<RoleId, String> = self
            .http
            .get_guild_roles(self.guild_id)
            .await?
            .into_iter()
            .map(|r| (r.id, r.name))
            .collect();
        let held_ids: HashSet<RoleId> = member.roles.iter().copied().collect();
        let all_names = role_names_for(&member.roles, &names);
        // Match managed roles by id (so name overrides don't hide a held role),
        // against the member's role set for O(1) lookups.
        let held = Role::ALL
            .into_iter()
            .filter(|r| self.role_ids.get(r).is_some_and(|id| held_ids.contains(id)))
            .collect();
        Ok(MemberRoles { all_names, held })
    }

    async fn list_channels(&self) -> Result<Vec<DiscordChannel>, DiscordError> {
        let channels = self.http.get_channels(self.guild_id).await?;
        Ok(channels
            .iter()
            .filter(|c| !is_thread(c.kind))
            .map(project_channel)
            .collect())
    }

    async fn list_role_ids(&self) -> Result<Vec<u64>, DiscordError> {
        Ok(self
            .http
            .get_guild_roles(self.guild_id)
            .await?
            .into_iter()
            .map(|r| r.id.get())
            .collect())
    }
}
