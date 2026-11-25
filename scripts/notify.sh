#!/usr/bin/env bash
# Sends a push notification to a phone through ntfy (https://ntfy.sh, or a
# self-hosted ntfy server).
#
# Usage: notify.sh <title> <message> [priority]
#   priority  min | low | default | high | urgent   (default: default)
#
# The address is read from notify.conf in the project folder, which is not
# committed because anyone who knows it can read and send the notifications:
#   NTFY_URL=https://ntfy.sh/<a long random topic name>
# Without that file nothing is sent. A failed send is reported but never stops
# the caller, so a notification problem cannot break a build.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
conf="$root/notify.conf"
[[ -f "$conf" ]] || exit 0
# shellcheck source=/dev/null
source "$conf"
[[ -n "${NTFY_URL:-}" ]] || exit 0

curl -fsS --max-time 20 --retry 2 -o /dev/null \
  -H "Title: $1" \
  -H "Priority: ${3:-default}" \
  -d "$2" \
  "$NTFY_URL" || echo "WARNING: could not send the notification" >&2
exit 0
