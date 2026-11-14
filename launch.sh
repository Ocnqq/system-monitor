#!/usr/bin/env bash
set -euo pipefail

bin_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
app_path="$bin_dir/system-monitor-overlay"
scope="app-io.github.systemmonitor.overlay-$$.scope"

exec systemd-run --user --scope --slice=app.slice --unit="$scope" "$app_path" "$@"
