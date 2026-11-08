//! Decrypting messages in place (§7.7, `rustket_core::e2e::shown`).
//!
//! Every `t: 'e2e'` message the app shows passes through [`process`] before it
//! is drawn: the decrypted `msg` (with an `md` tree parsed here), attachments
//! and files replace the server values, `content` keeps the envelope (a legacy
//! v1 ciphertext in `msg` is moved there), and `Message::e2e` gets a marker.
//! Markers carry a per-process nonce (`rustket_core::e2e::shown::stored`), so
//! a marker the server stored is never believed; what was processed is also
//! recorded per `(rid, _id)` with `_updatedAt` (the last few versions: copies
//! of one message in the timeline, the room's last message and the panels can
//! hold different versions), so an edit is decrypted again.
//! Server-side plaintext stored next to the envelope (`blocks`, `urls`,
//! attachments the payload does not have) is never shown as part of the
//! decrypted message; the message gets the plaintext note instead (§4).
//! Messages that could not be decrypted yet are tried again when keys change
//! ([`keys_changed`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use rocketchat::ServerUrl;
use rocketchat::models::{Message, MessageAttachment};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::e2e::shown as mark;
use rustket_core::e2e::{DecryptFailure, DecryptResult, E2eManager};
use serde_json::Value;

use super::{files, markdown};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    updated: Option<i64>,
    marker: &'static str,
    /// The server message carried plaintext next to `content` (§11.14).
    plain: bool,
    /// [`EPOCH`] when it was processed (non-final results are retried after
    /// a change).
    epoch: u64,
    /// Decrypted with all its quotes resolved: never processed again.
    done: bool,
    /// [`USES`] at the last lookup or write (the least recently used version
    /// is forgotten first).
    used: u64,
}

/// Processed versions of one message (newest last).
type Versions = Vec<Seen>;

/// Versions remembered per message. Every copy that is shown is looked up
/// on each pass, so copies in use are never the least recently used one
/// unless more than this many versions are shown at once.
const MAX_VERSIONS: usize = 16;

/// Lookup counter for [`Seen::used`].
static USES: AtomicU64 = AtomicU64::new(1);

fn next_use() -> u64 {
    USES.fetch_add(1, Ordering::Relaxed)
}

fn seen() -> &'static Mutex<HashMap<(String, String), Versions>> {
    static S: OnceLock<Mutex<HashMap<(String, String), Versions>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn seen_lock() -> std::sync::MutexGuard<'static, HashMap<(String, String), Versions>> {
    seen()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Bumped whenever keys, the account state or fetched quotes change.
static EPOCH: AtomicU64 = AtomicU64::new(1);

/// Room keys, the account state or the quote cache changed: messages that
/// were not decrypted (or had unresolved quotes) are processed again.
pub fn keys_changed() {
    EPOCH.fetch_add(1, Ordering::SeqCst);
}

/// File keys were dropped from the registry: decrypted messages are
/// processed again so their attachments are registered again.
pub fn forget_files() {
    for s in seen_lock().values_mut().flatten() {
        s.done = false;
    }
    keys_changed();
}

/// The current epoch (see [`keys_changed`]).
pub fn current_epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}

fn updated_ms(m: &Message) -> Option<i64> {
    m.updated_at
        .as_ref()
        .map(rocketchat::RcDate::timestamp_millis)
}

/// The marker of `m` if this process wrote it (markers carry a per-process
/// nonce the server cannot know) for this version (when it is recorded).
pub fn verified_marker(m: &Message) -> Option<&'static str> {
    let marker = mark::marker(m)?;
    let updated = updated_ms(m);
    let mut table = seen_lock();
    let Some(versions) = table.get_mut(&(m.rid.clone(), m.id.clone())) else {
        return Some(marker);
    };
    let s = versions.iter_mut().find(|s| s.updated == updated)?;
    s.used = next_use();
    Some(marker)
}

/// The record of this version of `m` whose marker `m` carries.
fn prior_record(m: &Message, updated: Option<i64>) -> Option<Seen> {
    let marker = mark::marker(m)?;
    let mut table = seen_lock();
    let s = table
        .get_mut(&(m.rid.clone(), m.id.clone()))?
        .iter_mut()
        .find(|s| s.updated == updated && s.marker == marker)?;
    s.used = next_use();
    Some(*s)
}

/// Whether `m` is shown decrypted (verified).
pub fn is_decrypted(m: &Message) -> bool {
    matches!(
        verified_marker(m),
        Some(mark::DECRYPTED | mark::DECRYPTED_PLAINTEXT)
    )
}

/// Where quoted messages come from.
pub trait Quotes {
    /// A quoted message by id (decrypted when it is encrypted), if known.
    fn get(&self, mid: &str) -> Option<Message>;
    /// A quoted message that is not known locally (fetched by the caller).
    fn want(&self, _mid: &str) {}
}

/// Inputs of [`process`].
pub struct Env<'a> {
    pub mgr: Option<E2eManager>,
    pub server: &'a ServerUrl,
    /// `Site_Url` (permalinks of quotes), else the server URL.
    pub site_url: String,
    pub quotes: &'a dyn Quotes,
}

impl<'a> Env<'a> {
    pub fn new(ws: &'a Workspace, server: &'a ServerUrl, quotes: &'a dyn Quotes) -> Self {
        let site_url = ws
            .settings()
            .str("Site_Url")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(server.as_str())
            .trim_end_matches('/')
            .to_owned();
        Self {
            mgr: ws.e2e().cloned(),
            server,
            site_url,
            quotes,
        }
    }
}

/// Quotes looked up in the workspace only.
pub struct WorkspaceQuotes<'a>(pub &'a Workspace);

impl Quotes for WorkspaceQuotes<'_> {
    fn get(&self, mid: &str) -> Option<Message> {
        find_local(self.0, mid)
    }
}

/// A message of any open timeline of `ws`.
pub fn find_local(ws: &Workspace, mid: &str) -> Option<Message> {
    for rid in ws.rooms_with_timelines() {
        if let Some(i) = ws.timeline(&rid).and_then(|t| t.get(mid)) {
            return Some(i.message.clone());
        }
    }
    None
}

/// Processes one message; returns whether it changed.
pub fn process(m: &mut Message, env: &Env<'_>) -> bool {
    match m.t.as_deref() {
        Some("e2e") => process_e2e(m, env, true),
        Some("message_pinned_e2e") => process_pinned(m, env),
        _ => {
            // A marker on anything else came from the server.
            if mark::has_marker_prefix(m) {
                m.e2e = None;
                return true;
            }
            false
        }
    }
}

/// The raw form of a message processed before (for re-decryption).
pub fn raw_view(m: &Message) -> Message {
    let mut raw = m.clone();
    if mark::has_marker_prefix(m) {
        raw.msg.clear();
        raw.md = None;
        raw.e2e = None;
    }
    raw
}

fn is_decrypted_marker(marker: &str) -> bool {
    matches!(marker, mark::DECRYPTED | mark::DECRYPTED_PLAINTEXT)
}

/// Most messages remembered; the table starts over beyond that (processed
/// copies keep their markers, see [`process_e2e`]).
const MAX_SEEN: usize = 50_000;

fn record(m: &Message, seen: Seen) {
    let mut table = seen_lock();
    if table.len() >= MAX_SEEN {
        table.clear();
    }
    let versions = table.entry((m.rid.clone(), m.id.clone())).or_default();
    let seen = Seen {
        used: next_use(),
        ..seen
    };
    match versions.iter_mut().find(|s| s.updated == seen.updated) {
        // Another copy of this version (a panel list, the thread view) is
        // shown decrypted: keep that record, so its plaintext is never
        // mistaken for a server message. Copies that are not decrypted hold
        // no plaintext.
        Some(old) if is_decrypted_marker(old.marker) && !is_decrypted_marker(seen.marker) => {
            old.used = seen.used;
        }
        Some(old) => *old = seen,
        None => {
            if versions.len() >= MAX_VERSIONS
                && let Some(lru) = versions
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, s)| s.used)
                    .map(|(i, _)| i)
            {
                versions.remove(lru);
            }
            versions.push(seen);
        }
    }
}

fn is_removed_file(a: &MessageAttachment) -> bool {
    a.kind.as_deref() == Some("removed-file")
}

/// Drops server-side plaintext the client never shows on an `e2e` message
/// (UIKit blocks, URL previews); `true` if there was any.
fn drop_server_extras(m: &mut Message) -> bool {
    let blocks = m
        .blocks
        .take()
        .is_some_and(|b| !(b.is_null() || b.as_array().is_some_and(Vec::is_empty)));
    let urls = m.urls.take().is_some_and(|u| !u.is_empty());
    blocks || urls
}

fn process_e2e(m: &mut Message, env: &Env<'_>, with_quotes: bool) -> bool {
    let epoch = EPOCH.load(Ordering::SeqCst);
    let updated = updated_ms(m);
    let prior = prior_record(m, updated).or_else(|| {
        // Processed by this process, but the record is gone.
        mark::marker(m).map(|marker| Seen {
            updated,
            marker,
            plain: marker == mark::DECRYPTED_PLAINTEXT,
            epoch: 0,
            done: false,
            used: 0,
        })
    });
    // The attachments are still the server's (not yet replaced by a
    // decrypted payload and local quotes in an earlier pass).
    let server_attachments = prior.is_none_or(|s| !is_decrypted_marker(s.marker));
    let (result, mut plain) = match prior {
        Some(s) if s.done || s.epoch == epoch => return false,
        Some(s) => {
            let r = match &env.mgr {
                Some(mgr) => mgr.decrypt_message(&raw_view(m)),
                None => DecryptResult::Pending,
            };
            (r, s.plain)
        }
        None => {
            // Straight from the server (any marker on it is not ours).
            let ciphertext = m
                .content
                .as_ref()
                .and_then(|c| c.get("ciphertext"))
                .and_then(Value::as_str);
            let plain = m.content.is_some() && !m.msg.is_empty() && ciphertext != Some(&m.msg);
            let extras = drop_server_extras(m);
            m.e2e = None;
            m.md = None;
            let r = match &env.mgr {
                Some(mgr) => mgr.decrypt_message(m),
                None => DecryptResult::Pending,
            };
            // Keep a legacy v1 ciphertext with the envelope.
            if m.content.is_none() && !m.msg.is_empty() {
                m.content = Some(serde_json::json!({
                    "algorithm": "rc.v1.aes-sha2",
                    "ciphertext": m.msg,
                }));
            }
            (r, plain || extras)
        }
    };
    let mut done = false;
    let marker = match result {
        DecryptResult::Decrypted(d) => {
            let p = &d.message;
            m.msg = p.text().to_owned();
            let md = markdown::parse(&m.msg);
            m.md = (!md.is_empty()).then_some(md);
            if let Some(atts) = &p.attachments {
                let mut atts = atts.clone();
                for (i, a) in atts.iter_mut().enumerate() {
                    files::mark_attachment(a, env.server, &m.rid, &m.id, i);
                }
                m.attachments = Some(atts);
            } else if server_attachments && let Some(server) = m.attachments.take() {
                // Attachments only the server has: plaintext anyone with
                // database access could add. With an envelope only the
                // `removed-file` placeholders stay; without one (§11.13) the
                // server's attachment is all there is, shown with the note.
                let unencrypted = server.iter().any(|a| !is_removed_file(a));
                if m.content.is_some() {
                    let kept: Vec<_> = server.into_iter().filter(is_removed_file).collect();
                    m.attachments = (!kept.is_empty()).then_some(kept);
                } else {
                    m.attachments = Some(server);
                }
                plain |= unencrypted;
            }
            if let Some(f) = &p.files {
                m.files = Some(f.clone());
            }
            if let Some(f) = &p.file {
                m.file = Some(f.clone());
            }
            done = !with_quotes || add_quotes(m, env);
            if d.plaintext_edit || plain {
                mark::DECRYPTED_PLAINTEXT
            } else {
                mark::DECRYPTED
            }
        }
        DecryptResult::Pending | DecryptResult::NotEncrypted => {
            m.msg.clear();
            m.md = None;
            let has_keys = env
                .mgr
                .as_ref()
                .is_some_and(|g| g.account_state().has_keys());
            if has_keys {
                mark::WAITING
            } else {
                mark::LOCKED
            }
        }
        DecryptResult::Failed(f) => {
            m.msg.clear();
            m.md = None;
            match f {
                DecryptFailure::NoKey => mark::NO_KEY,
                DecryptFailure::KeyError => mark::KEY_ERROR,
                DecryptFailure::Unsupported(_) | DecryptFailure::Malformed => mark::UNSUPPORTED,
            }
        }
    };
    mark::set_marker(m, marker);
    record(
        m,
        Seen {
            updated,
            marker,
            plain,
            epoch,
            done,
            used: 0,
        },
    );
    true
}

/// `message_pinned_e2e`: the pinned message's text is decrypted into the
/// first attachment (§7.7).
fn process_pinned(m: &mut Message, env: &Env<'_>) -> bool {
    let epoch = EPOCH.load(Ordering::SeqCst);
    let updated = updated_ms(m);
    let prior = prior_record(m, updated);
    if prior.is_some_and(|s| s.done || s.epoch == epoch) {
        return false;
    }
    let result = match &env.mgr {
        Some(mgr) => mgr.decrypt_pinned(&raw_view(m)),
        None => DecryptResult::Pending,
    };
    let (text, marker, done) = match result {
        DecryptResult::Decrypted(d) => (d.message.text().to_owned(), mark::DECRYPTED, true),
        _ => (
            t!("e2e.encrypted_message").into_owned(),
            mark::LOCKED,
            false,
        ),
    };
    if let Some(a) = m.attachments.as_mut().and_then(|a| a.first_mut()) {
        a.text = Some(text);
        a.md = None;
    }
    mark::set_marker(m, marker);
    record(
        m,
        Seen {
            updated,
            marker,
            plain: false,
            epoch,
            done,
            used: 0,
        },
    );
    true
}

/// Message ids quoted by permalinks to this server in `text` (`?msg=<id>`).
pub fn quoted_ids(text: &str, site_url: &str, server: &ServerUrl) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bases = [
        site_url.trim_end_matches('/'),
        server.as_str().trim_end_matches('/'),
    ];
    for word in
        text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | '<' | '>'))
    {
        if !bases.iter().any(|b| !b.is_empty() && word.starts_with(b)) {
            continue;
        }
        let Some((_, query)) = word.split_once('?') else {
            continue;
        };
        let ids: Vec<&str> = query
            .split('&')
            .filter_map(|kv| kv.strip_prefix("msg="))
            .filter(|v| !v.is_empty())
            .collect();
        if let [id] = ids.as_slice()
            && !out.iter().any(|(_, i): &(String, String)| i == id)
        {
            out.push((word.to_owned(), (*id).to_owned()));
        }
    }
    out
}

/// Most quotes resolved per message (the web's default
/// `Message_QuoteChainLimit` is 2 levels; a message rarely quotes more).
const MAX_QUOTES: usize = 5;

/// Appends quote attachments for the permalinks in `m.msg` (the server
/// cannot build them for encrypted text, §7.7). Returns whether every quote
/// was resolved.
fn add_quotes(m: &mut Message, env: &Env<'_>) -> bool {
    let links = quoted_ids(&m.msg, &env.site_url, env.server);
    if links.is_empty() {
        return true;
    }
    let mut all = true;
    for (url, id) in links.into_iter().take(MAX_QUOTES) {
        let present = m
            .attachments
            .iter()
            .flatten()
            .any(|a| a.message_link.as_deref() == Some(url.as_str()));
        if present || id == m.id {
            continue;
        }
        let Some(mut q) = env.quotes.get(&id) else {
            env.quotes.want(&id);
            all = false;
            continue;
        };
        if q.t.as_deref() == Some("e2e") && !is_decrypted(&q) {
            process_e2e(&mut q, env, false);
            if !is_decrypted(&q) {
                all = false;
                continue;
            }
        }
        let username = q.u.username.clone().unwrap_or_default();
        let quote = MessageAttachment {
            text: Some(q.msg.clone()),
            message_link: Some(url),
            author_name: Some(
                q.u.username
                    .clone()
                    .or_else(|| q.u.name.clone())
                    .unwrap_or_default(),
            ),
            author_icon: (!username.is_empty()).then(|| format!("/avatar/{username}")),
            ts: q.ts,
            ..Default::default()
        };
        m.attachments.get_or_insert_with(Vec::new).push(quote);
    }
    all
}

/// Quotes from a map, recording the ids it does not have.
struct MapQuotes<'a> {
    map: &'a HashMap<String, Message>,
    wants: std::cell::RefCell<Vec<String>>,
}

impl Quotes for MapQuotes<'_> {
    fn get(&self, mid: &str) -> Option<Message> {
        self.map.get(mid).cloned()
    }

    fn want(&self, mid: &str) {
        let mut w = self.wants.borrow_mut();
        if !w.iter().any(|x| x == mid) {
            w.push(mid.to_owned());
        }
    }
}

/// Processes every message of the open timelines and the last messages of
/// `ws`. Quotes come from the open timelines and `cache`; returns whether
/// anything changed and the quoted ids found nowhere (to fetch).
pub fn process_workspace(
    ws: &mut Workspace,
    server: &ServerUrl,
    cache: &HashMap<String, Message>,
) -> (bool, Vec<String>) {
    let run = |ws: &mut Workspace, map: &HashMap<String, Message>| {
        let quotes = MapQuotes {
            map,
            wants: Default::default(),
        };
        let (mgr, site_url) = {
            let e = Env::new(&*ws, server, &quotes);
            (e.mgr, e.site_url)
        };
        let env = Env {
            mgr,
            server,
            site_url,
            quotes: &quotes,
        };
        let mut changed = false;
        for rid in ws.rooms_with_timelines() {
            changed |= ws.rewrite_messages(&rid, &mut |m| process(m, &env));
        }
        changed |= ws.rewrite_last_messages(&mut |m| process(m, &env));
        drop(env);
        (changed, quotes.wants.into_inner())
    };
    let (mut changed, wants) = run(ws, cache);
    if wants.is_empty() {
        return (changed, wants);
    }
    let found: HashMap<String, Message> = wants
        .iter()
        .filter_map(|id| find_local(ws, id).map(|m| (id.clone(), m)))
        .collect();
    if found.is_empty() {
        return (changed, wants);
    }
    let mut map = cache.clone();
    map.extend(found);
    keys_changed();
    let (again, wants) = run(ws, &map);
    changed |= again;
    (changed, wants)
}

/// Processes a list (panel results).
pub fn process_list(ws: &Workspace, server: &ServerUrl, list: &mut [Message]) {
    let quotes = WorkspaceQuotes(ws);
    let env = Env::new(ws, server, &quotes);
    for m in list {
        process(m, &env);
    }
}

/// The marker as a short state for views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Decrypted {
        plaintext: bool,
    },
    Locked,
    Waiting,
    NoKey,
    KeyError,
    Unsupported,
    /// Not processed (e.g. drawn outside the app).
    Raw,
}

/// How an `e2e` message is shown; `None` for other messages.
pub fn state(m: &Message) -> Option<State> {
    if m.t.as_deref() != Some("e2e") {
        return None;
    }
    Some(match verified_marker(m) {
        Some(mark::DECRYPTED) => State::Decrypted { plaintext: false },
        Some(mark::DECRYPTED_PLAINTEXT) => State::Decrypted { plaintext: true },
        Some(mark::LOCKED) => State::Locked,
        Some(mark::WAITING) => State::Waiting,
        Some(mark::NO_KEY) => State::NoKey,
        Some(mark::KEY_ERROR) => State::KeyError,
        Some(mark::UNSUPPORTED) => State::Unsupported,
        _ => State::Raw,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;

    struct NoQuotes;
    impl Quotes for NoQuotes {
        fn get(&self, _: &str) -> Option<Message> {
            None
        }
    }

    fn msg(id: &str, v: Value) -> Message {
        let mut base = serde_json::json!({"_id": id, "rid": "r-shown", "u": {"_id": "u1", "username": "bob"},
            "_updatedAt": {"$date": 1_000}});
        if let (Some(b), Some(o)) = (base.as_object_mut(), v.as_object()) {
            for (k, x) in o {
                b.insert(k.clone(), x.clone());
            }
        }
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn without_keys_messages_are_locked_and_spoofed_markers_ignored() {
        let server = ServerUrl::parse("http://localhost:3000").unwrap();
        let ws = Workspace::new();
        let env = Env::new(&ws, &server, &NoQuotes);
        // A sender spoofs the marker and puts plaintext into `msg`.
        let mut m = msg(
            "spoof1",
            serde_json::json!({"t": "e2e", "msg": "plaintext!", "e2e": "rustket:ok",
                "content": {"algorithm": "rc.v2.aes-sha2", "kid": "k", "iv": "AAAA", "ciphertext": "AAAA"}}),
        );
        assert_eq!(state(&m), Some(State::Raw));
        assert!(process(&mut m, &env));
        assert_eq!(state(&m), Some(State::Locked));
        assert!(m.msg.is_empty());
        assert!(!mark::is_decrypted(&m));
        // Processed once per epoch.
        assert!(!process(&mut m, &env));
        keys_changed();
        assert!(process(&mut m, &env));
        // A newer version from the server is processed again.
        let mut edited = m.clone();
        edited.updated_at = rocketchat::RcDate::from_millis(2_000);
        assert_eq!(state(&edited), Some(State::Raw));
    }

    #[test]
    fn forged_markers_on_redelivered_copies_are_ignored() {
        let server = ServerUrl::parse("http://localhost:3000").unwrap();
        let ws = Workspace::new();
        let env = Env::new(&ws, &server, &NoQuotes);
        let raw = serde_json::json!({"t": "e2e", "msg": "from the server", "e2e": "rustket:ok+plain",
            "content": {"algorithm": "rc.v2.aes-sha2", "kid": "k", "iv": "AAAA", "ciphertext": "AAAA"}});
        let mut first = msg("spoof2", raw.clone());
        process(&mut first, &env);
        // The same version arrives again (rooms-changed, a reopened room).
        let mut again = msg("spoof2", raw);
        assert_eq!(state(&again), Some(State::Raw));
        assert!(process(&mut again, &env));
        assert!(again.msg.is_empty());
        assert!(again.md.is_none());
        assert!(!mark::is_decrypted(&again));
    }

    /// A read-only manager (E2E off, cached key, server unreachable) holding
    /// a room key for `rid`.
    pub(crate) async fn manager_with_key(
        rid: &str,
    ) -> (E2eManager, rocketchat::e2e::RoomKey, ServerUrl) {
        let server = ServerUrl::parse("http://127.0.0.1:9").unwrap();
        manager_on(rid, server, Default::default()).await
    }

    /// My key pair of the fixtures (public JWK, private JWK, key backup).
    pub(crate) fn my_keys() -> (String, String, String) {
        let path = format!(
            "{}/../rocketchat/tests/fixtures/e2e/keychain_pwdv2web.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let kc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let s = |k: &str| kc[k].as_str().unwrap().to_owned();
        (s("public_key"), s("private_key"), s("persisted"))
    }

    /// A local HTTP server answering every request with `body` (JSON).
    pub(crate) async fn stub_server(body: String) -> ServerUrl {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        ServerUrl::parse(&format!("http://127.0.0.1:{port}")).unwrap()
    }

    /// A ready manager (E2E on, keys on a stub server) holding a room key
    /// for `rid`.
    pub(crate) async fn ready_manager_with_key(
        rid: &str,
    ) -> (E2eManager, rocketchat::e2e::RoomKey, ServerUrl) {
        let (public, _, backup) = my_keys();
        let body =
            serde_json::json!({"public_key": public, "private_key": backup, "success": true});
        let server = stub_server(body.to_string()).await;
        let settings = rustket_core::e2e::E2eSettings {
            enabled: true,
            ..Default::default()
        };
        let (mgr, key, server) = manager_on(rid, server, settings).await;
        assert!(mgr.account_state().is_ready());
        (mgr, key, server)
    }

    async fn manager_on(
        rid: &str,
        server: ServerUrl,
        settings: rustket_core::e2e::E2eSettings,
    ) -> (E2eManager, rocketchat::e2e::RoomKey, ServerUrl) {
        use rocketchat::e2e::{RoomKey, UserPublicKey};
        use rocketchat::models::{Room, RoomType, Subscription};
        use rustket_core::secrets::SecretStore;
        let (public, private, _) = my_keys();
        let sid = rustket_core::ServerId("s-shown".into());
        let store = std::sync::Arc::new(rustket_core::secrets::MemoryStore::new());
        store
            .set(&rustket_core::e2e::private_key_secret(&sid, "me"), &private)
            .unwrap();
        let tls = crate::session::tls_for(&[]).unwrap();
        let rest = rocketchat::RestClient::new(server.clone(), &tls).unwrap();
        let mgr = E2eManager::new(
            rest,
            store,
            &sid,
            "me",
            settings,
            rustket_core::e2e::E2eConfig {
                auto_distribution: false,
                ..Default::default()
            },
        );
        mgr.start().await.unwrap();
        let key = RoomKey::generate();
        let public = UserPublicKey::from_jwk_str(&public).unwrap();
        mgr.update_room(&Room {
            id: rid.into(),
            t: RoomType::Private,
            encrypted: Some(true),
            e2e_key_id: Some(key.kid.clone()),
            ..Default::default()
        })
        .await;
        mgr.update_subscription(&Subscription {
            id: format!("sub-{rid}"),
            rid: rid.into(),
            t: RoomType::Private,
            e2e_key: Some(key.wrap_for(&public).unwrap()),
            ..Default::default()
        })
        .await;
        (mgr, key, server)
    }

    struct OneQuote(Message);
    impl Quotes for OneQuote {
        fn get(&self, mid: &str) -> Option<Message> {
            (self.0.id == mid).then(|| self.0.clone())
        }
    }

    #[tokio::test]
    async fn decrypts_in_place_with_files_quotes_and_plaintext_note() {
        let (mgr, key, server) = manager_with_key("r-dec").await;
        let enc = |v: Value| key.encrypt_json(&v).unwrap().to_json();
        let mk = |id: &str, v: Value| {
            let mut m = msg(id, v);
            m.rid = "r-dec".into();
            m
        };
        let file_key = rocketchat::e2e::FileKey::generate();
        let quoted = mk(
            "q1",
            serde_json::json!({"t": "e2e", "content": enc(serde_json::json!({"msg": "the *original*"}))}),
        );
        let quotes = OneQuote(quoted);
        let ws = Workspace::new();
        let mut env = Env::new(&ws, &server, &quotes);
        env.mgr = Some(mgr.clone());
        let payload = serde_json::json!({
            "msg": "see [ ](http://127.0.0.1:9/group/g?msg=q1) *now*",
            "attachments": [{
                "title": "a.png",
                "title_link": "/file-upload/f1/hash#rustket-e2e-0123",
                "image_url": "/file-upload/f1/hash",
                "encryption": file_key.to_attachment(),
            }],
        });
        let mut m = mk(
            "d1",
            serde_json::json!({"t": "e2e", "msg": "", "md": [{"type": "PARAGRAPH", "value": []}],
                "content": enc(payload)}),
        );
        assert!(process(&mut m, &env));
        assert_eq!(state(&m), Some(State::Decrypted { plaintext: false }));
        assert!(m.msg.starts_with("see "));
        assert!(m.md.as_ref().is_some_and(|md| !md.is_empty()));
        let atts = m.attachments.as_ref().unwrap();
        // The file URLs are marked for this message (a payload marker is replaced).
        assert!(atts[0].image_url.as_deref().is_some_and(files::is_marked));
        assert!(files::Decryptor::for_url(atts[0].image_url.as_deref().unwrap()).is_some());
        assert_ne!(
            atts[0].title_link.as_deref(),
            Some("http://127.0.0.1:9/file-upload/f1/hash#rustket-e2e-0123")
        );
        // The quote is decrypted and attached; the message is done.
        let quote = atts.iter().find(|a| a.message_link.is_some()).unwrap();
        assert_eq!(quote.text.as_deref(), Some("the *original*"));
        assert!(!process(&mut m, &env));
        keys_changed();
        assert!(
            !process(&mut m, &env),
            "done messages are not processed again"
        );
        // The same version again from the server is decrypted again.
        let mut copy = mk(
            "d1",
            serde_json::json!({"t": "e2e", "content": m.content.clone()}),
        );
        assert!(process(&mut copy, &env));
        assert!(is_decrypted(&copy));

        // Plaintext next to the content (§11.14) is flagged.
        let mut p = mk(
            "d2",
            serde_json::json!({"t": "e2e", "msg": "server plaintext",
                "content": enc(serde_json::json!({"msg": "secret"}))}),
        );
        process(&mut p, &env);
        assert_eq!(p.msg, "secret");
        assert_eq!(state(&p), Some(State::Decrypted { plaintext: true }));
    }

    #[tokio::test]
    async fn unresolved_quotes_are_wanted_and_retried() {
        let (mgr, key, server) = manager_with_key("r-q").await;
        let mut ws = Workspace::new();
        ws.set_e2e(Some(mgr));
        let content = key
            .encrypt_json(
                &serde_json::json!({"msg": "[ ](http://127.0.0.1:9/group/g?msg=missing) hi"}),
            )
            .unwrap()
            .to_json();
        let mut m = msg("w1", serde_json::json!({"t": "e2e", "content": content}));
        m.rid = "r-q".into();
        let map = HashMap::new();
        let quotes = MapQuotes {
            map: &map,
            wants: Default::default(),
        };
        let env = Env::new(&ws, &server, &quotes);
        process(&mut m, &env);
        assert!(is_decrypted(&m));
        assert_eq!(quotes.wants.borrow().as_slice(), ["missing".to_owned()]);
        // Not done: processed again once the quote arrives.
        let mut q = msg("missing", serde_json::json!({"msg": "plain quoted"}));
        q.rid = "r-q".into();
        let map: HashMap<String, Message> = [("missing".to_owned(), q)].into();
        let quotes = MapQuotes {
            map: &map,
            wants: Default::default(),
        };
        let env = Env::new(&ws, &server, &quotes);
        keys_changed();
        assert!(process(&mut m, &env));
        assert!(
            m.attachments
                .iter()
                .flatten()
                .any(|a| a.text.as_deref() == Some("plain quoted"))
        );
        assert!(quotes.wants.borrow().is_empty());
        // Locally added quotes are not server plaintext.
        assert_eq!(state(&m), Some(State::Decrypted { plaintext: false }));
    }

    #[tokio::test]
    async fn copies_with_different_versions_stay_decrypted() {
        let (mgr, key, server) = manager_with_key("r-ver").await;
        let mut ws = Workspace::new();
        ws.set_e2e(Some(mgr));
        let env = Env::new(&ws, &server, &NoQuotes);
        let content = key
            .encrypt_json(&serde_json::json!({"msg": "secret text"}))
            .unwrap()
            .to_json();
        let raw = serde_json::json!({"t": "e2e", "content": content});
        // The room's last message (older version) and the timeline copy (a
        // reaction bumped `_updatedAt`).
        let mut last = msg("v1", raw.clone());
        last.rid = "r-ver".into();
        let mut timeline = last.clone();
        timeline.updated_at = rocketchat::RcDate::from_millis(2_000);
        for _ in 0..2 {
            process(&mut timeline, &env);
            process(&mut last, &env);
            assert_eq!(timeline.msg, "secret text");
            assert_eq!(
                state(&timeline),
                Some(State::Decrypted { plaintext: false })
            );
            assert_eq!(state(&last), Some(State::Decrypted { plaintext: false }));
            assert!(is_decrypted(&timeline) && is_decrypted(&last));
            keys_changed();
        }
        // A version never processed here is not believed.
        let mut forged = timeline.clone();
        forged.updated_at = rocketchat::RcDate::from_millis(3_000);
        assert_eq!(state(&forged), Some(State::Raw));
    }

    #[tokio::test]
    async fn many_concurrent_versions_stay_decrypted() {
        let (mgr, key, server) = manager_with_key("r-many").await;
        let mut ws = Workspace::new();
        ws.set_e2e(Some(mgr));
        let env = Env::new(&ws, &server, &NoQuotes);
        let content = key
            .encrypt_json(&serde_json::json!({"msg": "secret text"}))
            .unwrap()
            .to_json();
        let raw = serde_json::json!({"t": "e2e", "content": content});
        // Timeline, last message, thread view, pinned and starred panels,
        // a fetched quote: six versions of one message shown at once.
        let mut copies: Vec<Message> = (0..6)
            .map(|i| {
                let mut c = msg("many", raw.clone());
                c.rid = "r-many".into();
                c.updated_at = rocketchat::RcDate::from_millis(1_000 + i);
                c
            })
            .collect();
        for _ in 0..3 {
            for c in &mut copies {
                process(c, &env);
            }
            for c in &copies {
                assert_eq!(c.msg, "secret text");
                assert!(is_decrypted(c));
                assert_eq!(state(c), Some(State::Decrypted { plaintext: false }));
            }
            keys_changed();
        }
    }

    #[tokio::test]
    async fn server_plaintext_next_to_the_envelope_is_not_shown() {
        let (mgr, key, server) = manager_with_key("r-extra").await;
        let mut ws = Workspace::new();
        ws.set_e2e(Some(mgr));
        let env = Env::new(&ws, &server, &NoQuotes);
        let enc = |v: Value| key.encrypt_json(&v).unwrap().to_json();
        let mk = |id: &str, v: Value| {
            let mut m = msg(id, v);
            m.rid = "r-extra".into();
            m
        };
        // A forged attachment, UIKit block and URL preview next to a text.
        let mut m = mk(
            "x1",
            serde_json::json!({"t": "e2e", "content": enc(serde_json::json!({"msg": "hello"})),
                "attachments": [{"text": "Transfer the money to X"}],
                "blocks": [{"type": "section", "text": {"type": "mrkdwn", "text": "spoofed"}}],
                "urls": [{"url": "https://evil.example/"}]}),
        );
        process(&mut m, &env);
        assert_eq!(m.msg, "hello");
        assert!(m.attachments.is_none());
        assert!(m.blocks.is_none());
        assert!(m.urls.is_none());
        assert_eq!(state(&m), Some(State::Decrypted { plaintext: true }));
        // Processed again (new keys): still flagged, nothing comes back.
        keys_changed();
        process(&mut m, &env);
        assert!(m.attachments.is_none() && m.blocks.is_none());
        assert_eq!(state(&m), Some(State::Decrypted { plaintext: true }));

        // A deleted file's placeholder stays.
        let mut r = mk(
            "x2",
            serde_json::json!({"t": "e2e", "content": enc(serde_json::json!({"msg": "a file"})),
                "attachments": [{"type": "removed-file", "title": "File removed"}]}),
        );
        process(&mut r, &env);
        assert_eq!(r.attachments.as_ref().map(Vec::len), Some(1));
        assert_eq!(state(&r), Some(State::Decrypted { plaintext: false }));

        // Payload attachments replace the server's (encrypted upload).
        let mut u = mk(
            "x3",
            serde_json::json!({"t": "e2e",
                "content": enc(serde_json::json!({"msg": "", "attachments": [{"title": "real.png"}]})),
                "attachments": [{"title": "0a1b2c", "type": "file"}]}),
        );
        process(&mut u, &env);
        let atts = u.attachments.as_ref().unwrap();
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].title.as_deref(), Some("real.png"));
        assert_eq!(state(&u), Some(State::Decrypted { plaintext: false }));

        // No envelope at all (§11.13): the server attachment is all there
        // is; shown, but marked.
        let mut c = mk(
            "x4",
            serde_json::json!({"t": "e2e", "msg": "", "attachments": [{"title": "0a1b2c", "type": "file"}]}),
        );
        process(&mut c, &env);
        assert_eq!(c.attachments.as_ref().map(Vec::len), Some(1));
        assert_eq!(state(&c), Some(State::Decrypted { plaintext: true }));
    }

    #[test]
    fn legacy_ciphertext_moves_to_content() {
        let server = ServerUrl::parse("http://localhost:3000").unwrap();
        let ws = Workspace::new();
        let env = Env::new(&ws, &server, &NoQuotes);
        let mut m = msg(
            "legacy1",
            serde_json::json!({"t": "e2e", "msg": "32c9e7917b78LHjHfqLMeDn+2UK1PhD/soFe8CVwvFdLkslcfxNHby4="}),
        );
        process(&mut m, &env);
        assert!(m.msg.is_empty());
        assert_eq!(
            m.content.as_ref().and_then(|c| c["algorithm"].as_str()),
            Some("rc.v1.aes-sha2")
        );
    }

    #[test]
    fn markers_on_plain_messages_are_dropped() {
        let server = ServerUrl::parse("http://localhost:3000").unwrap();
        let ws = Workspace::new();
        let env = Env::new(&ws, &server, &NoQuotes);
        let mut m = msg(
            "plain1",
            serde_json::json!({"msg": "hi", "e2e": "rustket:ok"}),
        );
        assert!(process(&mut m, &env));
        assert_eq!(m.e2e, None);
        assert_eq!(state(&m), None);
    }

    #[test]
    fn permalinks() {
        let server = ServerUrl::parse("http://localhost:3000").unwrap();
        let ids = quoted_ids(
            "[ ](http://localhost:3000/group/g?msg=abc) see https://other.org/x?msg=zzz and [ ](http://localhost:3000/direct/x?msg=abc)",
            "http://localhost:3000",
            &server,
        );
        assert_eq!(
            ids,
            vec![(
                "http://localhost:3000/group/g?msg=abc".to_owned(),
                "abc".to_owned()
            )]
        );
    }
}
