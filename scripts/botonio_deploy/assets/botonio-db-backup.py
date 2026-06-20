#!/usr/bin/env python3

# Nightly encrypted, off-box backup of the production membership database.
#
# The dump is age-encrypted ON the box before it is written or uploaded, so neither the
# local disk nor the Backblaze B2 bucket ever holds plaintext - decryption needs the box age
# private key (offline-backed), which never leaves the box. It runs from the
# bot-db-backup.service timer, which delivers the B2 application key as a systemd credential
# and runs this as root (so the runuser drop to postgres for pg_dump works); it is not meant
# to be run by hand.
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


def require_env(name, hint):
    value = os.environ.get(name)
    if not value:
        sys.exit(f"{name} is not set ({hint})")
    return value


def require_cmd(name, hint):
    if shutil.which(name) is None:
        sys.exit(f"{name} not installed ({hint})")


# Non-secret config, from the unit's Environment=.
recipient = require_env(
    "BACKUP_AGE_RECIPIENT", "set to the box age recipient (age1...)"
)
bucket = require_env("B2_BUCKET", "set to the destination B2 bucket name")
key_id = require_env("B2_KEY_ID", "set to the Backblaze application keyID")
db = os.environ.get("BACKUP_DB", "botonio_production")
prefix = os.environ.get("B2_PREFIX", "botonio-botsci")
keep_days = int(os.environ.get("BACKUP_KEEP_DAYS", "14"))

require_cmd("age", "apt install age")
require_cmd("rclone", "apt install rclone")

# The B2 application key is a systemd credential, never the environment or the unit file.
creds = os.environ.get("CREDENTIALS_DIRECTORY")
if not creds:
    sys.exit(
        "CREDENTIALS_DIRECTORY is not set (must run under systemd with LoadCredentialEncrypted)"
    )
b2_app_key = (Path(creds) / "b2_application_key").read_text().strip("\n")

dest = Path("/var/backups/botonio-botsci")
dest.mkdir(mode=0o700, parents=True, exist_ok=True)
dest.chmod(0o700)
stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
out_file = dest / f"{db}-{stamp}.dump.age"

# Dump straight into age - plaintext only ever exists on the pipe, never on disk. runuser
# drops to the postgres superuser so pg_dump connects by peer over the local socket.
dump = subprocess.Popen(
    ["runuser", "-u", "postgres", "--", "pg_dump", "--format=custom", db],
    stdout=subprocess.PIPE,
)
encrypt = subprocess.Popen(
    ["age", "-r", recipient, "-o", str(out_file)],
    stdin=dump.stdout,
)
# Let pg_dump receive SIGPIPE if age dies, then collect both exit codes.
dump.stdout.close()
encrypt_rc = encrypt.wait()
dump_rc = dump.wait()
if dump_rc != 0:
    sys.exit(f"pg_dump failed (exit {dump_rc})")
if encrypt_rc != 0:
    sys.exit(f"age failed (exit {encrypt_rc})")
out_file.chmod(0o600)

# Local retention: keep the most recent keep_days dailies. Names are constant-prefix +
# ISO UTC stamp, so a reverse lexical sort is newest-first.
dailies = sorted(dest.glob(f"{db}-*.dump.age"), reverse=True)
for stale in dailies[keep_days:]:
    stale.unlink()

# Off-box copy to B2 (ciphertext only). rclone takes its whole remote from the environment,
# so no rclone.conf on disk holds the key.
rclone_env = {
    **os.environ,
    "RCLONE_CONFIG_OFFSITE_TYPE": "b2",
    "RCLONE_CONFIG_OFFSITE_ACCOUNT": key_id,
    "RCLONE_CONFIG_OFFSITE_KEY": b2_app_key,
}
remote = f"offsite:{bucket}/{prefix}/"
subprocess.run(
    ["rclone", "copy", str(out_file), remote, "--no-traverse"],
    env=rclone_env,
    check=True,
)
# Mirror the local retention window in the bucket.
subprocess.run(
    ["rclone", "delete", "--min-age", f"{keep_days}d", remote],
    env=rclone_env,
    check=True,
)

print(f"backed up {db} -> {out_file} and {remote}")
