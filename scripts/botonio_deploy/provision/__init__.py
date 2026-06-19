"""The ``provision`` verb: box-side provisioning, one subcommand per layer.

Each provisioning layer is its own module and mounts its subcommand onto this app.
"""

from __future__ import annotations

from cyclopts import App

from .creds import creds
from .systemd import systemd
from .db import db

provision = App(
    name="provision",
    help="Provision the box from its encrypted secrets.",
)
provision.command(creds)
provision.command(systemd)
provision.command(db)
