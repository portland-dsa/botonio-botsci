"""Bot-instance deploy definitions.

The per-instance secret tokens and the non-secret ``Environment=`` keys for a bot instance.
``BotSecretTokens`` is the subset of the shared :class:`SecretTokens` each instance gets; its
members carry the canonical ``SecretTokens`` as their value so the two can never drift.
"""

from __future__ import annotations

from enum import Enum, StrEnum

from . import SecretTokens, EnvVars


class BotSecretTokens(Enum):
    """The ``SecretTokens`` provisioned per bot instance (Solidarity Tech is production-only).

    The three SSO credentials are always provisioned even when ``BOT_SSO_ENABLED`` is unset;
    the bot ignores them until that flag is on, but the ``.cred`` files must be present or
    systemd fails credential loading at start. Zoe encrypts the real values once
    workspace-sync is ready and the OAuth app is registered.
    """

    DiscordBotToken = SecretTokens.DiscordBotToken
    SolidarityTechToken = SecretTokens.SolidarityTechToken
    DbMigrationPassword = SecretTokens.DbMigrationPassword
    AuditHashKey = SecretTokens.AuditHashKey
    SsoOauthClientSecret = SecretTokens.SsoOauthClientSecret
    SsoSigningKey = SecretTokens.SsoSigningKey
    SsoCallerBearer = SecretTokens.SsoCallerBearer


class BotEnvironmentValues(StrEnum):
    """The non-secret ``Environment=`` keys rendered into a bot instance's override.conf."""

    DiscordGuildId = EnvVars.DiscordGuildId
    StUserListId = EnvVars.StUserListId

    # Personas (staging mock only)
    GoodStandingUserId = EnvVars.GoodStandingUserId
    ExpiringUserId = EnvVars.ExpiringUserId
    LapsedUserId = EnvVars.LapsedUserId

    # SSO role-check (non-secret; the client id is public, the redirect uri is the relay callback)
    SsoOauthClientId = EnvVars.SsoOauthClientId
    SsoRedirectUri = EnvVars.SsoRedirectUri
