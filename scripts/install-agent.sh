#!/usr/bin/env bash
# Installs `captain daemon` as a launchd LaunchAgent for the current macOS user, so it starts at
# every login and restarts after a crash.
#
#   scripts/install-agent.sh [--profile NAME] [--binary PATH] [--no-load]
#
# 1. Stops the agent if it runs, and copies the binary to ~/.local/bin/captain. launchd runs
#    that copy, so `make build` never swaps the binary under a running daemon.
# 2. Runs `captain auth status` in the foreground. A new binary makes macOS ask once for
#    Keychain access; answer Always Allow here, while you are at the machine, so the daemon
#    never waits on a hidden prompt. It also checks that the profile is logged in.
# 3. Writes ~/Library/LaunchAgents/<label>.plist and starts it, unless --no-load.
#
# Run it again after every rebuild you want the daemon to use. It is safe to repeat.
# Uninstall: launchctl bootout gui/$(id -u)/<label>, then delete the plist.
#
# Stop any other process using the session first (`captain listen`, the Python demo).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="default"
BINARY="$ROOT/target/release/captain"
LOAD=1
LABEL_PREFIX="dev.tantai.telegram-captain"
INSTALL_DIR="$HOME/.local/bin"
LOG_DIR="$HOME/Library/Logs/telegram-captain"
THROTTLE_SECS=30 # launchd waits this long between restarts
LOGGED_IN='"logged_in":true'

while [ $# -gt 0 ]; do
  case "$1" in
    --profile) PROFILE="$2"; shift 2 ;;
    --binary) BINARY="$2"; shift 2 ;;
    --no-load) LOAD=0; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ ! -x "$BINARY" ]; then
  echo "no captain binary at $BINARY; run 'make build' or pass --binary" >&2
  exit 1
fi

LABEL="$LABEL_PREFIX.$PROFILE"
DOMAIN="gui/$(id -u)"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
INSTALLED="$INSTALL_DIR/captain"
mkdir -p "$HOME/Library/LaunchAgents" "$INSTALL_DIR" "$LOG_DIR"
chmod 700 "$LOG_DIR"

if launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1; then
  launchctl bootout "$DOMAIN/$LABEL"
  echo "stopped $LABEL"
fi

# Copy then rename, so a half-written file is never at the installed path.
if ! cmp -s "$BINARY" "$INSTALLED"; then
  cp "$BINARY" "$INSTALLED.new"
  chmod 755 "$INSTALLED.new"
  mv -f "$INSTALLED.new" "$INSTALLED"
  echo "installed $INSTALLED"
fi

echo "checking the login; allow Keychain access if macOS asks"
STATUS="$("$INSTALLED" --profile "$PROFILE" auth status)"
echo "$STATUS"
if ! printf '%s' "$STATUS" | tr -d ' ' | grep -q "$LOGGED_IN"; then
  cat >&2 <<HELP
profile $PROFILE is not logged in, so the agent was not started. Log in with the installed
binary, so Keychain trusts the same program the daemon runs, then run this script again:
  $INSTALLED --profile $PROFILE auth init
  $INSTALLED --profile $PROFILE auth login
HELP
  exit 1
fi

cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$INSTALLED</string>
    <string>--profile</string><string>$PROFILE</string>
    <string>daemon</string>
    <string>--log-dir</string><string>$LOG_DIR</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key>
  <dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>$THROTTLE_SECS</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardErrorPath</key><string>$LOG_DIR/$PROFILE.stderr.log</string>
</dict>
</plist>
PLIST
plutil -lint "$PLIST" >/dev/null
echo "wrote $PLIST"

if [ "$LOAD" -eq 1 ]; then
  launchctl bootstrap "$DOMAIN" "$PLIST"
  echo "started $LABEL; logs in $LOG_DIR"
else
  echo "start it with: launchctl bootstrap $DOMAIN $PLIST"
fi
