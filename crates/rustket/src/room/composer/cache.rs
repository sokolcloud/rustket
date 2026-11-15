//! Process-wide caches of the composer: room members and slash commands for
//! autocomplete, and the outbox registry of running sends.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use rocketchat::models::{SlashCommand, UserSummary};
use rustket_core::ServerId;

use super::MEMBERS_TTL;
use crate::error::Res;

/// At most this many rooms keep cached members.
const MEMBERS_MAX_ROOMS: usize = 64;
/// `commands.list` is refetched after this long.
const COMMANDS_TTL: Duration = Duration::from_secs(600);
/// After a failed `commands.list`, wait this long before asking again.
const COMMANDS_RETRY: Duration = Duration::from_secs(15);

type MemberCache = HashMap<(String, String), (Instant, Arc<Vec<UserSummary>>)>;

/// Room members per (server, room) for `@` completion.
static MEMBERS: LazyLock<Mutex<MemberCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Default)]
struct CommandCache {
    lists: HashMap<String, (Instant, Arc<Vec<SlashCommand>>)>,
    failed: HashMap<String, Instant>,
}

/// `commands.list` per server.
static COMMANDS: LazyLock<Mutex<CommandCache>> =
    LazyLock::new(|| Mutex::new(CommandCache::default()));

pub(super) fn cached_members(server: &ServerId, rid: &str) -> Option<Arc<Vec<UserSummary>>> {
    let key = (server.as_str().to_owned(), rid.to_owned());
    let map = MEMBERS.lock().ok()?;
    map.get(&key)
        .filter(|(at, _)| at.elapsed() < MEMBERS_TTL)
        .map(|(_, m)| m.clone())
}

pub(super) fn store_members(server: &ServerId, rid: &str, list: Vec<UserSummary>) {
    if let Ok(mut map) = MEMBERS.lock() {
        map.retain(|_, (at, _)| at.elapsed() < MEMBERS_TTL);
        if map.len() >= MEMBERS_MAX_ROOMS
            && let Some(oldest) = map
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(
            (server.as_str().to_owned(), rid.to_owned()),
            (Instant::now(), Arc::new(list)),
        );
    }
}

pub(super) fn cached_commands(server: &ServerId) -> Option<Arc<Vec<SlashCommand>>> {
    COMMANDS
        .lock()
        .ok()?
        .lists
        .get(server.as_str())
        .filter(|(at, _)| at.elapsed() < COMMANDS_TTL)
        .map(|(_, l)| l.clone())
}

/// Caches a non-empty list. The server answers an empty list while its
/// Apps-Engine is still loading: that counts as a failure.
pub(super) fn store_commands(server: &ServerId, list: Vec<SlashCommand>) -> bool {
    let Ok(mut c) = COMMANDS.lock() else {
        return false;
    };
    if list.is_empty() {
        c.failed.insert(server.as_str().to_owned(), Instant::now());
        return false;
    }
    c.failed.remove(server.as_str());
    c.lists
        .insert(server.as_str().to_owned(), (Instant::now(), Arc::new(list)));
    true
}

pub(super) fn commands_failed(server: &ServerId) {
    if let Ok(mut c) = COMMANDS.lock() {
        c.failed.insert(server.as_str().to_owned(), Instant::now());
    }
}

/// Drops the cached `commands.list` of `server` (apps added or removed
/// commands, `crate::uikit`).
pub fn forget_commands(server: &ServerId) {
    if let Ok(mut c) = COMMANDS.lock() {
        c.lists.remove(server.as_str());
        c.failed.remove(server.as_str());
    }
}

/// `commands.list` failed a moment ago: do not ask again yet.
pub(super) fn commands_recently_failed(server: &ServerId) -> bool {
    COMMANDS.lock().ok().is_some_and(|c| {
        c.failed
            .get(server.as_str())
            .is_some_and(|at| at.elapsed() < COMMANDS_RETRY)
    })
}

/// Outbox sends by (server, message id). Results are recorded here as soon as
/// the request finishes, so a result that arrives while the room is closed
/// (its message is then dropped) is applied when the room opens again.
enum Outcome {
    InFlight,
    Done(Res<Box<rocketchat::models::Message>>),
}

struct OutboxEntry {
    rid: String,
    outcome: Outcome,
}

static OUTBOX: LazyLock<Mutex<HashMap<(String, String), OutboxEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn outbox_start(server: &ServerId, rid: &str, id: &str) {
    if let Ok(mut map) = OUTBOX.lock() {
        map.insert(
            (server.as_str().to_owned(), id.to_owned()),
            OutboxEntry {
                rid: rid.to_owned(),
                outcome: Outcome::InFlight,
            },
        );
    }
}

pub(super) fn outbox_finish(server: &str, id: &str, r: &Res<Box<rocketchat::models::Message>>) {
    if let Ok(mut map) = OUTBOX.lock()
        && let Some(e) = map.get_mut(&(server.to_owned(), id.to_owned()))
    {
        e.outcome = Outcome::Done(r.clone());
    }
}

pub(crate) fn outbox_forget(server: &ServerId, id: &str) {
    if let Ok(mut map) = OUTBOX.lock() {
        map.remove(&(server.as_str().to_owned(), id.to_owned()));
    }
}

pub(super) fn outbox_in_flight(server: &ServerId, id: &str) -> bool {
    OUTBOX.lock().ok().is_some_and(|m| {
        m.get(&(server.as_str().to_owned(), id.to_owned()))
            .is_some_and(|e| matches!(e.outcome, Outcome::InFlight))
    })
}

/// Takes the finished sends of room `rid`.
pub(super) fn outbox_take_done(
    server: &ServerId,
    rid: &str,
) -> Vec<(String, Res<Box<rocketchat::models::Message>>)> {
    let Ok(mut map) = OUTBOX.lock() else {
        return Vec::new();
    };
    let keys: Vec<(String, String)> = map
        .iter()
        .filter(|((s, _), e)| {
            s == server.as_str() && e.rid == rid && matches!(e.outcome, Outcome::Done(_))
        })
        .map(|(k, _)| k.clone())
        .collect();
    keys.into_iter()
        .filter_map(|k| match map.remove(&k)?.outcome {
            Outcome::Done(r) => Some((k.1, r)),
            Outcome::InFlight => None,
        })
        .collect()
}
