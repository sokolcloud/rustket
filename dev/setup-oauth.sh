#!/usr/bin/env bash
# Idempotently configures a Custom OAuth service "keycloak" (OpenID Connect) on the
# local dev Rocket.Chat, pointing at the Keycloak container from docker-compose.yml.
set -euo pipefail

SERVER="${RUSTKET_TEST_SERVER:-http://localhost:3000}"
KC="http://localhost:8081/realms/rustket/protocol/openid-connect"

login=$(curl -sf -X POST "$SERVER/api/v1/login" -H 'content-type: application/json' \
  -d '{"user":"admin","password":"adminpass"}')
UID_=$(printf '%s' "$login" | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["userId"])')
TOKEN=$(printf '%s' "$login" | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["authToken"])')
H=(-H "X-User-Id: $UID_" -H "X-Auth-Token: $TOKEN" -H 'content-type: application/json')

if ! curl -s "$SERVER/api/v1/settings/Accounts_OAuth_Custom-Keycloak" "${H[@]}" | grep -q '"success":true'; then
  curl -s -X POST "$SERVER/api/v1/method.call/addOAuthService" "${H[@]}" \
    -d '{"message":"{\"msg\":\"method\",\"id\":\"1\",\"method\":\"addOAuthService\",\"params\":[\"keycloak\"]}"}' >/dev/null
fi

set_setting() {
  curl -s -X POST "$SERVER/api/v1/settings/$1" "${H[@]}" -d "{\"value\":$2}" | grep -q '"success":true' \
    || echo "failed to set $1" >&2
}

P=Accounts_OAuth_Custom-Keycloak
set_setting "$P-url" "\"$KC\""
set_setting "$P-token_path" '"/token"'
set_setting "$P-identity_path" '"/userinfo"'
set_setting "$P-authorize_path" '"/auth"'
set_setting "$P-scope" '"openid email profile"'
set_setting "$P-id" '"rocketchat"'
set_setting "$P-secret" '"rocketchat-secret"'
set_setting "$P-token_sent_via" '"header"'
set_setting "$P-identity_token_sent_via" '"header"'
set_setting "$P-login_style" '"redirect"'
set_setting "$P-button_label_text" '"Keycloak"'
set_setting "$P-username_field" '"preferred_username"'
set_setting "$P-email_field" '"email"'
set_setting "$P-name_field" '"name"'
set_setting "$P-merge_users" 'true'
set_setting "$P-show_button" 'true'
set_setting "$P" 'true'

echo "Custom OAuth 'keycloak' configured (kcuser / kcpass)"
curl -s "$SERVER/api/v1/settings.oauth" | python3 -c 'import sys,json; print([s.get("service") for s in json.load(sys.stdin)["services"]])'
