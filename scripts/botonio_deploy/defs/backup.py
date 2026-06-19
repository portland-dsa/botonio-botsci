"""Backup deploy definitions.

The box-level backup credential and the non-secret ``Environment=`` keys for the backup timer.
Backup is not per-instance, so there is no staging/production split here. ``BackupSecretTokens``
mirrors ``BotSecretTokens``: a subset of the shared :class:`SecretTokens`, valued by the token
itself.
"""

from __future__ import annotations

from enum import Enum, StrEnum

from . import SecretTokens, EnvVars


class BackupSecretTokens(Enum):
    """The ``SecretTokens`` the backup timer needs (just the box-level b2 application key)."""

    B2ApplicationKey = SecretTokens.B2ApplicationKey


class BackupEnvironmentValues(StrEnum):
    """The non-secret ``Environment=`` keys for the backup timer."""

    B2BucketName = EnvVars.B2BucketName
    B2KeyId = EnvVars.B2KeyId
    BackupAgeRecipient = EnvVars.BackupAgeRecipient
