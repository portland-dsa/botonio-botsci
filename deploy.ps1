#!/usr/bin/env bash

# This is a jank polyglot file, but you can run it on powershell
# OR bash (if you chmod u+x or `bash` it directly)

echo --% >/dev/null;: ' | out-null
<#'
# ---- bash ----
D="$(cd "$(dirname "$0")" && pwd)/scripts/botonio_deploy"
exec uv run --project "$D" "$D" "$@"
exit #>
# ---- powershell ----
$D = "$PSScriptRoot\scripts\botonio_deploy"
& uv run --project $D $D @args
