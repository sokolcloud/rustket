#!/bin/sh
# Inventory and compliance check for Rustket on macOS and Linux: finds the
# installed copy, reports its version and, when a policy key is given,
# whether that key is applied (Rustket's own `--dump-policy` report).
#
#   rustket-policy-check.sh [--jamf] [KEY]
#
# Tools that pass no arguments (a Jamf Pro extension attribute, most
# compliance agents): set DEFAULT_KEY and JAMF below in the copy you upload.
#
# Exit status: 0 when Rustket 0.4.0 or later is installed and KEY (if given)
# is applied; 1 otherwise (not installed, too old, key not applied, report
# unreadable). With --jamf the summary is printed as <result>...</result>
# for a Jamf Pro extension attribute and the exit status is always 0.
# RUSTKET_BIN overrides the executable (a copy installed by root elsewhere,
# e.g. /opt; run as root, only a file that root alone can change, the file
# and every directory above it).
#
# Run as root (Jamf, a management agent) it reports the computer's policy;
# on macOS it may not see a profile installed for a user, not the device.
# Reads the JSON report with osascript (JavaScript) on macOS, which every
# Mac has, and with python3 on Linux. Writes only a temporary file.
set -u

# Used when the script gets no arguments: a policy key such as RequireHttps,
# and 1 for the <result> output of a Jamf Pro extension attribute.
DEFAULT_KEY=""
JAMF=0

jamf="$JAMF"
key="$DEFAULT_KEY"
if [ "$#" -gt 0 ]; then
    jamf=0
    if [ "$1" = "--jamf" ]; then
        jamf=1
        shift
    fi
    key="${1:-}"
fi

finish() {
    # $1: exit status, $2: summary line (printf: no escapes are expanded).
    if [ "$jamf" = 1 ]; then
        printf '<result>%s</result>\n' "$2"
        exit 0
    fi
    printf '%s\n' "$2"
    exit "$1"
}

# The installed copy. On macOS only the executable inside the app bundle
# sees the managed preferences.
os="$(uname -s)"
bin="${RUSTKET_BIN:-}"
if [ -z "$bin" ]; then
    if [ "$os" = Darwin ]; then
        bin=/Applications/Rustket.app/Contents/MacOS/rustket
    else
        bin="$(command -v rustket 2>/dev/null || echo /usr/bin/rustket)"
    fi
fi
[ -x "$bin" ] || finish 1 "Rustket not installed ($bin not found)"

# Run as root, the script executes $bin: an override must be a file that only
# root can change, and so must every directory above it.
root_only() {
    p="$1"
    while :; do
        [ -n "$(find "$p" -prune -user 0 ! -perm -020 ! -perm -002 2>/dev/null)" ] || return 1
        [ "$p" = / ] && return 0
        p="$(dirname "$p")"
    done
}
if [ -n "${RUSTKET_BIN:-}" ] && [ "$(id -u)" -eq 0 ]; then
    case "$bin" in
        /*) root_only "$bin" || finish 1 "RUSTKET_BIN $bin: not owned by root or writable by others; run the check as the user who owns it" ;;
        *) finish 1 "RUSTKET_BIN must be an absolute path" ;;
    esac
fi

# `rustket --version` prints "rustket X.Y.Z".
version="$("$bin" --version 2>/dev/null | awk 'NR == 1 { print $2 }')"
case "$version" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) finish 1 "Rustket at $bin: cannot read the version" ;;
esac
major="${version%%.*}"
minor="${version#*.}"
minor="${minor%%.*}"
case "$major$minor" in
    *[!0-9]*) finish 1 "Rustket at $bin: cannot read the version" ;;
esac
# Policy exists from 0.4.0. Older copies ignore --dump-policy and open their
# window, so they are never asked for the report.
if [ "$major" -eq 0 ] && [ "$minor" -lt 4 ]; then
    finish 1 "Rustket $version at $bin: older than 0.4.0, reads no policy"
fi
[ -n "$key" ] || finish 0 "Rustket $version at $bin"

# macOS never runs python3: without the developer tools /usr/bin/python3 is
# a stub that asks the signed-in user to install them.
if [ "$os" != Darwin ] && ! command -v python3 >/dev/null 2>&1; then
    finish 1 "Rustket $version at $bin: python3 is needed to read the policy report"
fi
report="$(mktemp)" || finish 1 "Rustket $version: cannot create a temporary file"
trap 'rm -f "$report"' EXIT
# Standard output, redirected by the shell: Rustket writes no path itself.
if ! "$bin" --dump-policy >"$report" 2>/dev/null; then
    finish 1 "Rustket $version at $bin: --dump-policy failed"
fi

# Both readers print "<status>|<summary>"; status 0 when a row of KEY is
# applied.
if [ "$os" = Darwin ]; then
    result="$(osascript -l JavaScript -e '
function run(argv) {
    ObjC.import("Foundation");
    var path = argv[0], key = argv[1], dump;
    var text = ObjC.unwrap($.NSString.stringWithContentsOfFileEncodingError(
        path, $.NSUTF8StringEncoding, null));
    if (typeof text !== "string") return "1|cannot read the policy report";
    try { dump = JSON.parse(text); } catch (e) {
        return "1|cannot read the policy report: " + e;
    }
    var rows = (dump.rows || []).filter(function (r) { return r.key === key; });
    var applied = rows.filter(function (r) { return r.status === "applied"; });
    var blocked = dump.blocked ? " (blocked)" : "";
    if (applied.length) {
        var a = applied[0];
        return "0|" + key + " applied: " + a.value + " from " + a.source + blocked;
    }
    if (rows.length) {
        var r = rows[0];
        return "1|" + key + " " + r.status + ": " + r.value + " from " + r.source + blocked;
    }
    return "1|" + key + " not set" + blocked;
}' "$report" "$key" 2>/dev/null)"
else
    result="$(python3 -I - "$report" "$key" <<'EOF'
import json, sys

path, key = sys.argv[1], sys.argv[2]
try:
    with open(path, encoding="utf-8") as f:
        dump = json.load(f)
except (OSError, ValueError) as e:
    print("1|cannot read the policy report: %s" % e)
    sys.exit()
rows = [r for r in dump.get("rows", []) if r.get("key") == key]
applied = [r for r in rows if r.get("status") == "applied"]
blocked = " (blocked)" if dump.get("blocked") else ""
if applied:
    r = applied[0]
    print("0|%s applied: %s from %s%s" % (key, r.get("value"), r.get("source"), blocked))
elif rows:
    r = rows[0]
    print("1|%s %s: %s from %s%s" % (key, r.get("status"), r.get("value"), r.get("source"), blocked))
else:
    print("1|%s not set%s" % (key, blocked))
EOF
)"
fi
case "$result" in
    0\|*) finish 0 "Rustket $version: ${result#0|}" ;;
    1\|*) finish 1 "Rustket $version: ${result#1|}" ;;
    *) finish 1 "Rustket $version: cannot read the policy report" ;;
esac
