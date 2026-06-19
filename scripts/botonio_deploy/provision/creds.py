"""``provision creds``: install systemd credentials from the encrypted secrets.

One leaf per component, because the two have different targeting:

* ``all`` - provisions all of the targets below. This behavior is also performed if no subcommands
    are passed
* ``bot`` - the per-instance discord/db tokens (and Solidarity Tech in production), provisioned
    once per ``--target``.
* ``backup`` - the single box-level b2 application key, with no instance split.

For each instance, decrypt its tokens out of ``secrets/<target>.enc.yaml`` and re-encrypt them
into systemd credentials under ``/etc/botonio-botsci/<component>/<target>/``. The plaintext flows straight
from ``sops`` into ``systemd-creds`` over a pipe and never touches argv or disk.

Box-side: escalates to root and reads real secrets, so it runs on the box, not the workstation.
"""

from __future__ import annotations

from pathlib import Path
from typing import Dict, FrozenSet, Optional, Set

from cyclopts import App

from .. import ops
from ..ops import STAGED_SECRETS
from ..defs import (
    ALL_TARGETS,
    BotSecretTokens,
    FilePermissions,
    SecretTokens,
    Targets,
    BackupSecretTokens,
)

creds = App(
    name="creds", help="Provision systemd credentials from the encrypted secrets."
)

DEFAULT_BOT_SECRETS = frozenset(set(BotSecretTokens))


def _run(
    targets: Set[Targets],
    secrets_dir: Path,
    tokens: Set[SecretTokens],
    component: str,
    *,
    exceptions: Optional[Dict[Targets, Set[SecretTokens]]] = None,
):
    ops.prepare(secrets_dir)
    _provision_instances(
        component, ops.SecretsIter(targets, secrets_dir, tokens, exceptions=exceptions)
    )


@creds.command(name="all")
@creds.default
def all(
    *,
    targets: FrozenSet[Targets] = ALL_TARGETS,
    secrets_dir: Path = ops.STAGED_SECRETS,
    bot_tokens: FrozenSet[BotSecretTokens] = DEFAULT_BOT_SECRETS,
):
    """Provisions all secret credentials for all components of the bot ecosystem.

    This provisions the secrets for every current component with secrets,
    as listed by `botonio_deploy provision creds --help`.

    If a component has only a single option, there will not be an argument for it below.

    Args:
        targets: The targets to provision. If a target does not exist for a credential
            (e.g. `backup` does not target `staging`), it will be skipped silently without error.
        bot_tokens: a list of tokens to provision for the bot component.
        secrets_dir: The (admin:root owned, 0o700 mode) directory the `.enc.yaml` files exist in.
    """

    # Both subcommands do this, but it's both not worth the effort to wire a whole argument
    # to prevent that and this explicit one guards against us removing it from the subcommands
    # for some reason later
    ops.prepare(secrets_dir)

    backup(secrets_dir=secrets_dir)
    bot(target=targets, secrets_dir=secrets_dir, tokens=bot_tokens)


@creds.command
def bot(
    *,
    target: FrozenSet[Targets] = ALL_TARGETS,
    secrets_dir: Path = STAGED_SECRETS,
    tokens: FrozenSet[BotSecretTokens] = DEFAULT_BOT_SECRETS,
) -> None:
    """
    Provision the credentials for the Discord bot for each target.

    Note that `staging` does not require a Solidarity Tech token as it uses
    a mock for testing and safety purposes.

    Args:
        target:         Which target to read the secrets for.
        secrets_dir:    The (admin:root owned, 0o700 mode) directory the `.enc.yaml` files exist in.
        tokens:         All the tokens you want to read. Invalid tokens for the given target will be safely
                            and silently ignored
    """
    exceptions = {Targets.Staging: {SecretTokens.SolidarityTechToken}}
    secret_tokens: Set[SecretTokens] = {SecretTokens(tok.value) for tok in tokens}

    _run(set(target), secrets_dir, secret_tokens, "bot", exceptions=exceptions)


@creds.command
def backup(*, secrets_dir: Path = STAGED_SECRETS) -> None:
    """
    Provision the credentials for the B2 backup service.

    Currently this only has one secret (the "b2_application_key") and only is deployed for prod,
    so those arguments are omitted for brevity.

    Args:
        secrets_dir:    The (admin:root owned, 0o700 mode) directory the `.enc.yaml` files exist in.
    """
    tokens = {SecretTokens(tok.value) for tok in BackupSecretTokens}

    _run({Targets.Production}, secrets_dir, tokens, "backup")


def _provision_instances(
    component: str, secrets_iterator: ops.SecretsIter[SecretTokens]
) -> None:
    """Decrypt one instance's tokens out of SOPS and re-encrypt each into a systemd credential."""

    for secret in secrets_iterator:
        target = secret.target

        print(f"provisioning {secret.token_name} for {component} on target {target}")

        etc = Path("/etc") / ops.ROOTNAME / component / target
        owner = ops.service(target)
        ops.install_dir(etc, FilePermissions.GroupDir, owner)
        dest = etc / f"{secret.token_name}.cred"

        ops.creds_encrypt(secret.token_name, secret.value, dest, owner)
        print(f"ok: {dest}")
