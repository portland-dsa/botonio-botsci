"""Bot-instance deploy definitions.

The per-instance secret tokens and the non-secret ``Environment=`` keys for a bot instance.
``BotSecretTokens`` is the subset of the shared :class:`SecretTokens` each instance gets; its
members carry the canonical ``SecretTokens`` as their value so the two can never drift.
"""

from __future__ import annotations

from enum import Enum, StrEnum

from . import SecretTokens, EnvVars


class BotSecretTokens(Enum):
    """The ``SecretTokens`` provisioned per bot instance (Solidarity Tech is production-only)."""

    DiscordBotToken = SecretTokens.DiscordBotToken
    SolidarityTechToken = SecretTokens.SolidarityTechToken
    DbMigrationPassword = SecretTokens.DbMigrationPassword
    AuditHashKey = SecretTokens.AuditHashKey


class BotEnvironmentValues(StrEnum):
    """The non-secret ``Environment=`` keys rendered into a bot instance's override.conf."""

    DiscordGuildId = EnvVars.DiscordGuildId
    StUserListId = EnvVars.StUserListId

    # Personas (staging mock only)
    GoodStandingUserId = EnvVars.GoodStandingUserId
    ExpiringUserId = EnvVars.ExpiringUserId
    LapsedUserId = EnvVars.LapsedUserId
