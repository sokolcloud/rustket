//! Decrypting messages in place (§7.7, `rustket_core::e2e::shown`).
//!
//! Every `t: 'e2e'` message the app shows passes through [`process`] before it
//! is drawn: the decrypted `msg` (with an `md` tree parsed here), attachments
//! and files replace the server values, `content` keeps the envelope (a legacy
//! v1 ciphertext in `msg` is moved there), and `Message::e2e` gets a marker.
//! What was processed is recorded per `(rid, _id)` with `_updatedAt`, so a
//! marker the server sent is never believed and an edit is decrypted again.
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
}

fn seen() -> &'static Mutex<HashMap<(String, String), Seen>> {
    static S: OnceLock<Mutex<HashMap<(String, String), Seen>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn seen_lock() -> std::sync::MutexGuard<'static, HashMap<(String, String), Seen>> {
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

/// The current epoch (see [`keys_changed`]).
pub fn current_epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}

fn updated_ms(m: &Message) -> Option<i64> {
    m.updated_at
        .as_ref()
        .map(rocketchat::RcDate::timestamp_millis)
}

/// The marker of `m` if this app wrote it for exactly this version.
pub fn verified_marker(m: &Message) -> Option<&'static str> {
    let marker = mark::marker(m)?;
    let s = *seen_lock().get(&(m.rid.clone(), m.id.clone()))?;
    (s.updated == updated_ms(m) && s.marker == marker).then_some(s.marker)
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
            if mark::marker(m).is_some() {
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
    if mark::marker(m).is_some() {
        raw.msg.clear();
        raw.md = None;
        raw.e2e = None;
    }
    raw
}

fn is_decrypted_marker(marker: &str) -> bool {
    matches!(marker, mark::DECRYPTED | mark::DECRYPTED_PLAINTEXT)
}

fn record(m: &Message, seen: Seen) {
    let mut table = seen_lock();
    let key = (m.rid.clone(), m.id.clone());
    // Another copy of this version (a panel list, the thread view) is shown
    // decrypted: keep that record, so its plaintext is never mistaken for a
    // server message. Copies that are not decrypted hold no plaintext.
    if let Some(old) = table.get(&key)
        && old.updated == seen.updated
        && is_decrypted_marker(old.marker)
        && !is_decrypted_marker(seen.marker)
    {
        return;
    }
    table.insert(key, seen);
}

fn process_e2e(m: &mut Message, env: &Env<'_>, with_quotes: bool) -> bool {
    let epoch = EPOCH.load(Ordering::SeqCst);
    let updated = updated_ms(m);
    let prior = seen_lock()
        .get(&(m.rid.clone(), m.id.clone()))
        .copied()
        .filter(|s| s.updated == updated && Some(s.marker) == mark::marker(m));
    let (result, plain) = match prior {
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
            (r, plain)
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
                for a in &mut atts {
                    files::mark_attachment(a, env.server);
                }
                m.attachments = Some(atts);
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
    m.e2e = Some(marker.to_owned());
    record(
        m,
        Seen {
            updated,
            marker,
            plain,
            epoch,
            done,
        },
    );
    true
}

/// `message_pinned_e2e`: the pinned message's text is decrypted into the
/// first attachment (§7.7).
fn process_pinned(m: &mut Message, env: &Env<'_>) -> bool {
    let epoch = EPOCH.load(Ordering::SeqCst);
    let updated = updated_ms(m);
    let prior = seen_lock()
        .get(&(m.rid.clone(), m.id.clone()))
        .copied()
        .filter(|s| s.updated == updated && Some(s.marker) == mark::marker(m));
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
    m.e2e = Some(marker.to_owned());
    record(
        m,
        Seen {
            updated,
            marker,
            plain: false,
            epoch,
            done,
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

/// Appends quote attachments for the permalinks in `m.msg` (the server
/// cannot build them for encrypted text, §7.7). Returns whether every quote
/// was resolved.
fn add_quotes(m: &mut Message, env: &Env<'_>) -> bool {
    let links = quoted_ids(&m.msg, &env.site_url, env.server);
    if links.is_empty() {
        return true;
    }
    let mut all = true;
    for (url, id) in links {
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
mod tests {
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
