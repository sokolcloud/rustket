#!/usr/bin/env bash
# Publishes the review workspace through a named Cloudflare Tunnel
# (REVIEW_PROXY=tunnel, README.md "Tunnel mode"). Idempotent.
#
#   ./tunnel.sh                        create or reuse the tunnel, route REVIEW_HOST to
#                                      it, write its config, install and start the unit
#   ./tunnel.sh --overwrite-dns        same, replacing a DNS record that points elsewhere
#   ./tunnel.sh status                 unit, tunnel connections, local and public checks
#   ./tunnel.sh down                   stop and remove the unit and the config
#   ./tunnel.sh down --delete-tunnel   also delete the tunnel, its credentials and,
#                                      with CLOUDFLARE_API_TOKEN set, its DNS record
#
# Needs `cloudflared tunnel login` once (writes ~/.cloudflared/cert.pem).
# Never prints the tunnel credentials.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

ACTION=up OVERWRITE_DNS=0 DELETE_TUNNEL=0
for arg in "$@"; do
  case $arg in
    up | status | down) ACTION=$arg ;;
    --overwrite-dns) OVERWRITE_DNS=1 ;;
    --delete-tunnel) DELETE_TUNNEL=1 ;;
    -h | --help)
      sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) die "unknown argument: $arg" ;;
  esac
done
((OVERWRITE_DNS == 0)) || [[ "$ACTION" == up ]] || die "--overwrite-dns goes with up only"
((DELETE_TUNNEL == 0)) || [[ "$ACTION" == down ]] || die "--delete-tunnel goes with down only"

command -v curl >/dev/null || die "curl is required"
command -v python3 >/dev/null || die "python3 is required"
load_config
[[ "$REVIEW_PROXY" == tunnel || "$ACTION" == down ]] ||
  die "REVIEW_PROXY is $REVIEW_PROXY in .env: set REVIEW_PROXY=tunnel and COMPOSE_FILE (see .env.example)"
[[ -n "${REVIEW_HOST:-}" ]] || die "REVIEW_HOST is not set in .env"
[[ "$ACTION" == down ]] || check_compose_file

NAME=$REVIEW_TUNNEL_NAME
CF_DIR=$HOME/.cloudflared
ORIGIN_CERT=${TUNNEL_ORIGIN_CERT:-$CF_DIR/cert.pem}
CONFIG=$CF_DIR/$NAME.yml
UNIT=$NAME.service
UNIT_DIR=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
UNIT_FILE=$UNIT_DIR/$UNIT
ORIGIN=http://127.0.0.1:$REVIEW_TUNNEL_PORT
# What the Mac app sends (crates/rocketchat/src/lib.rs USER_AGENT), so
# Cloudflare rules are checked against the real client.
RUSTKET_VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$KIT_DIR/../../Cargo.toml" 2>/dev/null | head -n 1) || true
RUSTKET_UA="Rustket/${RUSTKET_VERSION:-0.2.11} (Macintosh; Mac OS X)"

ERR=$(mktemp)
trap 'rm -f "$ERR"' EXIT

require_cloudflared() {
  CLOUDFLARED=$(command -v cloudflared) ||
    die "cloudflared is not installed (https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/)"
}

require_login() {
  [[ -f "$ORIGIN_CERT" ]] ||
    die "$ORIGIN_CERT is missing: run 'cloudflared tunnel login', open the printed URL and pick the zone that holds $REVIEW_HOST"
}

# cloudflared tunnel management with the origin certificate.
cf() { "$CLOUDFLARED" tunnel --origincert "$ORIGIN_CERT" "$@"; }

# Prints the id of the live tunnel named $NAME, or nothing.
tunnel_id() {
  local out
  out=$(cf list --name "$NAME" --output json 2>"$ERR") || {
    cat "$ERR" >&2
    die "cloudflared tunnel list failed"
  }
  python3 -I -c '
import json, sys
try:
    tunnels = json.loads(sys.stdin.read() or "null") or []
except ValueError:
    sys.exit("cloudflared tunnel list did not print JSON")
ids = [t["id"] for t in tunnels
       if t.get("name") == sys.argv[1]
       and (not t.get("deleted_at") or str(t["deleted_at"]).startswith("0001-"))]
print(ids[0] if ids else "")
' "$NAME" <<<"$out"
}

# write_file PATH CONTENT: mode 600, atomic; sets CHANGED=1 when the content differs.
CHANGED=0
write_file() {
  local path=$1 content=$2 tmp
  if [[ -f "$path" ]] && [[ "$(cat "$path")" == "$content" ]]; then
    chmod 600 "$path"
    return 0
  fi
  mkdir -p "$(dirname "$path")"
  tmp=$(umask 077 && mktemp "$path.XXXXXX")
  printf '%s\n' "$content" >"$tmp"
  chmod 600 "$tmp"
  mv -f "$tmp" "$path"
  CHANGED=1
  log "wrote $path"
}

# Makes sure the tunnel's credentials file is on this host. For a tunnel
# created elsewhere, `cloudflared tunnel token --cred-file` writes it from the
# account (stdout dropped: without the flag it would print the token).
ensure_credentials_file() {
  local id=$1 cred=$CF_DIR/$1.json
  if [[ ! -f "$cred" ]]; then
    log "credentials for tunnel $NAME are not on this host, fetching them"
    (umask 077 && cf token --cred-file "$cred" "$id" >/dev/null 2>"$ERR") || true
    [[ -f "$cred" ]] || {
      cat "$ERR" >&2
      die "no credentials file for tunnel $NAME ($id): copy $id.json from the host that created it to $CF_DIR, or run './tunnel.sh down --delete-tunnel' and rerun"
    }
  fi
  chmod 600 "$cred"
  python3 -I -c '
import json, sys
d = json.load(open(sys.argv[1]))
sys.exit(0 if str(d.get("TunnelID", "")).lower() == sys.argv[2].lower() else 1)
' "$cred" "$id" 2>/dev/null || die "$cred does not hold the credentials of tunnel $id"
  CRED_FILE=$cred
}

route_dns() {
  local id=$1 out host="" flags=()
  ((OVERWRITE_DNS)) && flags+=(--overwrite-dns)
  if ! out=$(cf route dns "${flags[@]}" "$id" "$REVIEW_HOST" 2>&1); then
    if grep -qi 'already exists' <<<"$out"; then
      die "$REVIEW_HOST already has an A, AAAA or CNAME record that does not route to tunnel $NAME ($id). Look at it in the Cloudflare dashboard (DNS > Records); if it is a leftover, delete it or rerun with --overwrite-dns"
    fi
    printf '%s\n' "$out" >&2
    die "cloudflared tunnel route dns failed"
  fi
  if [[ "$out" =~ CNAME\ ([^ ]+)\ which\ will\ route ]] || [[ "$out" =~ ([^ ]+)\ is\ already\ configured\ to\ route ]]; then
    host=${BASH_REMATCH[1]}
  fi
  # A hostname outside the zone of cert.pem gets that zone appended.
  if [[ -n "$host" && "${host,,}" != "${REVIEW_HOST,,}" ]]; then
    die "cloudflared routed $host instead of $REVIEW_HOST: cert.pem belongs to another zone. Delete that record in the dashboard, rerun 'cloudflared tunnel login' for the right zone (move the old cert.pem away first)"
  fi
  log "DNS: ${out##*INF }"
}

check_linger() {
  local linger
  linger=$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null || true)
  [[ "$linger" == yes ]] ||
    log "warning: lingering is off for $(id -un): the tunnel stops at logout and does not start at boot ('sudo loginctl enable-linger $(id -un)')"
}

up() {
  require_cloudflared
  require_login
  command -v systemctl >/dev/null || die "systemctl is required (systemd user session)"
  [[ "$CONFIG$CLOUDFLARED$HOME" != *[[:space:]%\"\\]* ]] ||
    die "paths with spaces, quotes or '%' are not supported in the systemd unit"

  local id
  id=$(tunnel_id)
  if [[ -z "$id" ]]; then
    log "creating tunnel $NAME"
    # Output dropped: shown only on failure (it names the credentials file, not its content).
    cf create "$NAME" >"$ERR" 2>&1 || {
      cat "$ERR" >&2
      die "cloudflared tunnel create failed"
    }
    id=$(tunnel_id)
    [[ -n "$id" ]] || die "tunnel $NAME was created but is not listed"
  else
    log "reusing tunnel $NAME ($id)"
  fi
  ensure_credentials_file "$id"
  route_dns "$id"

  write_file "$CONFIG" "# Written by deploy/review-server/tunnel.sh; rerun it instead of editing.
tunnel: $id
credentials-file: $CRED_FILE
ingress:
  - hostname: $REVIEW_HOST
    service: $ORIGIN
  - service: http_status:404"

  # The run is identified by UUID, so cert.pem is not needed at runtime. The
  # user manager has no network-online.target of its own; Restart= covers a
  # start before the network is up.
  write_file "$UNIT_FILE" "[Unit]
Description=Cloudflare Tunnel $NAME for the Rustket review server ($REVIEW_HOST)
Wants=network-online.target
After=network-online.target

[Service]
ExecStart=\"$CLOUDFLARED\" tunnel --no-autoupdate --config \"$CONFIG\" run $id
Restart=always
RestartSec=5s

[Install]
WantedBy=default.target"

  systemctl --user daemon-reload
  systemctl --user enable --quiet "$UNIT"
  if systemctl --user is-active --quiet "$UNIT"; then
    if ((CHANGED)); then
      log "restarting $UNIT"
      systemctl --user restart "$UNIT"
    fi
  else
    log "starting $UNIT"
    systemctl --user start "$UNIT"
  fi
  check_linger
  sleep 5
  status
}

status() {
  local code id=""
  if command -v systemctl >/dev/null; then
    systemctl --user --no-pager --lines=5 status "$UNIT" || true
  fi
  if command -v cloudflared >/dev/null && [[ -f "$ORIGIN_CERT" ]]; then
    CLOUDFLARED=$(command -v cloudflared)
    id=$(tunnel_id) || true
    if [[ -n "$id" ]]; then
      cf info "$id" || true
    else
      log "no tunnel named $NAME"
    fi
  fi

  code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 10 "$ORIGIN/api/info" 2>/dev/null) || true
  log "origin $ORIGIN/api/info: HTTP ${code:-000}$([[ "$code" == 200 ]] || printf ' (stack not up? ./setup.sh)')"

  # -w rather than the first header line: behind an HTTPS proxy that would be
  # the CONNECT answer.
  code=$(curl -sS -o /dev/null -D "$ERR" -w '%{http_code}' -A "$RUSTKET_UA" --max-time 20 \
    "https://$REVIEW_HOST/api/info" 2>/dev/null) || true
  if grep -qi '^cf-mitigated:' "$ERR"; then
    log "public https://$REVIEW_HOST/api/info: HTTP $code, Cloudflare CHALLENGE: the app cannot pass it (README.md, \"Tunnel mode\")"
  else
    log "public https://$REVIEW_HOST/api/info: HTTP ${code:-000} (200 expected; 502/530: origin or tunnel down)"
  fi
  # A WebSocket upgrade for DDP: 101 expected. curl then waits for frames
  # until --max-time, so its exit status is ignored.
  code=$(curl --http1.1 -sS -o /dev/null -w '%{http_code}' --max-time 5 -A "$RUSTKET_UA" \
    -H 'Connection: Upgrade' -H 'Upgrade: websocket' -H 'Sec-WebSocket-Version: 13' \
    -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' "https://$REVIEW_HOST/websocket" 2>/dev/null) || true
  log "public wss://$REVIEW_HOST/websocket upgrade: HTTP ${code:-000} (101 expected)"
  # Rocket.Chat sends uploads with a year-long max-age and no "private", and
  # Cloudflare caches static extensions by default, ignoring the app's auth:
  # a cached private upload would be served to anyone. A .png path must not
  # be cache-eligible (README.md, "Caching").
  local cache
  cache=$(curl -sS -o /dev/null -D - -A "$RUSTKET_UA" --max-time 20 \
    "https://$REVIEW_HOST/file-upload/tunnel-cache-probe/probe.png" 2>/dev/null |
    sed -n 's/^cf-cache-status:[[:space:]]*\([A-Za-z]*\).*/\1/Ip' | head -n 1) || true
  case ${cache^^} in
    HIT | MISS | EXPIRED | STALE | UPDATING | REVALIDATED)
      log "warning: Cloudflare caches /file-upload/ (cf-cache-status: $cache): add the Cache Rule 'Bypass cache' for $REVIEW_HOST (README.md, \"Tunnel mode\")"
      ;;
    *) log "public /file-upload/*.png: cf-cache-status ${cache:-none} (DYNAMIC or BYPASS expected)" ;;
  esac
}

# Deletes the tunnel's CNAME through the Cloudflare API when CLOUDFLARE_API_TOKEN
# (Zone > DNS > Edit) is set; cloudflared itself cannot delete DNS records.
delete_dns_record() {
  local id=$1 zone="" zone_id="" rec rec_id content candidate=$REVIEW_HOST
  if [[ -z "${CLOUDFLARE_API_TOKEN:-}" ]]; then
    log "delete the DNS record $REVIEW_HOST (CNAME $id.cfargotunnel.com) in the Cloudflare dashboard, DNS > Records (or rerun with CLOUDFLARE_API_TOKEN set)"
    return 0
  fi
  cfapi() {
    curl -sS --max-time 30 -X "$1" "https://api.cloudflare.com/client/v4/$2" \
      -H @<(printf 'Authorization: Bearer %s\n' "$CLOUDFLARE_API_TOKEN")
  }
  while [[ "$candidate" == *.*.* ]]; do
    candidate=${candidate#*.}
    zone_id=$(cfapi GET "zones?name=$candidate" | jget 'd["result"][0]["id"]')
    if [[ -n "$zone_id" ]]; then
      zone=$candidate
      break
    fi
  done
  [[ -n "$zone_id" ]] || {
    log "warning: no zone for $REVIEW_HOST visible to CLOUDFLARE_API_TOKEN; delete the DNS record in the dashboard"
    return 0
  }
  rec=$(cfapi GET "zones/$zone_id/dns_records?type=CNAME&name=$REVIEW_HOST")
  rec_id=$(jget 'd["result"][0]["id"]' <<<"$rec")
  content=$(jget 'd["result"][0]["content"]' <<<"$rec")
  if [[ -z "$rec_id" ]]; then
    log "no CNAME $REVIEW_HOST in zone $zone"
  elif [[ "${content,,}" != "$id.cfargotunnel.com" ]]; then
    log "warning: CNAME $REVIEW_HOST points to $content, not to tunnel $NAME: left alone"
  elif ok "$(cfapi DELETE "zones/$zone_id/dns_records/$rec_id")"; then
    log "deleted DNS record $REVIEW_HOST"
  else
    log "warning: deleting DNS record $REVIEW_HOST failed; delete it in the dashboard"
  fi
}

down() {
  if command -v systemctl >/dev/null && [[ -f "$UNIT_FILE" ]]; then
    log "stopping and removing $UNIT"
    systemctl --user disable --now --quiet "$UNIT" || true
    rm -f "$UNIT_FILE"
    systemctl --user daemon-reload
  fi
  if [[ -f "$CONFIG" ]]; then
    rm -f "$CONFIG"
    log "removed $CONFIG"
  fi
  if ((DELETE_TUNNEL == 0)); then
    log "tunnel $NAME and the DNS record of $REVIEW_HOST are kept (--delete-tunnel removes them)"
    return 0
  fi
  require_cloudflared
  require_login
  local id
  id=$(tunnel_id)
  if [[ -z "$id" ]]; then
    log "no tunnel named $NAME"
    return 0
  fi
  delete_dns_record "$id"
  cf cleanup "$id" >/dev/null 2>&1 || true
  if cf delete "$id" >"$ERR" 2>&1; then
    log "deleted tunnel $NAME ($id)"
    rm -f "$CF_DIR/$id.json"
  else
    cat "$ERR" >&2
    die "deleting tunnel $NAME failed (connections can linger for a minute after the stop: rerun './tunnel.sh down --delete-tunnel')"
  fi
}

case $ACTION in
  up) up ;;
  status) status ;;
  down) down ;;
esac
