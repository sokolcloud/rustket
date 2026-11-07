#!/usr/bin/env python3
# Diagnostic: drives the legacy Rocket.Chat OAuth flow through the dev Keycloak
# without a browser and logs in with the resulting rocketchat://auth credentials.
# Usage: python3 -I dev/oauth_probe.py   (needs dev/setup-oauth.sh to have run)
import urllib.error, base64, json, re, secrets, urllib.parse, urllib.request, http.cookiejar, html, sys
srv = "http://localhost:3000"
tok = secrets.token_urlsafe(32)[:43]
state = base64.urlsafe_b64encode(json.dumps({"loginStyle":"redirect","credentialToken":tok,"isCordova":True,"redirectUrl":"rocketchat://auth"}).encode()).decode()
q = urllib.parse.urlencode({"client_id":"rocketchat","redirect_uri":f"{srv}/_oauth/keycloak","response_type":"code","state":state,"scope":"openid email profile"})
url = "http://localhost:8081/realms/rustket/protocol/openid-connect/auth?" + q
cj = http.cookiejar.CookieJar()
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if newurl.startswith("rocketchat://"):
            print("REDIRECT ->", newurl[:200]); return None
        return super().redirect_request(req, fp, code, msg, headers, newurl)
op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cj), NoRedirect)
page = op.open(url).read().decode()
for c in cj: c.secure = False  # browsers send Secure cookies to http://localhost
action = html.unescape(re.search(r'action="([^"]+)"', page).group(1))
try:
    r = op.open(action, urllib.parse.urlencode({"username":"kcuser","password":"kcpass","credentialId":""}).encode())
except urllib.error.HTTPError as e:
    print("HTTPError", e.code, e.url[:300]); b=e.read().decode(); print(re.findall(r"(?:kc-feedback-text|pf-v5-c-alert__title)[^>]*>([^<]+)", b)[:3]); print([c.name+":"+str(c.secure) for c in cj]); sys.exit(1)
body = r.read().decode()
print("final url:", r.geturl())
m = re.search(r"rocketchat://auth[^\"'<\s]*", body)
cfg = re.search(r'id="config"[^>]*>([^<]+)<', body)
print("deeplink in body:", m.group(0)[:200] if m else None)
print(body[:3000])
cfgj = json.loads(html.unescape(re.search(r'id="config"[^>]*>([^<]+)<', body).group(1)))
link = cfgj["redirectUrl"]
print("deeplink:", link[:120], "...")
q2 = urllib.parse.parse_qs(urllib.parse.urlparse(link).query)
req = urllib.request.Request(srv + "/api/v1/login", data=json.dumps({"oauth": {"credentialToken": q2["credentialToken"][0], "credentialSecret": q2["credentialSecret"][0]}}).encode(), headers={"content-type": "application/json"})
try:
    resp = json.loads(urllib.request.urlopen(req).read())
    print("login:", resp["status"], resp["data"]["me"]["username"], resp["data"]["me"].get("emails"))
except urllib.error.HTTPError as e:
    print("login failed", e.code, e.read()[:300])
