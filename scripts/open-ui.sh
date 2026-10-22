#!/usr/bin/env bash
# Opens the read-only dashboard (`ui serve --attach`) signed in, on hub.localhost.
#
#   scripts/open-ui.sh [--profile NAME] [--port PORT] [--print]
#
# The sign-in token is read from the profile's ui_token file and is only passed to the browser.
# --print shows the URL without the token, for scripts and bug reports.
set -euo pipefail

PROFILE="default"
PORT="8788"
PRINT=0
HOST_NAME="hub.localhost"

while [ $# -gt 0 ]; do
  case "$1" in
    --profile) PROFILE="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    --print) PRINT=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

TOKEN_FILE="$HOME/Library/Application Support/telegram-captain/$PROFILE/ui_token"
[ -r "$TOKEN_FILE" ] || { echo "no ui_token for profile $PROFILE at $TOKEN_FILE" >&2; exit 1; }
BASE="http://$HOST_NAME:$PORT/"
if [ "$PRINT" -eq 1 ]; then
  echo "$BASE"
  exit 0
fi
open "$BASE?token=$(tr -d '\n' < "$TOKEN_FILE")"
