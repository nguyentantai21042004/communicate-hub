#!/usr/bin/env bash
# Keeps target/ under a size cap. Called after make targets and by the Claude Code Stop hook.
#
#   1. Under the cap: do nothing. This is the common case and costs one `du`.
#   2. Over the cap: delete incremental caches. Safe; the next build of workspace crates is
#      slower once.
#   3. Still over: `cargo clean`. What is left is stale dependency builds that cargo never
#      deletes, and the next full build takes about a minute.
#
# Skips everything while cargo or rustc is writing to this target dir, so it cannot break a
# running build. Set CAPTAIN_TARGET_CAP_MB to change the cap.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
DEFAULT_CAP_MB=2048 # one clean `make check` is about 1 GB; leaves room for a release build
CAP_MB="${CAPTAIN_TARGET_CAP_MB:-$DEFAULT_CAP_MB}"

[ -d "$TARGET" ] || exit 0

size_mb() { du -sm "$TARGET" | cut -f1; }

if pgrep -f "$TARGET/" >/dev/null 2>&1; then
  exit 0 # a build is running; try again next time
fi

before=$(size_mb)
[ "$before" -le "$CAP_MB" ] && exit 0

find "$TARGET" -mindepth 2 -maxdepth 2 -type d -name incremental -exec rm -rf {} +
after=$(size_mb)
step="removed incremental caches"

if [ "$after" -gt "$CAP_MB" ]; then
  (cd "$ROOT" && cargo clean -q)
  after=$(size_mb 2>/dev/null || echo 0)
  step="ran cargo clean"
fi

echo "target-gc: ${before} MB > ${CAP_MB} MB cap, ${step}, now ${after} MB" >&2
