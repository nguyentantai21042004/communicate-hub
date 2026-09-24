#!/usr/bin/env bash
# Enforces the comment-tag rules in docs/CONVENTIONS.md:
#   1. Only these tags may open a comment: TODO(owner): FIXME(owner): INVARIANT: TELEGRAM:
#      SECURITY: PERF:
#   2. TODO and FIXME always name an owner.
#   3. Outside tests, every .expect( has an INVARIANT: comment within the lines just above it.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ALLOWED='^(TODO\([a-z0-9_-]+\)|FIXME\([a-z0-9_-]+\)|INVARIANT|TELEGRAM|SECURITY|PERF)$'
INVARIANT_WINDOW=3 # lines above an .expect( searched for INVARIANT:

cd "$ROOT"
files=$(find crates apps -name '*.rs' -not -path '*/target/*')
status=0

# Rules 1 and 2: an all-caps word, optionally with (owner), then a colon, at the start of a
# comment's text.
while IFS= read -r hit; do
  [ -z "$hit" ] && continue
  tag=$(printf '%s' "$hit" | sed -E 's#^[^:]+:[0-9]+:.*//[/!]?[[:space:]]*([A-Z]{3,}(\([^)]*\))?):.*#\1#')
  if ! printf '%s' "$tag" | grep -Eq "$ALLOWED"; then
    echo "comment tag not allowed ($tag): $hit" >&2
    status=1
  fi
done < <(grep -nE '//[/!]?[[:space:]]*[A-Z]{3,}(\([^)]*\))?:' $files || true)

while IFS= read -r hit; do
  [ -z "$hit" ] && continue
  echo "TODO/FIXME needs an owner, e.g. TODO(tantai): $hit" >&2
  status=1
done < <(grep -nE '//.*\b(TODO|FIXME)\b' $files | grep -vE '(TODO|FIXME)\([a-z0-9_-]+\):' || true)

# Rule 3. Test modules sit at the end of each file, so checking stops at #[cfg(test)].
for f in $files; do
  awk -v file="$f" -v window="$INVARIANT_WINDOW" '
    /#\[cfg\(test\)\]/ { exit }
    { lines[NR] = $0 }
    /\.expect\(/ && $0 !~ /^[[:space:]]*\/\// {
      found = 0
      for (i = NR - 1; i >= NR - window && i > 0; i--) if (lines[i] ~ /INVARIANT:/) found = 1
      if (!found) { printf "%s:%d: .expect( without an INVARIANT: comment above it\n", file, NR; bad = 1 }
    }
    END { exit bad }
  ' "$f" >&2 || status=1
done

if [ "$status" -eq 0 ]; then echo "comment lint: ok"; fi
exit "$status"
