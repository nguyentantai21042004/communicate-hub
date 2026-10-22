#!/usr/bin/env bash
# Writes a launchd LaunchAgent for `captain hub run`.
#
#   scripts/install-hub-agent.sh [--profile NAME] [--binary PATH] [--dry-run] [--ui]
#
# --ui writes the agent for the read-only dashboard (`ui serve --attach`, label
# dev.tantai.comms-hub.ui) instead of the bridge.
#
# The label is dev.tantai.comms-hub.bridge. Logs go to ~/Library/Logs/comms-hub.
# The script writes the plist and does not bootstrap it. --dry-run prints the plist
# and the launchctl commands, and writes nothing. The owner runs launchctl.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="default"
BINARY="$ROOT/target/release/captain"
DRY=0
UI=0
LABEL="dev.tantai.comms-hub.bridge"
LOG_DIR="$HOME/Library/Logs/comms-hub"
THROTTLE_SECS=30
# launchd starts agents with a minimal PATH, so the sidecar would not find `node` (nvm) or `ffmpeg`.
NODE_DIR="$(dirname "$(command -v node 2>/dev/null || echo /usr/local/bin/node)")"
AGENT_PATH="$NODE_DIR:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"

while [ $# -gt 0 ]; do
  case "$1" in
    --profile) PROFILE="$2"; shift 2 ;;
    --binary) BINARY="$2"; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    --ui) UI=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ "$UI" -eq 1 ]; then
  LABEL="dev.tantai.comms-hub.ui"
  STDOUT_LOG="$LOG_DIR/ui.stdout.log"
  STDERR_LOG="$LOG_DIR/ui.stderr.log"
  COMMAND_ARGS="    <string>ui</string>
    <string>serve</string>
    <string>--attach</string>"
else
  STDOUT_LOG="$LOG_DIR/bridge.stdout.log"
  STDERR_LOG="$LOG_DIR/bridge.stderr.log"
  COMMAND_ARGS="    <string>hub</string>
    <string>run</string>
    <string>--log-dir</string><string>$LOG_DIR</string>"
fi
DOMAIN="gui/$(id -u)"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

render_plist() {
  cat <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$BINARY</string>
    <string>--profile</string><string>$PROFILE</string>
$COMMAND_ARGS
  </array>
  <key>EnvironmentVariables</key>
  <dict><key>PATH</key><string>$AGENT_PATH</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>$THROTTLE_SECS</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>$STDOUT_LOG</string>
  <key>StandardErrorPath</key><string>$STDERR_LOG</string>
</dict>
</plist>
PLIST
}

if [ "$DRY" -eq 1 ]; then
  render_plist
  echo "launchctl bootstrap $DOMAIN $PLIST"
  echo "launchctl bootout $DOMAIN/$LABEL"
  tmp="$(mktemp)"
  render_plist > "$tmp"
  plutil -lint "$tmp"
  rm -f "$tmp"
  exit 0
fi

if [ ! -x "$BINARY" ]; then
  echo "no captain binary at $BINARY; run 'make build' or pass --binary" >&2
  exit 1
fi

mkdir -p "$HOME/Library/LaunchAgents" "$LOG_DIR"
chmod 700 "$LOG_DIR"
render_plist > "$PLIST"
plutil -lint "$PLIST" >/dev/null
echo "wrote $PLIST"
echo "not started. bootstrap it with: launchctl bootstrap $DOMAIN $PLIST"
echo "stop it with: launchctl bootout $DOMAIN/$LABEL"
