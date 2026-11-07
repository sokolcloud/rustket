#!/usr/bin/env bash
# Idempotently seeds the local dev Rocket.Chat with test users and rooms.
set -euo pipefail

SERVER="${RUSTKET_TEST_SERVER:-http://localhost:3000}"

for _ in $(seq 1 60); do
  curl -sf "$SERVER/api/info" >/dev/null && break
  sleep 2
done

login=$(curl -sf -X POST "$SERVER/api/v1/login" -H 'content-type: application/json' \
  -d '{"user":"admin","password":"adminpass"}')
UID_=$(printf '%s' "$login" | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["userId"])')
TOKEN=$(printf '%s' "$login" | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["authToken"])')

api() {
  local method=$1 path=$2 body=${3:-}
  if [[ -n "$body" ]]; then
    curl -s -X "$method" "$SERVER/api/v1/$path" -H "X-User-Id: $UID_" -H "X-Auth-Token: $TOKEN" \
      -H 'content-type: application/json' -d "$body"
  else
    curl -s -X "$method" "$SERVER/api/v1/$path" -H "X-User-Id: $UID_" -H "X-Auth-Token: $TOKEN"
  fi
}

# Email 2FA is auto-enabled for verified users by default; only carol keeps it
# (to exercise the 2FA flow), alice and bob log in with a password only.
api POST settings/Accounts_TwoFactorAuthentication_By_Email_Auto_Opt_In '{"value":false}' >/dev/null

user_login() {
  curl -s -X POST "$SERVER/api/v1/login" -H 'content-type: application/json' \
    -d "{\"user\":\"$1\",\"password\":\"${1}pass\"}"
}

for u in alice bob carol; do
  name="$(tr '[:lower:]' '[:upper:]' <<<"${u:0:1}")${u:1}"
  if user_login "$u" | grep -q totp-required && [[ "$u" != carol ]]; then
    api POST users.delete "{\"username\":\"$u\",\"confirmRelinquish\":true}" >/dev/null
  fi
  api POST users.create "{\"username\":\"$u\",\"email\":\"$u@rustket.local\",\"password\":\"${u}pass\",\"name\":\"$name\",\"verified\":true,\"requirePasswordChange\":false}" >/dev/null || true
done

carol=$(user_login carol)
if printf '%s' "$carol" | grep -q '"status":"success"'; then
  cuid=$(printf '%s' "$carol" | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["userId"])')
  ctok=$(printf '%s' "$carol" | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["authToken"])')
  curl -s -X POST "$SERVER/api/v1/users.2fa.enableEmail" -H "X-User-Id: $cuid" -H "X-Auth-Token: $ctok" >/dev/null
fi

api POST channels.create '{"name":"dev-public","members":["alice","bob","carol"]}' >/dev/null || true
api POST groups.create '{"name":"dev-private","members":["alice","bob"]}' >/dev/null || true
api POST im.create '{"username":"alice"}' >/dev/null || true

# Ensure memberships even when the rooms already existed (users may have been recreated).
uid_of() { api GET "users.info?username=$1" | python3 -c 'import sys,json; print(json.load(sys.stdin)["user"]["_id"])'; }
for u in alice bob carol; do
  id=$(uid_of "$u")
  api POST channels.invite "{\"roomName\":\"dev-public\",\"userId\":\"$id\"}" >/dev/null || true
  api POST channels.invite "{\"roomName\":\"general\",\"userId\":\"$id\"}" >/dev/null || true
  [[ "$u" != carol ]] && api POST groups.invite "{\"roomName\":\"dev-private\",\"userId\":\"$id\"}" >/dev/null || true
done

echo "seeded $SERVER: users admin/alice/bob (password only), carol (email 2FA via Mailpit); rooms #general #dev-public dev-private(private)"
