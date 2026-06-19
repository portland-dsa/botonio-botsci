#!/usr/bin/env python3
#
# Create the botonio_* PostgreSQL roles and databases that back the membership cache.
# Run this instead of pasting the role/database SQL into a psql session by hand.
#
# It is idempotent and NON-DESTRUCTIVE: existing roles, databases, and passwords are left
# exactly as they are, so it is safe to re-run and safe to run on a box that is already
# serving production. It only creates what is missing and re-asserts the (idempotent)
# schema grants.
#
# Run on the box, with postgresql running (it re-execs itself under sudo if it is not
# already root, because it drives psql as the postgres superuser):
#
#   db-bootstrap.py                       # both instances (staging + production)
#   db-bootstrap.py staging               # just one
#
# Each migration role that gets CREATED needs a password (it logs in over TCP loopback with
# scram-sha-256). The runtime (_app) role needs no password at all - it authenticates by
# peer over the Unix socket. Two ways to supply the migration password:
#
#   --from-sops   read db_migration_password from secrets/<target>.enc.yaml, so it matches
#                 the systemd credential cred-setup.py provisions from the same file.
#   (default)     prompt for it (no echo) only when a migration role is actually created.
#
# The password reaches psql over stdin, never as a process argument and never on disk.
import argparse
from enum import Enum
from pathlib import PosixPath
import sys
import os
import subprocess
import getpass

ROOTNAME = "botonio-botsci"

# Drive psql as the postgres superuser, which needs root (it shells out to runuser). Re-exec
# under sudo if we are not root yet, matching cred-setup.py so the two read the same way.
if os.geteuid() != 0:
    os.execvp("sudo", ["sudo", sys.executable, *sys.argv])

# After the sudo re-exec $HOME may not be root's, so point sops at the box's age key
# explicitly rather than relying on ~/.config/sops/age/keys.txt resolving.
os.environ.setdefault("SOPS_AGE_KEYFILE", "/root/.config/sops/age/keys.txt")


class Target(Enum):
    Staging = "staging"
    Production = "production"


# ===============================
# psql plumbing
# ===============================


def pg(*sql_args, db=None, stdin=None):
    """Run psql as the postgres superuser over the local socket (peer auth).

    runuser (not sudo) because this process is already root - no sudoers dependency and no
    PAM/TTY surprises, matching bot-db-backup.py. ON_ERROR_STOP makes a failed statement
    abort the run instead of limping on. `db` selects the database; `stdin` feeds a script
    on the pipe (used for the one statement that carries a password, so it never lands in
    the argument list). Returns the captured stdout, stripped.
    """

    cmd = ["runuser", "-u", "postgres", "--", "psql", "-v", "ON_ERROR_STOP=1"]
    if db is not None:
        cmd += ["-d", db]
    cmd += list(sql_args)

    completed = subprocess.run(
        cmd,
        input=stdin,
        capture_output=True,
        text=True,
        check=True,
    )
    return completed.stdout.strip()


def role_exists(name):
    return bool(pg("-tAc", f"SELECT 1 FROM pg_roles    WHERE rolname = '{name}'"))


def db_exists(name):
    return bool(pg("-tAc", f"SELECT 1 FROM pg_database WHERE datname = '{name}'"))


def pg_literal(value):
    """Render a value as a PostgreSQL string literal, doubling any embedded single quotes.

    Used only for the migration password, which is fed to psql on stdin (not argv), so the
    secret never appears in the process arguments or on disk.
    """

    return "'" + value.replace("'", "''") + "'"


# ===============================
# Password sourcing
# ===============================


def migration_password(target, from_sops, secrets_dir):
    """The password for a migration role that is about to be created.

    With --from-sops, decrypt db_migration_password straight out of the target's encrypted
    file into memory; otherwise prompt without echo. Only called when the role does not yet
    exist, so a re-run never rotates a live password.
    """

    if from_sops:
        decrypted = subprocess.run(
            [
                "sops",
                "-d",
                "--extract",
                '["db_migration_password"]',
                str(secrets_dir / f"{target}.enc.yaml"),
            ],
            capture_output=True,
            check=True,
        ).stdout
        return decrypted.decode().strip("\n")

    return getpass.getpass(prompt=f"migration password for botonio_{target}_migrate: ")


# ===============================
# Parser
# ===============================

parser = argparse.ArgumentParser(
    prog="db-bootstrap",
    description="Create the botonio_* roles and databases for the membership cache.",
)
parser.add_argument(
    "targets",
    nargs="*",
    type=Target,
    choices=list(Target),
    default=list(Target),
    help="instances to provision (default: both staging and production)",
)
parser.add_argument(
    "--from-sops",
    action="store_true",
    help="read each migration password from secrets/<target>.enc.yaml instead of prompting",
)
parser.add_argument(
    "--secrets-dir",
    type=PosixPath,
    default=PosixPath(f"/tmp/{ROOTNAME}"),
    help="directory holding <target>.enc.yaml when --from-sops is set",
)

args = parser.parse_args()
targets = args.targets if args.targets else list(Target)

# ===============================
# Provisioning
# ===============================

# The cluster-wide group role the migrations grant to: created once, owns nothing, no login.
# Per-instance runtime roles are members of it.
pg(
    "-c",
    "DO $$ BEGIN "
    "IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'botonio_app') THEN "
    "CREATE ROLE botonio_app NOLOGIN; "
    "END IF; END $$;",
)

for target in (t.value for t in targets):
    db = f"botonio_{target}"
    migrate = f"botonio_{target}_migrate"
    app = f"botonio_{target}_app"

    # Migration/owner role: logs in over TCP loopback with a password. Created only if
    # absent - re-running never rotates an existing role's password.
    if not role_exists(migrate):
        pw = migration_password(target, args.from_sops, args.secrets_dir)
        pg(stdin=f"CREATE ROLE {migrate} LOGIN PASSWORD {pg_literal(pw)};\n")
        del pw

    # Runtime role: DML only, peer over the socket, no password. Its table rights come from
    # botonio_app (granted by the migrations), so it owns and creates nothing.
    if not role_exists(app):
        pg("-c", f"CREATE ROLE {app} LOGIN IN ROLE botonio_app;")

    # One database per instance, owned by the migration role.
    if not db_exists(db):
        pg("-c", f"CREATE DATABASE {db} OWNER {migrate};")

    # Schema grants (idempotent): the runtime role may connect and use the schema; the
    # migration role owns it and may create objects there.
    pg("-c", "REVOKE ALL ON SCHEMA public FROM PUBLIC;", db=db)
    pg("-c", f"GRANT CONNECT ON DATABASE {db} TO {app};", db=db)
    pg("-c", f"GRANT USAGE  ON SCHEMA   public TO {app};", db=db)
    pg("-c", f"GRANT ALL    ON SCHEMA   public TO {migrate};", db=db)

    print(f"ok: {db} (owner {migrate}, runtime {app})")
