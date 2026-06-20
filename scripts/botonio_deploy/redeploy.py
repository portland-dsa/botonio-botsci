from pathlib import Path
import shutil
from typing import FrozenSet, Optional, Tuple
import zipapp
import zipfile
import sys
from tempfile import TemporaryDirectory

from cyclopts import App

from . import ops
from .defs import ALL_TARGETS, Targets

redeploy = App(
    name="redeploy",
    help="Bundle this tool and its encrypted secrets into a .pyz, ship it to the box, and re-provision over SSH.",
)


def _bundle() -> Tuple[Path, TemporaryDirectory]:
    """Build a fresh .pyz (with the current encrypted secrets) in a temp dir, returned for cleanup.

    Refuses to run from a prebuilt .pyz: it can only bundle the *current* secrets from a source
    checkout, and a running archive would ship whatever was baked in at its own build time.
    """

    if zipfile.is_zipfile(sys.argv[0]):
        raise RuntimeError(
            "redeploy must run from a source checkout, not a prebuilt .pyz: it bundles the "
            "current encrypted secrets into a fresh archive, which a running .pyz cannot do."
        )

    project = Path(__file__).resolve().parent
    repo_root = project.parents[1]
    out = TemporaryDirectory(prefix="botonio-deploy-pyz-")
    pyz = Path(out.name) / "botonio-deploy.pyz"

    with TemporaryDirectory(prefix="botonio-deploy-src-") as src:
        pkg = Path(src) / "botonio_deploy"
        ops.run(["uv", "pip", "install", "--target", src, "cyclopts"], cwd=project)
        shutil.copytree(
            project,
            pkg,
            ignore=shutil.ignore_patterns(".venv", "__pycache__", "*.pyz"),
        )
        # Bundle the encrypted secrets so `provision --bundled-secrets` can self-extract them on
        # the box - it ships the ciphertext inside the .pyz instead of copying it across separately.
        bundled = pkg / ops.BUNDLED_SECRETS_DIR
        bundled.mkdir()
        for enc in (repo_root / "secrets").glob("*.enc.yaml"):
            shutil.copy2(enc, bundled / enc.name)

        zipapp.create_archive(
            src,
            target=pyz,
            main="botonio_deploy.cli:main",
            interpreter="/usr/bin/env python3",
            compressed=True,
        )

    return pyz, out


@redeploy.default
def _run(
    *,
    host: str,
    user: Optional[str] = None,
    targets: FrozenSet[Targets] = ALL_TARGETS,
) -> None:
    """Bundle this tool (with the encrypted secrets) and provision ``host`` over SSH.

    Workstation-side: ships only the self-contained .pyz into a fresh per-run ``mktemp`` staging
    dir, then runs ``provision --bundled-secrets --self-destruct`` under sudo on the box. No
    secret travels separately; both the extracted secrets and the shipped .pyz are wiped on the
    way out.
    """
    dest = f"{user}@{host}" if user is not None else host
    pyz, tmp = _bundle()
    try:
        # A per-invocation random staging dir (mktemp: mode 0700, login-owned, unpredictable
        # name) so no other unprivileged account on the box can pre-create or swap the .pyz that
        # is then executed under sudo - a fixed /tmp path would be a local-privesc foothold.
        stage = ops.run(["ssh", dest, "mktemp -d"], capture=True).stdout.strip()
        ops.scp(pyz, host, user=user, target_dir=stage)

        targets_flag = " ".join(f"--targets {t.value}" for t in targets)
        remote = (
            f'sudo python3 "{stage}/{pyz.name}" provision '
            f"--bundled-secrets --self-destruct {targets_flag}"
            f'; rc=$?; rm -rf -- "{stage}"; exit $rc'
        )
        # -tt forces a remote TTY so sudo can prompt from your terminal (needs NOPASSWD to run
        # unattended).
        ops.run(["ssh", "-tt", dest, remote])
    finally:
        tmp.cleanup()
