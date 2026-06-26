//! The live `DiscordHttp` client over `serenity::http::Http`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serenity::http::Http;
use serenity::model::guild::Role as GuildRole;
use serenity::model::id::{GuildId, RoleId, UserId};

use crate::MemberPage;
use crate::util::{DiscordChannelId, DiscordGuildId, DiscordHandle, DiscordUserId};

use super::channels::{
    ChannelKind, DiscordChannel, GuildChannels, OverwriteTarget, PermOverwrite, Permissions,
};
use super::client::DiscordClient;
use super::error::DiscordError;
use super::roles::{
    DiscordRosterMember, ManagedRole, MarkerRole, MemberRoles, Role, RoleExt, StatusDiff,
    diff_status_roles, role_names_for,
};
use domain::DiscordRoleId;

/// Reason attached to every role add/remove, so the change is legible in the
/// guild's audit log.
const AUDIT_LOG_REASON: &str = "discord-bulk-update verification";

/// Discord caps the guild-members list at 1000 per page; request the max so a sweep
/// of a few-thousand-member guild is a handful of round-trips.
const MEMBERS_PAGE_LIMIT: u64 = 1000;

/// Live [`DiscordClient`], REST-only - no gateway, no cache.
///
/// Wraps a shared `serenity::http::Http`, which handles per-bucket rate-limit
/// retries. In production the bot's gateway owns that `Http` and hands this client
/// a clone of the same `Arc` through [`from_http`](DiscordHttp::from_http), so
/// guild writes ride the one connection the gateway already authenticated - there
/// is no second token read or second client. The managed role ids are resolved
/// once, before construction (see [`resolve_managed_roles`]), so trait calls never
/// look them up.
pub struct DiscordHttp {
    http: Arc<Http>,
    guild_id: GuildId,
    role_ids: HashMap<Role, RoleId>,
    /// The three managed roles as resolved before construction (id, name, override
    /// provenance), kept so [`managed_roles`](DiscordClient::managed_roles) can
    /// echo them without a network round-trip. In `Role::ALL` order.
    managed: Vec<ManagedRole>,
    /// Marker roles held apart from `role_ids` (the status trichotomy) so a marker
    /// write never affects the strip set.
    marker_role_ids: HashMap<MarkerRole, RoleId>,
}

impl DiscordHttp {
    /// Returns every managed [`Role`] whose configured id is present in `roles`.
    ///
    /// Matches by role id (so `DISCORD_ROLE_*_ID` name overrides don't hide a
    /// held role) and returns in [`Role::ALL`] priority order.
    fn managed_roles_held(&self, roles: &[RoleId]) -> Vec<Role> {
        let held_ids: HashSet<RoleId> = roles.iter().copied().collect();
        Role::ALL
            .into_iter()
            .filter(|r| self.role_ids.get(r).is_some_and(|id| held_ids.contains(id)))
            .collect()
    }

    /// Builds the client from an already-authenticated `Http` and the
    /// pre-resolved managed roles.
    ///
    /// This is the production path: the bot's gateway owns the `Arc<Http>` and
    /// passes a clone of it, together with the [`ManagedRole`]s from a single
    /// [`resolve_managed_roles`] call (made where the gateway's `Http` is in
    /// scope). Nothing is read from the environment and no `Http` is built here.
    /// The `role_ids` lookup table the write methods use is derived from `managed`.
    pub fn from_http(http: Arc<Http>, guild_id: GuildId, managed: Vec<ManagedRole>) -> Self {
        let role_ids: HashMap<Role, RoleId> = managed
            .iter()
            .map(|m| (m.role, RoleId::new(m.id)))
            .collect();
        Self {
            http,
            guild_id,
            role_ids,
            managed,
            marker_role_ids: HashMap::new(),
        }
    }

    /// Build directly from an already-resolved `Role -> RoleId` map, bypassing the
    /// by-name [`resolve_managed_roles`] lookup. A caller that holds the role ids
    /// already - for instance from stored configuration - uses this to build the
    /// write client without a guild role fetch, and may rebuild it cheaply whenever
    /// those ids change. `managed` is derived from the map for the trait's
    /// [`managed_roles`](DiscordClient::managed_roles) accessor; the role name is
    /// filled with the [`Role`]'s own label, since the live guild name is fetched on
    /// demand where it is actually displayed.
    ///
    /// The map must hold a [`RoleId`] for every [`Role`] in [`Role::ALL`]: the write
    /// methods index `role_ids` unconditionally and panic on a missing role. The bot
    /// upholds this by building only once all managed roles are configured.
    pub fn from_role_map(
        http: Arc<Http>,
        guild_id: GuildId,
        role_ids: HashMap<Role, RoleId>,
        marker_role_ids: HashMap<MarkerRole, RoleId>,
    ) -> Self {
        debug_assert!(
            Role::ALL.iter().all(|r| role_ids.contains_key(r)),
            "from_role_map requires a RoleId for every managed Role",
        );
        // Build `managed` in `Role::ALL` order (the order the field documents), rather
        // than relying on the map's iteration order; skip any role absent from the map.
        let managed = Role::ALL
            .into_iter()
            .filter_map(|role| {
                role_ids.get(&role).map(|id| ManagedRole {
                    role,
                    id: id.get(),
                    name: role.as_str().to_owned(),
                    from_env_override: false,
                })
            })
            .collect();
        Self {
            http,
            guild_id,
            role_ids,
            managed,
            marker_role_ids,
        }
    }

    /// Constructs the client standalone, reading the token and building its own
    /// `Http`.
    ///
    /// Reads `DISCORD_BOT_TOKEN` (wrapped immediately in a `SecretString`) and
    /// `DISCORD_GUILD_ID`, then resolves the roles via [`resolve_managed_roles`].
    /// Returns [`DiscordError::MissingEnv`] or [`DiscordError::BadEnv`] for env
    /// problems, or [`DiscordError::RoleNotFound`] if a role name is not present on
    /// the guild.
    ///
    /// Unlike [`from_http`](DiscordHttp::from_http) this reads the token itself and
    /// stands up a second `Http` - which the production bot never does, since it
    /// shares the gateway's connection. It is therefore gated to the test and
    /// `live-discord` builds that exercise the client directly, and is compiled out
    /// of the bot binary.
    #[cfg(feature = "live-discord")]
    pub async fn from_env() -> Result<Self, DiscordError> {
        use secrecy::{ExposeSecret, SecretString};

        let token_raw =
            crate::util::secret::from_credstore_or_env("discord_bot_token", "DISCORD_BOT_TOKEN")
                .ok_or(DiscordError::MissingEnv("DISCORD_BOT_TOKEN"))?;
        let token = SecretString::from(token_raw);
        let guild_id = GuildId::new(read_env_u64("DISCORD_GUILD_ID")?);

        let http = Arc::new(Http::new(token.expose_secret()));
        let managed = resolve_managed_roles(&http, guild_id).await?;
        Ok(Self::from_http(http, guild_id, managed))
    }
}

/// Resolves the three managed status [`Role`]s against the live guild.
///
/// Fetches the guild's roles once and, for each [`Role`], takes its
/// `DISCORD_ROLE_*_ID` environment override when set (otherwise matches by name),
/// labelling every resolved id with its current guild name so a fat-fingered
/// override is visible. Returns the [`ManagedRole`]s in `Role::ALL` order;
/// [`DiscordHttp::from_http`] turns them into the id lookup table the write methods
/// use. The bot calls this once at startup with the gateway's `Http`.
///
/// Returns [`DiscordError::BadEnv`] for an unparseable override, or
/// [`DiscordError::RoleNotFound`] if a role has neither an override nor a by-name
/// match on the guild.
pub async fn resolve_managed_roles(
    http: &Http,
    guild_id: GuildId,
) -> Result<Vec<ManagedRole>, DiscordError> {
    // Fetch the guild's roles once. We need them to resolve any role without an env
    // override by name, *and* to label every resolved id - overrides included -
    // with its current guild name, so a caller can confirm which role each id
    // points at before writing.
    let guild_roles: Vec<GuildRole> = http.get_guild_roles(guild_id).await?;
    let name_by_id: HashMap<RoleId, String> = guild_roles
        .iter()
        .map(|gr| (gr.id, gr.name.clone()))
        .collect();

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
        // For a by-name match this is always the canonical name; for an override it
        // is whatever the (possibly mistaken) id actually points at - exactly what
        // makes a fat-fingered override visible.
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
    Ok(managed)
}

#[cfg(feature = "live-discord")]
fn read_env(key: &'static str) -> Result<String, DiscordError> {
    std::env::var(key).map_err(|_| DiscordError::MissingEnv(key))
}

#[cfg(feature = "live-discord")]
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

    async fn set_role(
        &self,
        user: DiscordUserId,
        current: Option<Role>,
        target: Role,
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

    async fn remove_roles(&self, user: DiscordUserId, roles: &[Role]) -> Result<(), DiscordError> {
        if roles.is_empty() {
            tracing::debug!(user = %user, "remove_roles: nothing to remove");
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
        let all_names = role_names_for(&member.roles, &names);
        let held = self.managed_roles_held(&member.roles);
        Ok(MemberRoles { all_names, held })
    }

    async fn member_status_role(&self, user: DiscordUserId) -> Result<Option<Role>, DiscordError> {
        let member = match self
            .http
            .get_member(self.guild_id, UserId::new(user.0))
            .await
        {
            Ok(m) => m,
            // A non-member returns 404; that is "not in the guild", not an error.
            Err(serenity::Error::Http(ref http_err))
                if http_err.status_code() == Some(reqwest::StatusCode::NOT_FOUND) =>
            {
                return Ok(None);
            }
            Err(e) => {
                // Distinguish a persistent permission/intent misconfiguration (403) from a
                // transient error: a 403 fails *every* SSO check (the bot lacks the
                // GUILD_MEMBERS intent or the member-read permission), so surface it
                // distinctly rather than letting it read as a probe in the uniform abuse
                // warning the denial emits downstream. A 5xx/429 still fails closed as a
                // plain read error.
                let forbidden = matches!(
                    &e,
                    serenity::Error::Http(h)
                        if h.status_code() == Some(reqwest::StatusCode::FORBIDDEN)
                );
                if forbidden {
                    tracing::error!(
                        error = %e,
                        "discord: member read forbidden - SSO role checks will all fail until \
                         the GUILD_MEMBERS intent and the member-read permission are granted"
                    );
                }
                return Err(e.into());
            }
        };
        // `managed_roles_held` filters `Role::ALL`, so `held` is already in
        // [Member, DuesExpired, Unverified] precedence order; the highest-priority held role
        // is simply its first element, and no managed role held means Unverified.
        let held = self.managed_roles_held(&member.roles);
        Ok(Some(held.first().copied().unwrap_or(Role::Unverified)))
    }

    async fn members_page(
        &self,
        cursor: Option<&str>,
    ) -> Result<MemberPage<DiscordRosterMember>, DiscordError> {
        // The cursor is the stringified "after" snowflake; None starts at id 0.
        let after = cursor.and_then(|c| c.parse::<u64>().ok());
        // serenity 0.12: get_guild_members(guild_id, limit: Option<u64>, after: Option<u64>).
        let members = self
            .http
            .get_guild_members(self.guild_id, Some(MEMBERS_PAGE_LIMIT), after)
            .await?;

        let scanned = members.len() as u64;
        // Discord returns the page sorted by ascending id; the next "after" is the
        // last id seen. A short page (< limit) ends the sweep.
        let next = if (scanned as usize) < MEMBERS_PAGE_LIMIT as usize {
            None
        } else {
            members.last().map(|m| m.user.id.get().to_string())
        };

        let projected = members
            .into_iter()
            .map(|m| {
                let held = self.managed_roles_held(&m.roles);
                DiscordRosterMember {
                    id: DiscordUserId(m.user.id.get()),
                    handle: DiscordHandle(m.user.name.clone()),
                    held,
                    bot: m.user.bot,
                }
            })
            .collect();

        Ok(MemberPage {
            members: projected,
            scanned,
            total: None, // Discord's member-list endpoint reports no total count.
            next,
        })
    }

    async fn assign_marker_role(
        &self,
        user: DiscordUserId,
        marker: MarkerRole,
    ) -> Result<(), DiscordError> {
        let role = *self
            .marker_role_ids
            .get(&marker)
            .ok_or(DiscordError::MarkerRoleUnconfigured(marker))?;
        self.http
            .add_member_role(
                self.guild_id,
                UserId::new(user.0),
                role,
                Some(AUDIT_LOG_REASON),
            )
            .await?;
        Ok(())
    }

    async fn remove_marker_role(
        &self,
        user: DiscordUserId,
        marker: MarkerRole,
    ) -> Result<(), DiscordError> {
        // No role configured for this marker means there is nothing to remove.
        let Some(role) = self.marker_role_ids.get(&marker).copied() else {
            return Ok(());
        };
        self.http
            .remove_member_role(
                self.guild_id,
                UserId::new(user.0),
                role,
                Some(AUDIT_LOG_REASON),
            )
            .await?;
        Ok(())
    }

    async fn read_channels(&self) -> Result<GuildChannels, DiscordError> {
        use serenity::model::channel::{ChannelType, PermissionOverwriteType};

        let raw = self.http.get_channels(self.guild_id).await?;

        let map_target = |kind: &PermissionOverwriteType| match kind {
            PermissionOverwriteType::Role(r) => Some(OverwriteTarget::Role(DiscordRoleId(r.get()))),
            PermissionOverwriteType::Member(u) => {
                Some(OverwriteTarget::Member(DiscordUserId(u.get())))
            }
            // Forward-compat: an overwrite kind we do not model is dropped.
            _ => None,
        };

        let mut channels = Vec::with_capacity(raw.len());
        for c in raw {
            let kind = match c.kind {
                ChannelType::Category => ChannelKind::Category,
                ChannelType::Text => ChannelKind::Text,
                ChannelType::Voice => ChannelKind::Voice,
                ChannelType::News => ChannelKind::Announcement,
                ChannelType::Forum => ChannelKind::Forum,
                ChannelType::Stage => ChannelKind::Stage,
                // Threads and any unknown future kind: skipped (perms derive from parent).
                _ => continue,
            };
            let overwrites = c
                .permission_overwrites
                .iter()
                .filter_map(|o| {
                    map_target(&o.kind).map(|target| PermOverwrite {
                        target,
                        allow: o.allow,
                        deny: o.deny,
                    })
                })
                .collect();
            channels.push(DiscordChannel {
                id: DiscordChannelId(c.id.get()),
                name: c.name.clone(),
                kind,
                parent_id: c.parent_id.map(|p| DiscordChannelId(p.get())),
                position: c.position,
                overwrites,
            });
        }

        let guild_roles = self.http.get_guild_roles(self.guild_id).await?;
        // The @everyone role shares its id with the guild id.
        let everyone_base_view = guild_roles
            .iter()
            .find(|r| r.id.get() == self.guild_id.get())
            .is_some_and(|r| r.permissions.contains(Permissions::VIEW_CHANNEL));

        Ok(GuildChannels {
            guild_id: DiscordGuildId(self.guild_id.get()),
            everyone_base_view,
            channels,
        })
    }

    async fn set_channel_overwrites(
        &self,
        id: DiscordChannelId,
        overwrites: &[PermOverwrite],
    ) -> Result<(), DiscordError> {
        use serenity::builder::EditChannel;
        use serenity::model::channel::{PermissionOverwrite as SerPo, PermissionOverwriteType};
        use serenity::model::id::ChannelId;

        let ser: Vec<SerPo> = overwrites
            .iter()
            .map(|o| SerPo {
                allow: o.allow,
                deny: o.deny,
                kind: match o.target {
                    OverwriteTarget::Role(r) => PermissionOverwriteType::Role(RoleId::new(r.0)),
                    OverwriteTarget::Member(u) => PermissionOverwriteType::Member(UserId::new(u.0)),
                },
            })
            .collect();

        ChannelId::new(id.0)
            .edit(&self.http, EditChannel::new().permissions(ser))
            .await
            .map(|_| ())
            .map_err(DiscordError::from)
    }
}
