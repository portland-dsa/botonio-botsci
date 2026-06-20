"""``provision systemd``: write the systemd unit files from their bundled templates.

For each component, render the per-target ``override.conf`` (bot) or service unit (backup) by
filling the managed ``Environment=`` lines from the non-secret values in the encrypted file (the
``*EnvironmentValues`` allowlists in ``defs``), then install the static units that need no
templating. The values flow ``sops`` -> render -> unit file; only the template's ``${...}``
placeholders are substituted.

Box-side: escalates to root and reads the encrypted file, so it runs on the box.
"""

from __future__ import annotations

from pathlib import Path
from typing import Callable, Dict, FrozenSet
from importlib import resources
from string import Template

from cyclopts import App

from .. import ops
from ..ops import STAGED_SECRETS, SecretsIter
from ..defs import ALL_TARGETS, EnvVars, FilePermissions, Targets
from ..defs.bot import BotEnvironmentValues
from ..defs.backup import BackupEnvironmentValues

systemd = App(
    name="systemd", help="Write the systemd unit override files from their templates."
)

SYSTEMD_CONF_PATH = Path("/etc/systemd/system")
SBIN_PATH = Path("/usr/local/sbin")

DEFAULT_BOT_VARS = frozenset(BotEnvironmentValues)
DEFAULT_BACKUP_VARS = frozenset(BackupEnvironmentValues)


def _render(
    component: str,
    env_iter: SecretsIter[EnvVars],
    find_unit_path: Callable[[Targets], Path],
    find_template: Callable[[Targets], str],
) -> None:
    """Render each target's unit file from its template, filling the decrypted env values in."""
    values = _collect(component, env_iter)

    for target, env in values.items():
        unit_path = find_unit_path(target)
        ops.install_dir(unit_path.parent, FilePermissions.WorldDir)

        template_text = (
            resources.files("botonio_deploy")
            .joinpath(find_template(target))
            .read_text(encoding="utf-8")
        )
        rendered = Template(template_text).substitute(env)
        ops.install_file(rendered.encode("utf-8"), unit_path, FilePermissions.WorldConfig)
        print(f"ok: {unit_path}")


def _collect(
    component: str, env_iter: SecretsIter[EnvVars]
) -> Dict[Targets, Dict[str, str]]:
    """Group the decrypted env values by target into ``{target: {key: value}}`` mappings."""
    values: Dict[Targets, Dict[str, str]] = {}

    # These aren't really secrets, just encrypted.
    for decrypted in env_iter:
        print(f"reading {decrypted.token_name} for {component} on target {decrypted.target}")
        per_target = values.setdefault(decrypted.target, {})
        per_target[str(decrypted.token_name)] = decrypted.value.decode("utf-8").strip()

    return values


def _install_static(
    name: str, dest: Path, perms: FilePermissions = FilePermissions.WorldConfig
) -> None:
    """Copy a bundled static file (no templating) into place at ``perms``, root-owned."""
    data = resources.files("botonio_deploy").joinpath(f"assets/{name}").read_bytes()
    ops.install_file(data, dest, perms)
    print(f"ok: {dest}")


@systemd.command(name="all")
@systemd.default
def all(
    *,
    targets: FrozenSet[Targets] = ALL_TARGETS,
    secrets_dir: Path = STAGED_SECRETS,
    bot_vars: FrozenSet[BotEnvironmentValues] = DEFAULT_BOT_VARS,
    backup_vars: FrozenSet[BackupEnvironmentValues] = DEFAULT_BACKUP_VARS,
) -> None:
    """Write the unit files for every component (also the default with no subcommand)."""
    ops.prepare(secrets_dir)

    bot(targets=targets, secrets_dir=secrets_dir, vars=bot_vars)
    # The backup unit is production-only; skip it unless production is in scope.
    if Targets.Production in targets:
        backup(secrets_dir=secrets_dir, vars=backup_vars)


@systemd.command
def bot(
    *,
    targets: FrozenSet[Targets] = ALL_TARGETS,
    secrets_dir: Path = STAGED_SECRETS,
    vars: FrozenSet[BotEnvironmentValues] = DEFAULT_BOT_VARS,
) -> None:
    """Render each bot instance's override.conf and install the shared service unit."""
    BOT_BASENAME = "botonio-botsci@"
    ops.prepare(secrets_dir)

    def _find_unit_path(target: Targets) -> Path:
        return SYSTEMD_CONF_PATH / f"{BOT_BASENAME}{target}.service.d" / "override.conf"

    def _find_template(target: Targets) -> str:
        return f"assets/{BOT_BASENAME}{target}.service.d/override.conf.tmpl"

    # The personas are staging-mock-only, so production has no values for them.
    exceptions = {
        Targets.Production: {
            EnvVars.GoodStandingUserId,
            EnvVars.ExpiringUserId,
            EnvVars.LapsedUserId,
        }
    }

    vars_as_env = {EnvVars(var) for var in vars}
    env_iter = SecretsIter(targets, secrets_dir, vars_as_env, exceptions=exceptions)

    _render("bot", env_iter, _find_unit_path, _find_template)
    _install_static(f"{BOT_BASENAME}.service", SYSTEMD_CONF_PATH / f"{BOT_BASENAME}.service")
    ops.daemon_reload()


@systemd.command
def backup(
    *,
    secrets_dir: Path = STAGED_SECRETS,
    vars: FrozenSet[BackupEnvironmentValues] = DEFAULT_BACKUP_VARS,
) -> None:
    """Render the backup service unit and install its timer."""
    BACKUP_BASENAME = "botonio-db-backup"
    ops.prepare(secrets_dir)

    def _find_unit_path(_: Targets) -> Path:
        return SYSTEMD_CONF_PATH / f"{BACKUP_BASENAME}.service"

    def _find_template(_: Targets) -> str:
        return f"assets/{BACKUP_BASENAME}.service.tmpl"

    vars_as_env = {EnvVars(var) for var in vars}
    env_iter = SecretsIter({Targets.Production}, secrets_dir, vars_as_env)

    _render("backup", env_iter, _find_unit_path, _find_template)
    _install_static(f"{BACKUP_BASENAME}.timer", SYSTEMD_CONF_PATH / f"{BACKUP_BASENAME}.timer")
    # The executable the service runs (root, 0700 - lives under /usr/local/sbin, not a unit).
    _install_static(
        f"{BACKUP_BASENAME}.py", SBIN_PATH / BACKUP_BASENAME, FilePermissions.PrivateDir
    )
    ops.daemon_reload()
