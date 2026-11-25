# shellcheck shell=bash
# Helpers shared by setup.sh and seed.sh (sourced, not executed).
# Secrets never go on a command line: request bodies and auth headers reach
# curl through stdin or process substitution, so they do not show up in `ps`.

KIT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CREDENTIALS_FILE=${CREDENTIALS_FILE:-$KIT_DIR/credentials.env}

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
die() {
  log "error: $*"
  exit 1
}

# Loads KEY=value lines without shell evaluation; variables already set in the
# environment win.
load_env_file() {
  local file=$1 line key value
  [[ -f "$file" ]] || return 0
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" =~ ^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]] || continue
    key=${BASH_REMATCH[1]} value=${BASH_REMATCH[2]}
    if [[ "$value" =~ ^\"(.*)\"$ || "$value" =~ ^\'(.*)\'$ ]]; then
      value=${BASH_REMATCH[1]}
    fi
    [[ -n "${!key+x}" ]] || export "$key=$value"
  done <"$file"
  return 0
}

# Loads .env and works out the server URL (REVIEW_SERVER overrides it).
load_config() {
  load_env_file "$KIT_DIR/.env"
  if [[ -z "${REVIEW_SERVER:-}" ]]; then
    [[ -n "${REVIEW_HOST:-}" ]] || die "REVIEW_HOST is not set (copy .env.example to .env)"
    REVIEW_SERVER="https://$REVIEW_HOST"
  fi
  SERVER=${REVIEW_SERVER%/}
}

# A password a reviewer can type: GROUPS (default 4) groups of five characters
# without look-alikes, at least one lower, upper and digit, no character three
# times in a row (Rocket.Chat's default password policy; "-" is the special one).
gen_password() {
  python3 -I -c '
import secrets, sys
alphabet = "abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789"
while True:
    groups = ["".join(secrets.choice(alphabet) for _ in range(5)) for _ in range(int(sys.argv[1]))]
    pw = "-".join(groups)
    if (any(c.islower() for c in pw) and any(c.isupper() for c in pw)
            and any(c.isdigit() for c in pw)
            and not any(pw[i] == pw[i + 1] == pw[i + 2] for i in range(len(pw) - 2))):
        print(pw)
        break
' "${1:-4}"
}

# First two and last character only, for logs.
mask() { printf '%s…%s (%d chars)' "${1:0:2}" "${1: -1}" "${#1}"; }

sha256_hex() { printf '%s' "$1" | python3 -I -c 'import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())'; }

# jb key=string key:=json ... -> JSON object on stdout. The arguments (often
# passwords) reach python3 NUL-separated on stdin, not on its command line.
jb() {
  printf '%s\0' "$@" | python3 -I -c '
import json, sys
out = {}
for arg in sys.stdin.buffer.read().decode("utf-8").split("\0"):
    if not arg:
        continue
    i = arg.index("=")
    if i > 0 and arg[i - 1] == ":":
        out[arg[:i - 1]] = json.loads(arg[i + 1:])
    else:
        out[arg[:i]] = arg[i + 1:]
print(json.dumps(out, ensure_ascii=False))
'
}

# jget <python expression over d> : reads JSON on stdin; prints "" for None
# and on any error (missing key, not JSON), so callers test for emptiness.
# The expression is always a literal in these scripts, never server data.
jget() {
  python3 -I -c '
import json, sys
try:
    d = json.load(sys.stdin)
    v = eval(sys.argv[1], {"d": d})
except Exception:
    v = None
if v is None:
    print("")
elif isinstance(v, bool):
    print("true" if v else "false")
elif isinstance(v, (dict, list)):
    print(json.dumps(v, ensure_ascii=False))
else:
    print(v)
' "$1"
}

ok() { [[ "$(jget 'd.get("success")' <<<"$1")" == true ]]; }

# Identities: login tokens per key, plus the 2FA code for the operator.
declare -A AUTH_UID=() AUTH_TOKEN=() AUTH_2FA=()

# http METHOD PATH [BODY] [IDENTITY] -> response body. Retries on 429 and on
# connection errors; never fails the script by itself.
http() {
  local method=$1 path=$2 body=${3:-} who=${4:-} attempt out code
  local headers="content-type: application/json"$'\n'
  if [[ -n "$who" ]]; then
    headers+="X-User-Id: ${AUTH_UID[$who]}"$'\n'"X-Auth-Token: ${AUTH_TOKEN[$who]}"$'\n'
    if [[ -n "${AUTH_2FA[$who]:-}" ]]; then
      headers+="x-2fa-method: password"$'\n'"x-2fa-code: ${AUTH_2FA[$who]}"$'\n'
    fi
  fi
  for attempt in 1 2 3 4 5 6 7 8; do
    if [[ -n "$body" ]]; then
      out=$(curl -sS --max-time 60 -X "$method" "$SERVER/api/v1/$path" -H @<(printf '%s' "$headers") \
        --data-binary @- -w $'\n%{http_code}' <<<"$body") || out=$'\n000'
    else
      out=$(curl -sS --max-time 60 -X "$method" "$SERVER/api/v1/$path" -H @<(printf '%s' "$headers") \
        -w $'\n%{http_code}') || out=$'\n000'
    fi
    code=${out##*$'\n'}
    out=${out%$'\n'*}
    if [[ "$code" == 429 || "$code" == 000 || "$code" == 502 || "$code" == 503 ]]; then
      log "$method $path: HTTP $code, retrying ($attempt/8)"
      sleep $((attempt * 5))
      continue
    fi
    printf '%s' "$out"
    return 0
  done
  printf '{"success":false,"error":"HTTP %s after retries"}' "$code"
}

# as IDENTITY METHOD PATH [BODY]
as() { http "$2" "$3" "${4:-}" "$1"; }

wait_for_server() {
  local i rc
  log "waiting for $SERVER (HTTPS must validate) ..."
  for i in $(seq 1 120); do
    rc=0
    curl -sf --max-time 10 "$SERVER/api/info" >/dev/null 2>&1 || rc=$?
    if ((rc == 0)); then
      log "server is up"
      return 0
    fi
    # 60: the certificate does not validate.
    if ((rc == 60)) && [[ "${ACME_CA:-}" == *staging* ]]; then
      die "the certificate comes from the Let's Encrypt staging CA (ACME_CA in .env), which is not trusted: remove ACME_CA, run 'docker compose up -d caddy', then rerun this script"
    fi
    ((i % 6 == 0)) && log "still waiting ($((i * 5)) s, curl exit $rc)"
    sleep 5
  done
  die "$SERVER did not answer within 10 minutes (DNS, certificate or container problem; see README.md)"
}

# login IDENTITY USER PASSWORD [operator] -> 0 on success.
login() {
  local key=$1 user=$2 pass=$3 r
  r=$(http POST login "$(jb user="$user" password="$pass")")
  ok_status "$r" || return 1
  AUTH_UID[$key]=$(jget 'd["data"]["userId"]' <<<"$r")
  AUTH_TOKEN[$key]=$(jget 'd["data"]["authToken"]' <<<"$r")
  [[ "${4:-}" == operator ]] && AUTH_2FA[$key]=$(sha256_hex "$pass")
  return 0
}

ok_status() { [[ "$(jget 'd.get("status")' <<<"$1")" == success ]]; }

logout() {
  [[ -n "${AUTH_TOKEN[$1]:-}" ]] || return 0
  as "$1" POST logout >/dev/null
  unset 'AUTH_UID[$1]' 'AUTH_TOKEN[$1]' 'AUTH_2FA[$1]'
}

# Credentials file: KEY=value, chmod 600, git-ignored.
declare -A CRED=()
load_credentials() {
  local line
  [[ -f "$CREDENTIALS_FILE" ]] || return 0
  while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" =~ ^([A-Z_][A-Z0-9_]*)=(.*)$ ]]; then
      CRED[${BASH_REMATCH[1]}]=${BASH_REMATCH[2]}
    fi
  done <"$CREDENTIALS_FILE"
}

require_credentials() {
  [[ -f "$CREDENTIALS_FILE" ]] || die "$CREDENTIALS_FILE is missing: run setup.sh first"
  load_credentials
  local k
  for k in "$@"; do
    [[ -n "${CRED[$k]:-}" ]] || die "$k is missing in $CREDENTIALS_FILE: run setup.sh"
  done
}

# Operator login with the 2FA code for settings, permissions and user writes.
login_operator() {
  require_credentials OPS_ADMIN_USER OPS_ADMIN_PASSWORD
  login ops "${CRED[OPS_ADMIN_USER]}" "${CRED[OPS_ADMIN_PASSWORD]}" operator ||
    die "cannot log in as ${CRED[OPS_ADMIN_USER]} (was the database created with another admin? see README.md)"
}

user_id() { as ops GET "users.info?username=$1" | jget 'd["user"]["_id"]'; }

# Names shared by setup.sh (review notes) and seed.sh. SEED_SUFFIX exists only
# to try the scripts on a shared test server without name clashes.
SFX=${SEED_SUFFIX:-}
TEAM_NAME="product-team$SFX"
E2E_ROOM="secure-notes$SFX"
