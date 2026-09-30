#!/bin/sh
# Docker exec starts this trusted helper; untrusted command text is used only after dropping rights.
set -eu
[ "$#" -eq 4 ] || exit 125
sandbox_uid=$1
sandbox_gid=$2
sandbox_seconds=$3
sandbox_command=$4
case "$sandbox_uid:$sandbox_gid:$sandbox_seconds" in *[!0-9:]*) exit 125 ;; esac
[ "$sandbox_uid" -ne 0 ] || exit 125
[ -f /tmp/kanon-ready ] || { echo 'Sandbox is still initializing' >&2; exit 125; }
exec /usr/bin/setpriv \
    --reuid="$sandbox_uid" --regid="$sandbox_gid" --clear-groups \
    --bounding-set=-all --inh-caps=-all --ambient-caps=-all --no-new-privs \
    /usr/bin/timeout --signal=TERM --kill-after=1 "$sandbox_seconds" \
    /bin/bash --noprofile --norc -o pipefail -c 'mkdir -p "$HOME"; exec /bin/bash --noprofile --norc -o pipefail -c "$1"' kanon "$sandbox_command"
