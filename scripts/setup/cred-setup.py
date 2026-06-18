#!/usr/bin/env python3
import argparse
from enum import Enum
from pathlib import PosixPath
import sys
import pwd
import os
import grp
import shutil
import subprocess
import getpass

ADMIN_GROUPS = ["root", "sudo"]

ROOTNAME = "botonio-botsci"

# If we're not root, escalate to root. Use sys.executable so this works whether the
# script was launched directly (./cred-setup.py) or via `python cred-setup.py`.
if os.geteuid() != 0:
    os.execvp("sudo", ["sudo", sys.executable, *sys.argv])

# After the sudo re-exec, $HOME may not be root's, so point sops at the box's age key
# explicitly rather than relying on ~/.config/sops/age/keys.txt resolving.
os.environ.setdefault("SOPS_AGE_KEYFILE", "/root/.config/sops/age/keys.txt")


# ===============================
# Permission Checking Functions
# ===============================
def user_in_group(uname, gname):
    primary_gid = pwd.getpwnam(uname).pw_gid
    group = grp.getgrnam(gname)

    if group.gr_gid == primary_gid:
        return True
    return uname in group.gr_mem


def group_is_admin(gname):
    return gname in ADMIN_GROUPS


def user_is_admin(uname):
    uid = pwd.getpwnam(uname).pw_uid

    if uid == 0:
        return True

    return any(map(lambda group: user_in_group(uname, group), ADMIN_GROUPS))


def file_trusted(path: PosixPath):
    own_group = path.group()
    own_user = path.owner()

    if not user_is_admin(own_user) or not group_is_admin(own_group):
        raise PermissionError(
            f"File {path} is owned by {own_user}:{own_group}, it must be owned by both an user AND group (i.e. root or a sudoer)"
        )

    if path.stat().st_mode & 0o007:
        mode = path.stat().st_mode & 0o777
        raise PermissionError(
            f"File {path} is accessible to other users (mode {mode:03o}); aborting for security"
        )

    return True


# ===============================
# Reimplement Unix Utilities Needlessly
# ===============================


def install_d(path: PosixPath, mode=0o750, uname="root", gname=None):
    """Reimplements `install -d -m {mode} -o {uname} -g {gname} {path}"""

    path.mkdir(mode=mode, parents=True, exist_ok=True)
    # Is path.mkdir blocked by umask like os.makedirs? Not sure, this is for safety
    path.chmod(mode)

    shutil.chown(path, user=uname, group=gname)


# ===============================
# Argument Custom Types
# ===============================


class Target(Enum):
    Staging = "staging"
    Production = "production"


class Tokens(Enum):
    DiscordBotToken = "discord_bot_token"
    SolidarityTechToken = "solidarity_tech_token"
    DbMigrationPassword = "db_migration_password"


class UniqueAppendAction(argparse.Action):
    def __call__(self, _parser, namespace, values, option_string=None):
        current = getattr(namespace, self.dest, None) or set()
        # `values` is a list when the option uses nargs, a single item otherwise.
        current.update(values if isinstance(values, list) else [values])
        setattr(namespace, self.dest, current)


# ===============================
# Parser Definition
# ===============================

parser = argparse.ArgumentParser(
    prog="cred-setup",
    description="Easily set up credentials for botonio-botsci without going remembering 500 basic commands",
)
parser.add_argument(
    "--target",
    type=Target,
    choices=list(Target),
    action=UniqueAppendAction,
    dest="targets",
    default=set(),
)
sops_group = parser.add_mutually_exclusive_group()

sops_group.add_argument("--from-sops", action="store_true")
sops_group.add_argument(
    "--with-token",
    type=Tokens,
    choices=list(Tokens),
    action=UniqueAppendAction,
    nargs="+",
    dest="tokens",
    default=set(),
)

parser.add_argument(
    "--secrets-dir", type=PosixPath, default=PosixPath(f"/tmp/{ROOTNAME}")
)

args = parser.parse_args()

# =========================================
# Validation
# =========================================

if len(args.targets) == 0:
    print("--target not set, assuming both staging and production")
    args.targets = set(Target)

if len(args.tokens) == 0:
    print("--with-token not set, assuming all tokens")
    args.tokens = set(Tokens)

if args.from_sops:
    args.tokens = set(Tokens)

    if not args.secrets_dir.exists():
        raise argparse.ArgumentError(None, f"{args.secrets_dir} does not exist")

    if not args.secrets_dir.is_dir():
        raise argparse.ArgumentError(None, f"{args.secrets_dir} is not a directory")

    # Technically just `file_trusted(secrets_dir)` works, it's a zero-side-effect
    # function that raises if the file *isn't* trusted, but since we're checking something
    # it makes sense to wrap it in an `if` so the fact a check is happening stands out more.
    # like, would you have stopped to read this comment and know it's a stealth check
    # without this block being here?
    if file_trusted(args.secrets_dir):
        pass

# =========================================
# Main loop
# =========================================

for target in map(lambda x: x.value, args.targets):
    deploygroup = f"{ROOTNAME}-{target}"
    path = PosixPath("/etc") / ROOTNAME / target
    install_d(path, gname=deploygroup)

    for token in map(lambda x: x.value, args.tokens):

        if target == Target.Staging and token == Tokens.SolidarityTechToken:
            print(
                "Ignoring Solidarity Tech Token for target: {target} - it's unused because we use a mock for testing and safety purposes"
            )
            continue

        cred_path = path / f"{token}.cred"

        if args.from_sops:
            # Decrypt straight into memory, then into systemd-creds - never to disk.
            decrypted = subprocess.run(
                [
                    "sops",
                    "-d",
                    "--extract",
                    f'["{token}"]',
                    args.secrets_dir / f"{target}.enc.yaml",
                ],
                capture_output=True,
                check=True,
            ).stdout
        else:
            decrypted = getpass.getpass(prompt=f"{token} ({target}): ").encode()

        subprocess.run(
            [
                "systemd-creds",
                "encrypt",
                f"--name={token}",
                "--with-key=host",
                "-",
                cred_path,
            ],
            input=decrypted,
            check=True,
        )

        cred_path.chmod(0o600)
