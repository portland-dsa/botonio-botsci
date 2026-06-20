"""The ``provision`` verb: box-side provisioning, one subcommand per layer.

Each provisioning layer is its own module and mounts its subcommand onto this app.
"""

from __future__ import annotations

import shutil
from importlib import resources
from pathlib import Path
from typing import FrozenSet

from cyclopts import App

from .creds import creds, all as creds_all
from .systemd import systemd, all as systemd_all
from .db import db, provision_db
from .. import ops
from ..defs import ALL_TARGETS, FilePermissions, Targets
from ..ops import STAGED_SECRETS

provision = App(
    name="provision",
    help="Provision the box from its encrypted secrets.",
)
provision.command(creds)
provision.command(systemd)
provision.command(db)


@provision.default
def all(
    *,
    targets: FrozenSet[Targets] = ALL_TARGETS,
    secrets_dir: Path = STAGED_SECRETS,
    bundled_secrets: bool = False,
    self_destruct: bool = False,
) -> None:
    """Provisions the entire workspace for the Botonio Botsci infrastructure.

    This includes systemd unit files, the backup scheduler and script, and database provisioning.

    This can *only* be filtered by *target*. If you want more fine-grained control, use the
    subcommands directly. With ``--bundled-secrets`` the encrypted secrets shipped inside this
    archive are unpacked into ``secrets_dir`` first - used by ``redeploy``, which bundles them
    into the .pyz rather than copying them across separately. With ``--self-destruct``,
    ``secrets_dir`` is removed afterward (even on failure) - the cleanup half of that same
    ephemeral flow.
    """
    if bundled_secrets:
        _extract_bundled_secrets(targets, secrets_dir)

    try:
        creds_all(targets=targets, secrets_dir=secrets_dir)
        systemd_all(targets=targets, secrets_dir=secrets_dir)
        provision_db(targets=targets, secrets_dir=secrets_dir)
    finally:
        if self_destruct:
            shutil.rmtree(secrets_dir, ignore_errors=True)
            print(f"self-destruct: removed {secrets_dir}")


def _extract_bundled_secrets(targets: FrozenSet[Targets], secrets_dir: Path) -> None:
    """Unpack the per-target ``.enc.yaml`` bundled into this archive into ``secrets_dir``.

    Escalates first so the directory is created root:root 0700 - which ``ops.assert_trusted``
    (run by each layer) then accepts. Only the requested targets are materialized.
    """
    ops.ensure_root()
    secrets_dir = Path(secrets_dir)
    ops.install_dir(secrets_dir, FilePermissions.PrivateDir)

    bundle = resources.files("botonio_deploy").joinpath(ops.BUNDLED_SECRETS_DIR)
    for target in targets:
        name = f"{target}.enc.yaml"
        ops.install_file(
            bundle.joinpath(name).read_bytes(),
            secrets_dir / name,
            FilePermissions.Private,
        )
        print(f"extracted bundled secret -> {secrets_dir / name}")
