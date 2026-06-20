"""Filesystem and secrets primitives for the deploy tool.

The foundation the verbs build on: permission/ownership value types, a subprocess wrapper,
the directory + credential installers, SOPS decryption, and root escalation. Everything
POSIX-specific (``os.geteuid``, ``pwd``, ``grp``, ``shutil.chown``) is used *inside* a
function, never at import time, so this module imports cleanly on Windows.
"""

from __future__ import annotations

from dataclasses import dataclass
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Dict, Generic, Iterable, List, Optional, Set, TypeVar

from .defs import *

ROOTNAME = "botonio-botsci"
SOPS_AGE_KEYFILE = Path("/root/.config/sops/age/keys.txt")
STAGED_SECRETS = Path("/tmp/botonio-botsci")
# Where redeploy stashes the encrypted secrets inside the .pyz; `provision --bundled-secrets`
# reads them back out via importlib.resources.
BUNDLED_SECRETS_DIR = "_bundled_secrets"


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


def prepare(secrets_dir: Path) -> None:
    """Escalate to root and refuse to read secrets out of an untrusted directory.

    The standard preamble for the box-side provision verbs.
    """
    ensure_root()
    assert_trusted(secrets_dir)


def scp(
    file: Path,
    host: str,
    *,
    user: Optional[str] = None,
    target_dir: str = "",
    ensure_exists: bool = False,
) -> None:
    """Copy ``file`` to ``host`` over scp (into ``target_dir``, the login's home by default).

    ``user`` selects the login (``user@host``). With ``ensure_exists`` the destination dir is
    created first at mode 0700 - as the *login* user, not root, so it's for a user-writable
    staging dir, not the root-only secrets dir. The file is sent as a bare name with ``cwd`` set
    to its parent, so a Windows drive letter (``Z:\\...``) is never mistaken for an scp
    ``host:path``.
    """
    if user is not None:
        payload = f"{user}@{host}"
    else:
        payload = host

    payload_dir = f"{payload}:{target_dir}"

    if ensure_exists:
        run(["ssh", payload, f"install -d -m 700 {target_dir}"])

    run(["scp", file.name, payload_dir], cwd=file.parent)


def install_dir(dest, perms: FilePermissions, owner: FileOwnership = ROOT) -> None:
    """Ensure directory ``dest`` exists with the given mode and ownership (like ``install -d``)."""
    dest = Path(dest)
    dest.mkdir(mode=perms, parents=True, exist_ok=True)
    dest.chmod(perms)  # mkdir's mode is masked by umask; chmod forces it.
    shutil.chown(dest, user=owner.user, group=owner.group)


def install_file(
    data: bytes, dest: Path, perms: FilePermissions, owner: FileOwnership = ROOT
) -> None:
    """Write ``data`` to ``dest`` with the given mode and ownership (like ``install -m``)."""
    dest = Path(dest)
    dest.write_bytes(data)
    dest.chmod(perms)
    shutil.chown(dest, user=owner.user, group=owner.group)


def daemon_reload() -> None:
    """Make systemd re-read unit files after they have been installed or changed.

    Without this, ``systemctl`` warns that the unit changed on disk and refuses to act on the
    new definition until reloaded.
    """
    run(["systemctl", "daemon-reload"])


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


# A SOPS key is always a string; every typed key enum (SecretTokens, BotEnvironmentValues, ...)
# is a StrEnum, so it satisfies this bound - and `f"{key}"` renders the wire name, not the member.
T = TypeVar("T", bound=str)


@dataclass
class ExtractedSecret(Generic[T]):
    token_name: T
    target: Targets
    value: bytes


class SecretsIter(Generic[T]):
    def __init__(
        self,
        target: Iterable[Targets],
        secrets_dir: Path,
        tokens: Iterable[T],
        *,
        exceptions: Optional[Dict[Targets, Set[T]]] = None,
    ):
        self._target = iter(target)
        self._curr_target: Optional[Targets] = None
        self._secrets_file = None
        self._secrets_dir = secrets_dir
        self._tokens = tokens
        self._tokens_iter = iter(self._tokens)
        if exceptions is None:
            exceptions = {}
        self._exceptions = exceptions

    def __iter__(self) -> SecretsIter[T]:
        return self

    def _sops_extract(self, key: T) -> bytes:
        assert self._secrets_file is not None

        return run(
            ["sops", "-d", "--extract", f'["{key}"]', str(self._secrets_file)],
            capture=True,
            text=False,
        ).stdout

    def _next_target(self, force: bool = False) -> Targets:
        if force or self._curr_target is None:
            # Implicit behavior: this automatically raises StopIteration for us
            self._curr_target = next(self._target)
            self._secrets_file = (
                self._secrets_dir / f"{self._curr_target.value}.enc.yaml"
            )

        return self._curr_target

    def _next_token(self, force: bool = False) -> T:
        if force or self._tokens_iter is None:
            self._tokens_iter = iter(self._tokens)

        try:
            self._curr_token = next(self._tokens_iter)
        except StopIteration:
            self._next_target(True)
            # Recursion is the simplest method here,
            # this will never be more than one recursive level
            return self._next_token(True)

        return self._curr_token

    def __next__(self) -> ExtractedSecret[T]:

        value = None

        while value is None:
            token = self._next_token()
            target = self._next_target()

            if not (target in self._exceptions and token in self._exceptions[target]):
                value = self._sops_extract(token)
            else:
                continue

        assert self._curr_target is not None
        return ExtractedSecret(self._curr_token, self._curr_target, value)
