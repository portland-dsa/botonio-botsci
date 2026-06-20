"""Command-line surface (cyclopts): the root ``botonio-deploy`` app.

This only defines the top-level app and mounts the verb subapps, so it stays small.
Each verb's handlers and logic live under its own package (``provision/``).
"""

from __future__ import annotations

from cyclopts import App

from .provision import provision
from .redeploy import redeploy

app = App(
    name="botonio-deploy",
    help="Easy setup tool for updating the bot's creds, DB, and other system-level setup changes",
)
app.command(provision)
app.command(redeploy)


def main() -> None:
    app()
