# App Review demo server

A small, always-on Rocket.Chat workspace that App Review signs in to when it tests
the Mac App Store build (plan blocker B8, decision D1). It runs on a VM the
organization controls, at a company subdomain, with a public HTTPS certificate,
over IPv4 and IPv6, and with fictional content only.

| File | Purpose |
|---|---|
| `docker-compose.yml` | Rocket.Chat (same tag as `dev/docker-compose.yml`), MongoDB 7.0 replica set |
| `docker-compose.override.yml` | Caddy (default mode); Compose loads it automatically |
| `Caddyfile` | TLS reverse proxy, Let's Encrypt certificate for `$REVIEW_HOST` |
| `docker-compose.tunnel.yml` | [Tunnel mode](#tunnel-mode-cloudflare-tunnel): no Caddy, Rocket.Chat on `127.0.0.1` only |
| `tunnel.sh` | Tunnel mode: the Cloudflare Tunnel, its DNS record and its systemd user unit |
| `.env.example` | Copy to `.env`: proxy mode, hostname, ACME email, image tags, review options |
| `setup.sh` | Starts the stack, creates the operator admin, applies settings, creates the review accounts, writes the review notes |
| `seed.sh` | Fictional workspace content ("Quillmoor Labs") |
| `lib.sh` | Helpers shared by the scripts |

Generated files, all `chmod 600` and git-ignored: `credentials.env` (every account and
password), `admin.env` (the operator account Rocket.Chat creates on its first start),
`review-notes.txt` (the App Store Connect review notes, filled in).

The default described below is a VM with its own public addresses and Caddy. A host
without public ports (a workstation, a NAT'ed box) can publish the same stack through a
Cloudflare Tunnel instead: see [Tunnel mode](#tunnel-mode-cloudflare-tunnel), which
replaces the firewall, DNS and Docker IPv6 requirements.

## Requirements

- **VM:** 2 vCPU, 4 GB RAM, 40 GB SSD, Ubuntu 24.04 LTS or Debian 12. A public IPv4
  address *and* a public IPv6 address (App Review may connect over IPv6/NAT64).
- **Software:** Docker Engine 27 or later with the Compose plugin 2.24 or later
  (`env_file` with `required: false`), `curl`, `python3`, `git`.
- **Firewall / security group:** inbound 22/tcp (SSH, ideally restricted to your
  addresses), 80/tcp (ACME HTTP-01 and the redirect), 443/tcp, 443/udp (HTTP/3).
  Nothing else. MongoDB has no published port. Keep outbound HTTPS open: besides
  Let's Encrypt, Rocket.Chat reports statistics to Rocket.Chat Cloud, and an
  unlicensed workspace that stops reporting is restricted after a grace period
  (Rocket.Chat's "air-gapped" restriction: sending messages fails).
- **DNS** for the subdomain, e.g. `review.example.com`:
  - an `A` record to the VM's IPv4 address,
  - an `AAAA` record to the VM's IPv6 address,
  - if the zone has `CAA` records, one must allow `letsencrypt.org`.
  - No CDN proxy, WAF or bot challenge in front (with Caddy, the record stays
    "DNS only", no Cloudflare orange cloud: Caddy needs the direct connection for its
    certificate). The app talks REST and WebSocket and cannot solve a challenge page;
    tunnel mode goes through Cloudflare's proxy on purpose and turns the challenges
    off for the hostname instead.
- **Docker IPv6:** with Docker 27+ the `edge` network gets a private IPv6 subnet
  without further configuration, and published ports are forwarded over IPv6 with
  `ip6tables` (on by default; do not set `"ip6tables": false` in
  `/etc/docker/daemon.json`). `docker network inspect rustket-review_edge` must show
  an IPv6 subnet; if `docker compose up` fails with an IPv6 address-pool error, the
  Docker Engine is older than 27.

## Deploy

```sh
git clone https://github.com/sokolcloud/rustket.git && cd rustket/deploy/review-server
cp .env.example .env
$EDITOR .env                  # REVIEW_HOST, ACME_EMAIL, REVIEW_SUPPORT_EMAIL
dig +short A "$(grep ^REVIEW_HOST .env | cut -d= -f2)"
dig +short AAAA "$(grep ^REVIEW_HOST .env | cut -d= -f2)"   # both must point at this VM
./setup.sh                    # first start takes a few minutes
./seed.sh
```

`setup.sh` (idempotent, safe to rerun outside a review):

1. generates `credentials.env` on the first run: the operator `rustket-ops` (for these
   scripts only, never given to Apple), `reviewer` (regular user), `review-admin`
   (workspace admin), `review-agent` (omnichannel agent), a password for the
   fictional users, the reviewer's E2E password and a random token for the
   seeded omnichannel visitor. Passwords are random, 23–41 characters, and
   appear in the log only masked;
2. writes `admin.env` and runs `docker compose up -d`; Rocket.Chat creates
   `rustket-ops` on its first start (`--no-up` skips this step);
3. waits until `https://$REVIEW_HOST/api/info` answers with a valid certificate;
4. applies the settings below with the 2FA headers (`x-2fa-method: password`,
   `x-2fa-code: sha256(password)`), writing only values that differ;
5. creates or heals the three review accounts (a password is reset only when it no
   longer works, so reruns keep sessions), makes `review-agent` an available agent;
6. writes `review-notes.txt`.

Settings applied:

| Area | Settings |
|---|---|
| Name | `Site_Name` = "Rustket Review" |
| Accounts | registration form disabled; no registration through auth services; password form shown; no anonymous read; email 2FA and TOTP off and email 2FA auto opt-in off (no SMTP here, and a reviewer cannot lock a shared account); no password reset, password, username or e-mail change; deleting one's own account per `REVIEW_ALLOW_DELETE_OWN_ACCOUNT` (default off) |
| Login services | LDAP, SAML, CAS and every built-in OAuth provider off: password login only |
| Failed logins | blocked by client address (Rocket.Chat's default: 50 failures, 5 minutes), not by user: the review usernames are public in this repo, and blocking by user would let anyone lock a review account |
| Moderation | bad-words filter on with `REVIEW_BAD_WORDS`; the moderation console permissions (`view-moderation-console`, `manage-moderation-actions`) kept on the admin role |
| Omnichannel | enabled, manual routing (a queued chat waits for the agent), chats accepted with no agent online |
| E2E | `E2E_Enable` = `REVIEW_E2E` (default true); not on by default for new DMs or groups |
| Uploads | enabled, `FileUpload_MaxFileSize` = `REVIEW_MAX_UPLOAD_BYTES` (5 MB); Caddy caps bodies at 25 MB (Cloudflare at 100 MB in tunnel mode) |

Rocket.Chat's own defaults stay as they are for everything else, notably the REST rate
limiter (Caddy, or Cloudflare in tunnel mode, passes the client address through
`X-Forwarded-For`, `HTTP_FORWARDED_COUNT=1`, so rate limits and failed-login blocking
see the real client).

`seed.sh` creates, as fictional users of a fictional company:

- `#general` greetings (only if it has no user messages yet), `#announcements`
  with a pinned welcome message, `#engineering` with a thread (three replies),
  reactions and a code block, `#random` with a poll (Rustket's `📊` format with
  keycap reactions as votes) and spam from `rex.hollis`, one of whose messages and
  the user himself are already reported, so the moderation console is not empty;
- the private group `leadership`, the team `product-team` with the discussion
  "Spring launch plan", the E2E group `secure-notes` (empty, see below);
- DMs from `nora.quill` and from `rex.hollis` (spam: the user to block and report)
  to `reviewer`, and one between two fictional users;
- an omnichannel chat from the visitor "Jordan Avery" waiting in the queue.

A room gets content only while it has no user messages, so reruns do not duplicate
anything; memberships are healed on every run. `./seed.sh --reset` deletes the seeded
rooms and DM messages and seeds again; `./seed.sh --clear` only deletes them.

### E2E

Rocket.Chat end-to-end keys are created by a client, not by the server, so the
`secure-notes` room starts empty and needs one manual step per deployment, with any
Rustket build:

1. Sign in as `reviewer`. Rustket creates the user's keys and shows "Save your new
   E2E password".
2. My account → Security → End-to-end encryption → Change E2E password: set
   `REVIEWER_E2E_PASSWORD` from `credentials.env`. It is in the review notes, so a
   reviewer on a fresh Mac can unlock the room.
3. Open `secure-notes` (this creates the room key) and send "This room is end-to-end
   encrypted." Sign out.

`./seed.sh --reset` recreates `secure-notes` without a key, so repeat step 3 after it
(the reviewer's keys and E2E password stay).

If this does not work reliably on the server, set `REVIEW_E2E=false` in `.env` and rerun
`./setup.sh`: E2E is switched off and the E2E lines disappear from `review-notes.txt`.

### Verify

```sh
H=review.example.com
curl -4 -sS "https://$H/api/info"; echo        # {"version":...,"success":true}
curl -6 -sS "https://$H/api/info"; echo        # same answer over IPv6
curl -sSI "http://$H/" | head -3               # 308 redirect to https
echo | openssl s_client -connect "$H:443" -servername "$H" 2>/dev/null | openssl x509 -noout -issuer -dates
docker compose ps                               # all three services healthy
```

Check IPv6 from a machine that has it (or an online IPv6 checker), not only from the VM.
Then do a dry run with the TestFlight or dmg build on a Mac: add the server, sign in
with each account, open a thread, vote in the poll, block and report `rex.hollis`, see the
reports in Administration → Moderation as `review-admin`, take Jordan Avery's chat from
the queue as `review-agent`, unlock `secure-notes` with the E2E password. Afterwards
unblock `rex.hollis` as `reviewer` (a block outlives `--reset`, and rex's DM could not be
seeded again), run `./seed.sh --reset` (new rooms and a new chat in the queue), repeat
E2E step 3, and dismiss the dry-run reports in the moderation console.

## Tunnel mode (Cloudflare Tunnel)

For a host with no public address or open ports, e.g. a workstation behind NAT, when
the hostname's zone is on Cloudflare. `cloudflared` on the host keeps outbound
connections to Cloudflare; Cloudflare's edge terminates TLS (its certificate), serves
IPv4, IPv6 and HTTP/3, and forwards requests for `$REVIEW_HOST` through the tunnel to
Rocket.Chat on `127.0.0.1:$REVIEW_TUNNEL_PORT`. Nothing listens publicly, Caddy does not
run, `ACME_EMAIL` and `ACME_CA` play no part, and the A/AAAA, CAA, firewall and Docker
IPv6 requirements above do not apply. Rocket.Chat keeps `HTTP_FORWARDED_COUNT=1`:
Cloudflare appends the address that connected to it to `X-Forwarded-For`, and
`cloudflared` adds no hop, so the last entry is the real client
(`docker-compose.tunnel.yml`).

Requirements: Docker as above, `cloudflared` 2024 or later, a systemd user session with
lingering on (`loginctl show-user "$USER" -p Linger` says `yes`, otherwise
`sudo loginctl enable-linger "$USER"`: the tunnel then starts at boot and survives
logout), and the Cloudflare account that holds the zone.

```sh
cp .env.example .env
$EDITOR .env      # REVIEW_PROXY=tunnel, uncomment COMPOSE_FILE, REVIEW_HOST, REVIEW_SUPPORT_EMAIL
                  # (REVIEW_TUNNEL_PORT=3010, REVIEW_TUNNEL_NAME=rustket-review by default)
cloudflared tunnel login     # once: open the URL, pick the zone of REVIEW_HOST -> ~/.cloudflared/cert.pem
./tunnel.sh       # tunnel, DNS record, ~/.cloudflared/<name>.yml, <name>.service
./setup.sh
./seed.sh
```

`tunnel.sh` (idempotent) reuses a tunnel named `REVIEW_TUNNEL_NAME` or creates it,
routes `REVIEW_HOST` to it with a proxied CNAME to `<tunnel id>.cfargotunnel.com` (it
stops if the hostname already has a record that points elsewhere: delete the leftover
or rerun with `--overwrite-dns`), writes `~/.cloudflared/<name>.yml` (mode 600: the
tunnel id, the credentials file, the ingress `REVIEW_HOST` → `http://127.0.0.1:<port>`,
everything else 404), and installs and starts the systemd user unit
`~/.config/systemd/user/<name>.service` (`cloudflared --no-autoupdate`, `Restart=always`).
`./tunnel.sh status` shows the unit, the tunnel's connections and the local, public and
WebSocket checks. The credentials file `~/.cloudflared/<tunnel id>.json` and `cert.pem`
are secrets outside the repository; back them up with `credentials.env`. Plain
`docker compose` commands in this README work unchanged because `COMPOSE_FILE` in `.env`
selects the tunnel file; the scripts pass the files explicitly and stop when
`COMPOSE_FILE` does not match `REVIEW_PROXY` (otherwise a plain `docker compose up -d`
would start Caddy on ports 80 and 443 of this host).

Cloudflare settings for the zone, in the dashboard (the app is not a browser and cannot
solve a challenge or a JavaScript check; a challenged request fails with HTTP 403 and
the header `cf-mitigated: challenge`):

- **Security → Bots:** Bot Fight Mode off (on the Free plan it cannot be scoped to one
  hostname); with Super Bot Fight Mode, "Definitely automated" and "Likely automated" set
  to Allow, or a skip rule for the hostname.
- **Security → Settings:** Security Level not "I'm Under Attack"; Browser Integrity Check
  off for the hostname if anything is challenged (a configuration rule can do that).
- **Security → WAF:** no custom rule, rate limiting rule or managed rule whose action is a
  (managed, JS or interactive) challenge or block for this hostname; if the zone needs
  one, add a skip rule for `http.host eq "<REVIEW_HOST>"` above it. Security → Events
  shows what was challenged and by which rule.
- **Network:** WebSockets on (the default); the app's DDP connection is a WebSocket to
  `/websocket`. Cloudflare closes idle WebSockets after 100 s, which Rocket.Chat's and the
  app's DDP pings stay well under.
- **Caching → Cache Rules:** a rule for `http.host eq "<REVIEW_HOST>"` with the action
  "Bypass cache" (available on Free). Rocket.Chat serves uploads with
  `Cache-Control: max-age=31536000` and no `private`, and Cloudflare caches paths with
  static extensions (`.png`, `.pdf`, ...) by default regardless of the app's auth headers
  and cookies: without the rule a private upload, once fetched, is served from the edge
  to anyone with its URL, even after a moderator deletes it. `./tunnel.sh status` warns
  when a `/file-upload/….png` path is still cache-eligible.
- **SSL/TLS:** Edge Certificates, Universal SSL active for the hostname (the default for
  a first-level subdomain; deeper names need Advanced Certificate Manager). The origin
  encryption mode does not matter for the tunnel.

Verify from another network, with the app's User-Agent:

```sh
H=review-rustket.example.com
UA='Rustket/0.3.0 (Macintosh; Mac OS X)'
curl -4 -sS -A "$UA" "https://$H/api/info"; echo     # {"version":...,"success":true}
curl -6 -sS -A "$UA" "https://$H/api/info"; echo     # same over IPv6
curl -sS -o /dev/null -D - -A "$UA" "https://$H/api/info" | grep -i '^cf-mitigated'   # must print nothing
curl --http1.1 -sS -o /dev/null -w '%{http_code}\n' --max-time 5 -A "$UA" \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' -H 'Sec-WebSocket-Version: 13' \
  -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' "https://$H/websocket"    # 101 (then a timeout, fine)
curl -sS -o /dev/null -D - -A "$UA" "https://$H/file-upload/x/probe.png" | grep -i '^cf-cache-status'
                                                    # DYNAMIC or BYPASS, never HIT/MISS
```

Once an image is posted in a private room (after `seed.sh`, or by hand), copy its
`/file-upload/<id>/<name>` URL and fetch it twice without credentials:
`curl -sS -o /dev/null -D - "https://$H/file-upload/<id>/<name>" | grep -iE '^HTTP|^cf-cache-status'`
must answer 403 both times, never with `cf-cache-status: HIT`.

Then check that Rocket.Chat sees client addresses, not `127.0.0.1` or the Docker gateway:
make one failed login from another network (`curl -sS -A "$UA" -H 'content-type:
application/json' -d '{"user":"reviewer","password":"wrong"}' "https://$H/api/v1/login"`)
and look at the address it recorded:

```sh
docker compose exec -T mongo mongosh rocketchat --quiet --eval \
  'db.rocketchat_server_events.find({t: "failed-login-attempt"}, {ip: 1, ts: 1}).sort({ts: -1}).limit(3).toArray()'
```

`ip` must be that network's public address. If it is a `172.x`/`127.0.0.1` address,
blocking by IP would lock everyone out after 50 failures: stop and fix
`HTTP_FORWARDED_COUNT` before the review.

The host serves the review, so while a review is open it must stay on and online:
suspend and hibernate off (e.g. `sudo systemctl mask sleep.target suspend.target
hibernate.target hybrid-sleep.target`, and no lid or idle suspend in the desktop's
power settings), no reboots, no network changes. The change freeze, backups, uptime
check and E2E steps below apply as they are; the uptime check also covers the tunnel
(Cloudflare answers 502 or 530 when the host or the tunnel is down). Cloudflare renews
the edge certificate by itself.

Teardown: `./tunnel.sh down` stops and removes the unit and the config and keeps the
tunnel and its DNS record; `./tunnel.sh down --delete-tunnel` also deletes the tunnel and
its credentials file, and the DNS record too when `CLOUDFLARE_API_TOKEN` (Zone → DNS →
Edit) is set in the environment (otherwise it says which record to delete in the
dashboard: `cloudflared` cannot delete DNS records). Then `docker compose down -v` as in
[Teardown](#teardown).

## Review notes

`review-notes.txt` is the plan's §7 draft with the placeholders filled in. Paste it into
App Store Connect → the version → App Review Information → Notes, and put the regular
user into the Sign-In fields. The placeholders and where their values come from:

| Placeholder | Value |
|---|---|
| `<demo-host>` | `https://$REVIEW_HOST` |
| Regular user `<user>` / `<password>` | `REVIEWER_USER` / `REVIEWER_PASSWORD` (also the Sign-In fields) |
| Workspace administrator | `REVIEW_ADMIN_USER` / `REVIEW_ADMIN_PASSWORD` |
| Omnichannel agent | `AGENT_USER` / `AGENT_PASSWORD` |
| E2E password (only with `REVIEW_E2E=true`) | `REVIEWER_E2E_PASSWORD` |
| `<support email>` | `REVIEW_SUPPORT_EMAIL` |
| `<e2e room>`, `<channel>` | `secure-notes`, `product-team` |
| Tested on | fill in by hand |

```
Rustket is a free, open-source native Mac client for an organization's own Rocket.Chat
server (self-hosted team chat). The developer operates no chat service; the server
below exists only for this review and stays online until review is complete.

Server: https://<demo-host>  (accept the terms on the first screen, then enter the server and sign in)
Accounts (no two-factor authentication):
- Regular user: <user> / <password>  (also in the Sign-In fields)
- Workspace administrator: <admin> / <password>
- Omnichannel agent: <agent> / <password>
- End-to-end encryption password of the regular user (asked for on a new Mac):
  <e2e password>

Moderation (Guideline 1.2): accounts and content are managed by each workspace's
administrators. In Rustket:
- Report a message: right-click a message > Report.
- Report a user: right-click one of their messages > Report user, or click their
  avatar > User info > Report user.
- Block a user: open the direct message, click the User info (i) button at the top
  right > Block (their messages are collapsed).
- Ignore a user in a channel: click their avatar > User info > Ignore.
- Hidden words filter: Settings (Rustket > Settings..., Cmd-,) > Privacy & storage >
  Hidden words.
- Terms (zero tolerance for objectionable content) are accepted on first launch.
Reports go to the workspace's moderation console (sign in as the administrator >
Administration > Moderation). On this server we act on reports within 24 hours.
Contact: <support email>.

Features that depend on server role or settings (Guideline 2.5.19): Administration and
the moderation console (admin account), the Omnichannel section of the sidebar (agent account), end-to-end
encrypted rooms (room "<e2e room>"), teams and discussions (channel "<channel>"), app
message blocks (rendered natively from JSON; no code is downloaded or executed).

Video calls open in the user's default browser. Updates come only from the Mac App
Store. Launch at login is off by default (Settings > General).
Encryption: standard TLS (rustls) and Rocket.Chat end-to-end encryption (RSA-OAEP, AES).
Tested on: <Apple silicon Mac>, macOS 27.2.1; <Intel Mac>, macOS 26.x.
```

Before each submission, check that the menu paths in the notes still match the build
being submitted, and do not mention external-browser sign-in (SSO) anywhere.

## Change freeze during review

From "Submit for Review" (and from an external TestFlight submission) until the decision:

- no `docker compose pull`, no image tag changes, no `up -d` after editing `.env`
  or `admin.env` (either would recreate containers);
- no `setup.sh`, `seed.sh` or `tunnel.sh` runs, no password changes, no settings changes
  in the admin UI, and in tunnel mode no changes to the zone's Cloudflare security
  settings or `cloudflared` upgrades;
- no host reboots: disable automatic reboots of unattended upgrades
  (`Unattended-Upgrade::Automatic-Reboot "false";`), apply kernel updates between reviews;
- watch the uptime check; if the server must be touched, do it, verify with the dry-run
  steps above, and say so in the reply to App Review.

Rocket.Chat's omnichannel visitor API is public by design: anyone who finds the
hostname (certificate transparency logs list it) can open a chat in the queue the
agent account sees. Right before each submission, sign in to the web UI
(`https://$REVIEW_HOST`) as `rustket-ops` and close any omnichannel chat that is not
Jordan Avery's (Omnichannel → Current Chats).

Leave what reviewers create (messages, blocks, reports) alone until the review ends.
Afterwards, act on their reports in the moderation console within the 24 hours the
notes promise, then `./seed.sh --reset` if the content needs a clean slate.

## Backups

```sh
# Database (users, messages, settings, uploads in GridFS):
docker compose exec -T mongo mongodump --archive --gzip > "rc-$(date +%F).archive.gz"
# Restore (Rocket.Chat stopped meanwhile, it caches settings and users):
docker compose stop rocketchat
docker compose exec -T mongo mongorestore --archive --gzip --drop < rc-YYYY-MM-DD.archive.gz
docker compose start rocketchat
```

Also keep `.env` and `credentials.env` in a password manager or an encrypted backup:
without `credentials.env` the operator account and the review accounts are lost (rerunning
`setup.sh` would generate new passwords, and Rocket.Chat would not recreate the operator
on an existing database). A nightly cron of the dump with a few days of rotation is enough;
the content is fictional and can always be seeded again.

## Updates

Only between reviews, and only to the Rocket.Chat version the app is tested against:

1. bump `ROCKETCHAT_IMAGE` in `.env` together with `dev/docker-compose.yml` (the app's
   live tests run against that tag);
2. take a backup, then `docker compose pull && docker compose up -d`;
3. `./setup.sh --no-up` (settings can be renamed between versions; warnings name
   them) and repeat the dry run.

MongoDB major upgrades follow MongoDB's path (one major at a time, then set
`featureCompatibilityVersion`). Caddy renews the certificate by itself; keep the
`caddy-data` volume, it holds the ACME account and certificates.

## Uptime check

Use an external HTTP monitor (any hosted uptime service or a cron on another machine),
every 5 minutes, alerting by e-mail or chat:

- `GET https://$REVIEW_HOST/api/info` must return 200 and contain `"success":true`,
  checked over IPv4 and over IPv6 separately if the monitor allows it;
- certificate expiry warning at 14 days (Caddy renews at about 30 days left).

A minimal cron alternative on another host:

```sh
*/5 * * * * for f in -4 -6; do curl $f -fsS --max-time 20 https://review.example.com/api/info | grep -q '"success":true' || echo "review server down ($f)"; done
```

## Operations

```sh
docker compose ps
docker compose logs -f --tail=100 rocketchat     # or caddy, mongo
docker compose restart rocketchat
```

`admin.env` holds the operator password; `docker compose config` and `docker inspect`
print it, so keep the VM's Docker access to administrators. MongoDB runs without
authentication on an internal Docker network with no published port. Caddy keeps no
access log (it would record Rocket.Chat's `X-Auth-Token` headers); its log shows
certificate and proxy errors only.

Troubleshooting:

- **No certificate:** DNS does not point at the VM yet, port 80 is closed, or a `CAA`
  record forbids Let's Encrypt (`docker compose logs caddy`). For first experiments set
  `ACME_CA` to the staging URL in `.env` to avoid rate limits. `setup.sh` needs a
  trusted certificate, so with staging it stops at the wait with a hint once Caddy has
  the staging certificate; then remove `ACME_CA`, `docker compose up -d caddy`, and
  rerun `./setup.sh`.
- **`cannot log in as rustket-ops`:** the database was created before `admin.env`
  existed or with other credentials. Before any review it is simplest to start over:
  `docker compose down -v`, then `./setup.sh`.
- **No omnichannel chat:** omnichannel was off when `seed.sh` ran; rerun `./setup.sh --no-up`
  and `./seed.sh`.

Trying the scripts against another server (never production): `REVIEW_SERVER` overrides
the URL, `CREDENTIALS_FILE` points at another credentials file, and `SEED_SUFFIX` appends a
suffix to every seeded user and room name so nothing clashes on a shared test server.
`setup.sh` changes global settings, so only run it against a server set up for this.

## Teardown

Keep the server for as long as the app is on the store: every update goes through App
Review again. When it is no longer needed:

```sh
docker compose down -v        # deletes containers, the database and the certificates
```

then delete the DNS records (tunnel mode: `./tunnel.sh down --delete-tunnel`), the VM and its snapshots, and `credentials.env` from wherever
it was backed up. Remove the review accounts from App Store Connect's App Review
Information.
