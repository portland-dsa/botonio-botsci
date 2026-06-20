"""Database deploy definitions.

``DbSecretTokens`` is the subset of the shared :class:`SecretTokens` the database provisioning
needs (just the migration password); its member carries the canonical ``SecretTokens`` as its
value so the two can never drift.
"""

from __future__ import annotations

from enum import Enum

from . import SecretTokens


class DbSecretTokens(Enum):
    """The ``SecretTokens`` the database provisioning needs (the migration role's password)."""

    DbMigrationPassword = SecretTokens.DbMigrationPassword
