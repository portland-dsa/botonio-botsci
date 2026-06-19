"""Filesystem and secrets primitives for the deploy tool.

The foundation the verbs build on: permission/ownership value types, a subprocess wrapper,
the directory + credential installers, SOPS decryption, and root escalation. Everything
POSIX-specific (``os.geteuid``, ``pwd``, ``grp``, ``shutil.chown``) is used *inside* a
function, never at import time, so this module imports cleanly on Windows.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

from .defs import *

ROOTNAME = "botonio-botsci"
SOPS_AGE_KEYFILE = Path("/root/.config/sops/age/keys.txt")


def service(target: str) -> FileOwnership:
    """Ownership for a service instance's files: ``root`` owner, ``botonio-botsci-<target>`` group."""
    return FileOwnership(group=f"{ROOTNAME}-{target}")


def run(cmd, *, input=None, capture=False, text=True, env=None, cwd=None, check=True):
    """Run a command (list form, never a shell), checking the exit code by default.

    Pass ``text=False`` to send/receive ``bytes`` - used wherever a secret must not be
    decoded to ``str`` on the way through.
    """
    return subprocess.run(
        cmd,
        input=input,
        capture_output=capture,
        text=text,
        env=env,
        cwd=cwd,
        check=check,
    )


def ensure_root() -> None:
    """Re-exec the whole tool under ``sudo`` if not already root, then point sops at the box key.

    POSIX-only (``os.geteuid``); called by the on-box verbs (box-side only).
    """
    if os.geteuid() != 0:
        os.execvp("sudo", ["sudo", sys.executable, *sys.argv])
    os.environ.setdefault("SOPS_AGE_KEYFILE", SOPS_AGE_KEYFILE.as_posix())


ADMIN_GROUPS = ("root", "sudo")


def assert_trusted(path: Path) -> None:
    """Raise unless ``path`` is owned by an admin user+group and is not world-accessible.

    Guards the staged secrets directory before ciphertext is read out of it.
    """
    path = Path(path)
    owner, group = path.owner(), path.group()
    if not _is_admin_user(owner) or group not in ADMIN_GROUPS:
        raise PermissionError(
            f"{path} must be owned by an admin user and group, not {owner}:{group}"
        )
    if Mode.from_st_mode(path.stat().st_mode).other.any:
        raise PermissionError(
            f"{path} is world-accessible; refusing to read secrets from it"
        )


def _is_admin_user(name: str) -> bool:
    """True if ``name`` is root or belongs to an admin group (root/sudo).

    ``pwd``/``grp`` are imported here, not at module top, so this module still loads on Windows.
    """
    import pwd

    info = pwd.getpwnam(name)
    return info.pw_uid == 0 or any(
        _in_group(name, info.pw_gid, group) for group in ADMIN_GROUPS
    )


def _in_group(name: str, primary_gid: int, group: str) -> bool:
    """True if ``name`` is in ``group`` - by its primary gid or the membership list."""
    import grp

    entry = grp.getgrnam(group)
    return entry.gr_gid == primary_gid or name in entry.gr_mem


def install_dir(dest, perms: FilePermissions, owner: FileOwnership = ROOT) -> None:
    """Ensure directory ``dest`` exists with the given mode and ownership (like ``install -d``)."""
    dest = Path(dest)
    dest.mkdir(mode=perms, parents=True, exist_ok=True)
    dest.chmod(perms)  # mkdir's mode is masked by umask; chmod forces it.
    shutil.chown(dest, user=owner.user, group=owner.group)


def creds_encrypt(
    token: SecretTokens, data: bytes, dest: Path, owner: FileOwnership = ROOT
) -> None:
    """Encrypt ``data`` into a systemd credential at ``dest`` (``systemd-creds encrypt``).

    The plaintext is piped in on stdin so it never reaches argv or disk; the ``.cred`` is
    written 0600. ``name`` must match the unit's ``LoadCredentialEncrypted=<name>``.
    """
    run(
        [
            "systemd-creds",
            "encrypt",
            f"--name={token.value}",
            "--with-key=host",
            "-",
            str(dest),
        ],
        input=data,
        text=False,
    )
    Path(dest).chmod(FilePermissions.Private)
    shutil.chown(dest, user=owner.user, group=owner.group)


def sops_extract(enc_path: Path, key: SecretTokens) -> bytes:
    """Decrypt a single key out of a SOPS file; returns raw ``bytes`` (fed onward over stdin)."""
    return run(
        ["sops", "-d", "--extract", f'["{key}"]', str(enc_path)],
        capture=True,
        text=False,
    ).stdout
