"""``provision db``: create the bot's Postgres roles and databases.

Idempotent and non-destructive: existing roles, databases, and passwords are left exactly as
they are, so this is safe to re-run against a box already serving production. It creates only
what is missing and re-asserts the (idempotent) schema grants. The migration password comes
from the same SOPS key the ``creds`` slice provisions, so the in-database role and the
credential can't drift.

Only the bot has a database, so there's a single command (no per-component split).

Box-side: escalates to root and drives psql as the postgres superuser.
"""

from __future__ import annotations

from pathlib import Path
from typing import Dict, FrozenSet, Optional

from cyclopts import App

from .. import ops
from ..ops import STAGED_SECRETS, SecretsIter
from ..defs import ALL_TARGETS, SecretTokens, Targets
from ..defs.db import DbSecretTokens

db = App(
    name="db",
    help="Create the bot's Postgres roles and databases for the membership cache.",
)

# The cluster-wide group role the migrations grant table rights to: owns nothing, can't log in.
# Per-instance runtime roles are members of it. Created once, idempotently.
_ENSURE_GROUP_ROLE = (
    "DO $$ BEGIN "
    "IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'botonio_app') THEN "
    "CREATE ROLE botonio_app NOLOGIN; "
    "END IF; END $$;"
)

# The /forget reset deletes a member's manual_override stamp, which needs DELETE on that table.
# The migrations grant only SELECT and INSERT (the stamp is meant to be permanent), and they
# can't differentiate environments since the same SQL runs against both databases - so DELETE
# is granted here for staging alone, scoped to the staging database so the privilege never
# reaches the production table even though the grantee group role is cluster-wide. Guarded on
# the table existing: on a fresh database the migrate step has not created it yet, and this
# command is meant to run before the first bot start; a later re-run applies the grant.
_STAGING_OVERRIDE_DELETE_GRANT = (
    "DO $$ BEGIN "
    "IF to_regclass('public.manual_override') IS NOT NULL THEN "
    "GRANT DELETE ON manual_override TO botonio_app; "
    "END IF; END $$;"
)


@db.default
def provision_db(
    *, targets: FrozenSet[Targets] = ALL_TARGETS, secrets_dir: Path = STAGED_SECRETS
) -> None:
    """Create the botonio_* roles and database for each instance (idempotent & non-destructive).

    Args:
        targets:     Whether we should provision the DB for staging, prod, or both.
        secrets_dir: The (admin:root owned, 0o700 mode) directory the `.enc.yaml` files exist in.
    """
    ops.prepare(secrets_dir)
    _psql("-c", _ENSURE_GROUP_ROLE)

    for target in targets:
        database = f"botonio_{target}"
        migrate = f"botonio_{target}_migrate"
        app = f"botonio_{target}_app"

        # Migration/owner role: logs in over TCP loopback with a password. Created only if
        # absent, so a re-run never rotates a live password - and the password is only
        # decrypted when we're actually about to create the role.
        if not _role_exists(migrate):
            pw = _migration_password(target, secrets_dir)
            _psql(stdin=f"CREATE ROLE {migrate} LOGIN PASSWORD {_pg_literal(pw)};\n")
            del pw

        # Runtime role: DML only, peer auth over the socket, no password. Its table rights come
        # from botonio_app (granted by the migrations), so it owns and creates nothing.
        if not _role_exists(app):
            _psql("-c", f"CREATE ROLE {app} LOGIN IN ROLE botonio_app;")

        # One database per instance, owned by the migration role.
        if not _db_exists(database):
            _psql("-c", f"CREATE DATABASE {database} OWNER {migrate};")

        # Schema grants (idempotent): the runtime role may connect and use the schema; the
        # migration role owns it and may create objects there.
        _psql("-c", "REVOKE ALL ON SCHEMA public FROM PUBLIC;", dbname=database)
        _psql("-c", f"GRANT CONNECT ON DATABASE {database} TO {app};", dbname=database)
        _psql("-c", f"GRANT USAGE  ON SCHEMA   public TO {app};", dbname=database)
        _psql("-c", f"GRANT ALL    ON SCHEMA   public TO {migrate};", dbname=database)

        # Staging alone may delete override stamps, enabling the /forget reset; production
        # withholds it so the stamp stays permanent there.
        if target == Targets.Staging:
            _psql("-c", _STAGING_OVERRIDE_DELETE_GRANT, dbname=database)

        print(f"ok: {database} (owner {migrate}, runtime {app})")


def _migration_password(target: Targets, secrets_dir: Path) -> str:
    """Decrypt this instance's db_migration_password out of its encrypted file."""
    db_tokens = {tok.value for tok in DbSecretTokens}
    secrets: Dict[SecretTokens, str] = {
        s.token_name: s.value.decode("utf-8").strip()
        for s in SecretsIter({target}, secrets_dir, db_tokens)
    }
    return secrets[SecretTokens.DbMigrationPassword]


def _psql(
    *sql_args: str, dbname: Optional[str] = None, stdin: Optional[str] = None
) -> str:
    """Run psql as the postgres superuser over the local socket (peer auth).

    ``runuser`` (not sudo) because we're already root - no sudoers dependency. ON_ERROR_STOP
    aborts on the first failed statement. ``dbname`` selects the database; ``stdin`` feeds a
    script on the pipe - used for the one statement carrying the password, so it never lands in
    the argument list.
    """
    cmd = ["runuser", "-u", "postgres", "--", "psql", "-v", "ON_ERROR_STOP=1"]
    if dbname is not None:
        cmd += ["-d", dbname]
    cmd += list(sql_args)
    return ops.run(cmd, input=stdin, capture=True).stdout.strip()


def _role_exists(name: str) -> bool:
    return bool(_psql("-tAc", f"SELECT 1 FROM pg_roles WHERE rolname = '{name}'"))


def _db_exists(name: str) -> bool:
    return bool(_psql("-tAc", f"SELECT 1 FROM pg_database WHERE datname = '{name}'"))


def _pg_literal(value: str) -> str:
    """Render a value as a PostgreSQL string literal, doubling embedded single quotes.

    Used only for the migration password (fed to psql on stdin, never argv).
    """
    return "'" + value.replace("'", "''") + "'"
