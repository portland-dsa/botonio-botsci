"""Backup deploy definitions.

The box-level backup credential and the non-secret ``Environment=`` keys for the backup timer.
Backup is not per-instance, so there is no staging/production split here. ``BackupSecretTokens``
mirrors ``BotSecretTokens``: a subset of the shared :class:`SecretTokens`, valued by the token
itself.
"""

from __future__ import annotations

from enum import Enum, StrEnum

from . import SecretTokens


class BackupSecretTokens(Enum):
    """The ``SecretTokens`` the backup timer needs (just the box-level b2 application key)."""

    B2ApplicationKey = SecretTokens.B2ApplicationKey


class BackupEnvironmentValues(StrEnum):
    """The non-secret ``Environment=`` keys for the backup timer."""

    B2BucketName = "b2_bucket_name"
    B2KeyId = "b2_key_id"
    BackupAgeRecipient = "box_age_pubkey"
