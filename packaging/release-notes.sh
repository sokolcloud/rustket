#!/usr/bin/env bash
# Prints the CHANGELOG.md section of <version> without its heading; fails when
# it is missing or empty. Used by .github/workflows/release.yml for the GitHub
# release notes.
#
#   packaging/release-notes.sh 0.2.2
#   packaging/release-notes.sh 0.2.2 path/to/CHANGELOG.md
set -euo pipefail
version="${1:?usage: release-notes.sh <version> [CHANGELOG.md]}"
file="${2:-$(dirname "${BASH_SOURCE[0]}")/../CHANGELOG.md}"
[ -f "$file" ] || { echo "error: $file not found" >&2; exit 1; }
notes="$(awk -v want="## [$version]" '
    index($0, want) == 1 { found = 1; next }
    found && (/^## \[/ || /^\[[^]]+\]: /) { exit }
    found { print }
' "$file" | sed -e '/./,$!d' | awk '
    # GitHub renders single newlines in release notes as line breaks, so
    # wrapped continuation lines are joined to the line they continue.
    NR > 1 && prev != "" && prev !~ /^#/ && $0 != "" && $0 !~ /^[[:space:]]*([-*]|[0-9]+\.|#)[[:space:]]/ {
        sub(/^[[:space:]]+/, ""); prev = prev " " $0; next
    }
    NR > 1 { print prev }
    { prev = $0 }
    END { if (NR) print prev }
')"
if [ -z "${notes//[[:space:]]/}" ]; then
    echo "error: $file has no non-empty '## [$version]' section" >&2
    exit 1
fi
printf '%s\n' "$notes"
