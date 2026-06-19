#!/usr/bin/env python3
import argparse
import shutil
import subprocess
import sys
from enum import Enum
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
REMOTE_SECRETS = "/tmp/botonio-botsci"
REMOTE_STAGE = "/tmp/botonio-botsci-provision"


class Target(Enum):
    Staging = "staging"
    Production = "production"


def run(cmd, *, input_text=None, dry_run=False, cwd=None):
    """Run a local command (ssh/scp), echoing it first; obey --dry-run."""

    print("+ " + " ".join(cmd))
    if dry_run:
        if input_text:
            print("--- stdin ---\n" + input_text + "------------")
        return
    subprocess.run(cmd, input=input_text, text=True, check=True, cwd=cwd)


def remote_script(targets, restart):
    """The single root shell snippet run on the box, after the files are staged.

    A trap cleans up the staged tree and the shipped ciphertext on the way out, even if a
    step fails. The setup scripts are invoked via python3 so neither an executable bit nor a
    shebang has to survive the copy.
    """

    targets_sh = " ".join(t.value for t in targets)
    lifecycle = "restart" if restart else "start"
    return f"""set -eu
stage="{REMOTE_STAGE}"
secrets="{REMOTE_SECRETS}"
trap 'rm -rf "$stage" "$secrets"' EXIT

for c in python3 psql sops systemd-creds systemctl install; do
  command -v "$c" >/dev/null || {{ echo "missing on box: $c" >&2; exit 1; }}
done

# Unit files and the backup script (static - no rendering needed).
install -D -m0644 "$stage/deploy/systemd/botonio-botsci@.service" /etc/systemd/system/botonio-botsci@.service
install -D -m0644 "$stage/deploy/systemd/bot-db-backup.service"    /etc/systemd/system/bot-db-backup.service
install -D -m0644 "$stage/deploy/systemd/bot-db-backup.timer"     /etc/systemd/system/bot-db-backup.timer
install -m0700 "$stage/scripts/ops/bot-db-backup.py" /usr/local/sbin/bot-db-backup

# PostgreSQL config, only if the cluster is present (listen_addresses/port changes still need
# a manual `systemctl restart postgresql@18-main`; a reload covers pg_hba/pg_ident).
if [ -d /etc/postgresql/18/main/conf.d ]; then
  install -D -m0644 "$stage/deploy/postgres/conf.d/10-botonio.conf" /etc/postgresql/18/main/conf.d/10-botonio.conf
  install -D -m0640 -o postgres -g postgres "$stage/deploy/postgres/pg_hba.conf"   /etc/postgresql/18/main/pg_hba.conf
  install -D -m0640 -o postgres -g postgres "$stage/deploy/postgres/pg_ident.conf" /etc/postgresql/18/main/pg_ident.conf
  systemctl reload postgresql@18-main || true
fi

for t in {targets_sh}; do
  python3 "$stage/scripts/setup/cred-setup.py"   --from-sops --secrets-dir "$secrets" --target "$t"
  python3 "$stage/scripts/setup/db-bootstrap.py" --from-sops --secrets-dir "$secrets" "$t"
  python3 "$stage/scripts/setup/unit-config.py"  --secrets-dir "$secrets" --source-dir "$stage/deploy/systemd" --target "$t"
done

systemctl daemon-reload
for t in {targets_sh}; do
  systemctl enable "botonio-botsci@$t"
  systemctl {lifecycle} "botonio-botsci@$t"
  echo "ok: botonio-botsci@$t {lifecycle}ed"
done
"""


# ===============================
# Parser
# ===============================

parser = argparse.ArgumentParser(
    prog="redeploy",
    description="Provision and restart a botonio-botsci box over SSH from the encrypted secrets.",
)
parser.add_argument(
    "--host", default="pdx-dsa-hetzner", help="SSH host (default: pdx-dsa-hetzner)"
)
parser.add_argument("--user", default="root", help="SSH user (default: root)")
parser.add_argument(
    "--target",
    type=Target,
    choices=list(Target),
    action="append",
    dest="targets",
    help="instance to provision (repeatable; default: both)",
)
parser.add_argument(
    "--secrets-dir",
    type=Path,
    default=REPO_ROOT / "secrets",
    help="local directory holding <target>.enc.yaml",
)
parser.add_argument(
    "--no-restart", action="store_true", help="start (not restart) the services"
)
parser.add_argument(
    "--dry-run", action="store_true", help="print every command without running it"
)

args = parser.parse_args()
targets = args.targets if args.targets else list(Target)

# ===============================
# Preflight (local)
# ===============================

for tool in ("ssh", "scp"):
    if shutil.which(tool) is None:
        sys.exit(f"{tool} not found on PATH")

for target in targets:
    enc = args.secrets_dir / f"{target.value}.enc.yaml"
    if not enc.is_file():
        sys.exit(f"missing encrypted secrets: {enc}")

dest = f"{args.user}@{args.host}"
ssh = ["ssh", "-T", dest]

# ===============================
# Stage -> run -> (trap) cleanup
# ===============================

# Fresh staging dir, plus the root-owned 0700 secrets dir the setup scripts insist on.
run(
    ssh
    + [
        f"rm -rf {REMOTE_STAGE} && mkdir -p {REMOTE_STAGE} "
        f"&& install -d -m700 {REMOTE_SECRETS}"
    ],
    dry_run=args.dry_run,
)

# Relative paths (cwd=repo root) so a Windows drive letter never looks like an scp host:path.
secrets_rel = (
    args.secrets_dir.relative_to(REPO_ROOT)
    if args.secrets_dir.is_relative_to(REPO_ROOT)
    else args.secrets_dir
)
for target in targets:
    run(
        [
            "scp",
            str((secrets_rel / f"{target.value}.enc.yaml").as_posix()),
            f"{dest}:{REMOTE_SECRETS}/",
        ],
        dry_run=args.dry_run,
        cwd=REPO_ROOT,
    )
run(
    ["scp", "-r", "scripts", "deploy", f"{dest}:{REMOTE_STAGE}/"],
    dry_run=args.dry_run,
    cwd=REPO_ROOT,
)

run(
    ssh + ["bash", "-s"],
    input_text=remote_script(targets, restart=not args.no_restart),
    dry_run=args.dry_run,
)

print("done.")
