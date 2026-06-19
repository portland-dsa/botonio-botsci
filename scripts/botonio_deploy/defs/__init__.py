"""Descriptive value types shared across the deploy tool.

Pure data: no I/O and no POSIX calls at import time, so this package loads anywhere (including
the Windows workstation). The cross-component types live here; the per-component subsets live in
the ``bot`` and ``backup`` submodules and are re-exported below, so callers import them straight
from ``defs`` without caring which file holds them. ``ops`` star-imports this package.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import IntEnum, StrEnum
from stat import (
    S_IRGRP,
    S_IROTH,
    S_IRUSR,
    S_IWGRP,
    S_IWOTH,
    S_IWUSR,
    S_IXGRP,
    S_IXOTH,
    S_IXUSR,
)


class Targets(StrEnum):
    Staging = "staging"
    Production = "production"


class SecretTokens(StrEnum):
    DiscordBotToken = "discord_bot_token"
    SolidarityTechToken = "solidarity_tech_token"
    DbMigrationPassword = "db_migration_password"
    B2ApplicationKey = "b2_application_key"


class FilePermissions(IntEnum):
    """Named POSIX modes used on the box.

    An :class:`IntEnum` so a member *is* an ``int`` - ``path.chmod(FilePermissions.Private)``
    works directly, and the name documents intent at the call site.
    """

    Private = 0o600  # secret files (.cred)
    PrivateDir = 0o700  # secret dirs, the sbin backup script
    GroupConfig = 0o640  # root writes, the service/postgres group reads
    GroupDir = 0o750  # /etc/botonio-botsci/<component>/<target>
    WorldConfig = 0o644  # world-readable config (the unit files)
    WorldDir = 0o755  # systemd drop-in dirs


@dataclass(frozen=True)
class FileOwnership:
    """Owner of an installed file or directory.

    ``user`` is ``root`` for everything we install; ``group`` is the part that varies (a
    service group, or ``postgres``). Frozen so the shared constants below can't be mutated.
    """

    group: str
    user: str = "root"


ROOT = FileOwnership(group="root")


@dataclass(frozen=True)
class Access:
    """The read/write/execute bits of one POSIX class (owner, group, or other)."""

    read: bool = False
    write: bool = False
    execute: bool = False

    @property
    def any(self) -> bool:
        """Whether this class is granted any access at all."""
        return self.read or self.write or self.execute


@dataclass(frozen=True)
class Mode:
    """A file's permission bits as the three POSIX classes.

    Built from a ``stat`` mode so checks read as intent (``mode.other.any``) rather than as
    octal bitmasks (``st_mode & 0o007``).
    """

    owner: Access
    group: Access
    other: Access

    @classmethod
    def from_st_mode(cls, st_mode: int) -> "Mode":
        """Decompose a ``stat`` mode integer into its owner/group/other access."""

        def access(r: int, w: int, x: int) -> Access:
            return Access(
                read=bool(st_mode & r),
                write=bool(st_mode & w),
                execute=bool(st_mode & x),
            )

        return cls(
            owner=access(S_IRUSR, S_IWUSR, S_IXUSR),
            group=access(S_IRGRP, S_IWGRP, S_IXGRP),
            other=access(S_IROTH, S_IWOTH, S_IXOTH),
        )


# The component subsets below import the shared types defined above, so these imports must come
# after those definitions - that ordering is what keeps the package/submodule cycle resolvable.
from .backup import BackupEnvironmentValues, BackupSecretTokens
from .bot import BotEnvironmentValues, BotSecretTokens

__all__ = [
    "Targets",
    "SecretTokens",
    "FilePermissions",
    "FileOwnership",
    "ROOT",
    "Access",
    "Mode",
    "BotSecretTokens",
    "BotEnvironmentValues",
    "BackupSecretTokens",
    "BackupEnvironmentValues",
]
