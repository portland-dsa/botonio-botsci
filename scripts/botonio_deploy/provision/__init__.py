"""The ``provision`` verb: box-side provisioning, one subcommand per layer.

Each provisioning layer is its own module and mounts its subcommand onto this app.
"""

from __future__ import annotations
from typing import FrozenSet

from cyclopts import App

from .creds import creds, all as creds_all
from .systemd import systemd, all as systemd_all
from .db import db, provision_db
from ..defs import Targets, ALL_TARGETS
from ..ops import STAGED_SECRETS

provision = App(
    name="provision",
    help="Provision the box from its encrypted secrets.",
)
provision.command(creds)
provision.command(systemd)
provision.command(db)


@provision.default
def all(*, targets: FrozenSet[Targets] = ALL_TARGETS, secrets_dir=STAGED_SECRETS):
    """Provisions the entire workspace for the Botonio Botsci infrastructure.

    This includes systemd unit files, the backup scheduler and script, and database provisioning.

    This can *only* be filtered by *target*. If you want more fine-grained control, use the subcommands directly.
    """

    creds_all(targets=targets, secrets_dir=secrets_dir)
    systemd_all(targets=targets, secrets_dir=secrets_dir)
    provision_db(targets=targets, secrets_dir=secrets_dir)
