//! Who reacted to a message: reactions grouped by emoji and by user, with
//! the real names the client knows.

use rocketchat::models::Message;

use crate::presence::Presence;

/// One user who reacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reactor {
    pub username: String,
    /// Real name, when known.
    pub name: Option<String>,
}

/// The users who reacted with one emoji.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionGroup {
    /// Shortcode as stored (`:thumbsup:`).
    pub emoji: String,
    pub users: Vec<Reactor>,
}

/// Non-empty reactions of `m`, by count (descending), then emoji key. Names
/// come from `Reaction.names` when it is aligned with the usernames (REST
/// results with `UI_Use_Real_Name`), else from `presence`.
pub fn groups(m: &Message, presence: &Presence) -> Vec<ReactionGroup> {
    let Some(map) = &m.reactions else {
        return Vec::new();
    };
    let mut out: Vec<ReactionGroup> = map
        .iter()
        .filter(|(_, r)| !r.usernames.is_empty())
        .map(|(emoji, r)| {
            let aligned = r.names.as_ref().filter(|n| n.len() == r.usernames.len());
            let users = r
                .usernames
                .iter()
                .enumerate()
                .map(|(i, u)| Reactor {
                    username: u.clone(),
                    name: aligned
                        .and_then(|n| n.get(i))
                        .filter(|n| !n.is_empty())
                        .cloned()
                        .or_else(|| presence.get_by_username(u).and_then(|e| e.name.clone())),
                })
                .collect();
            ReactionGroup {
                emoji: emoji.clone(),
                users,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.users
            .len()
            .cmp(&a.users.len())
            .then_with(|| a.emoji.cmp(&b.emoji))
    });
    out
}

/// Each reactor once, with the emojis they used, in first-seen order.
pub fn by_user(groups: &[ReactionGroup]) -> Vec<(Reactor, Vec<String>)> {
    let mut out: Vec<(Reactor, Vec<String>)> = Vec::new();
    for g in groups {
        for u in &g.users {
            match out.iter_mut().find(|(r, _)| r.username == u.username) {
                Some((r, emojis)) => {
                    if r.name.is_none() {
                        r.name.clone_from(&u.name);
                    }
                    if !emojis.contains(&g.emoji) {
                        emojis.push(g.emoji.clone());
                    }
                }
                None => out.push((u.clone(), vec![g.emoji.clone()])),
            }
        }
    }
    out
}

/// Total number of reactions (one per user and emoji).
pub fn total(groups: &[ReactionGroup]) -> usize {
    groups.iter().map(|g| g.users.len()).sum()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::{UserStatus, UserSummary};

    fn msg(json: serde_json::Value) -> Message {
        serde_json::from_value(
            serde_json::json!({"_id": "m", "rid": "r", "msg": "", "reactions": json}),
        )
        .unwrap()
    }

    fn presence() -> Presence {
        let mut p = Presence::new();
        p.update_from_summary(&UserSummary {
            id: "u-carol".into(),
            username: Some("carol".into()),
            name: Some("Carol C".into()),
            status: Some(UserStatus::Online),
            ..UserSummary::default()
        });
        p
    }

    #[test]
    fn groups_sorted_and_named() {
        let m = msg(serde_json::json!({
            ":tada:": {"usernames": ["carol"]},
            ":thumbsup:": {"usernames": ["alice", "bob"], "names": ["Alice A", "Bob B"]},
            ":eyes:": {"usernames": ["bob"]},
            ":empty:": {"usernames": []},
            // Misaligned names fall back to presence, then nothing.
            ":zzz:": {"usernames": ["carol", "dave"], "names": ["only one"]},
        }));
        let g = groups(&m, &presence());
        let keys: Vec<&str> = g.iter().map(|g| g.emoji.as_str()).collect();
        assert_eq!(keys, [":thumbsup:", ":zzz:", ":eyes:", ":tada:"]);
        assert_eq!(g[0].users[1].name.as_deref(), Some("Bob B"));
        assert_eq!(g[1].users[0].name.as_deref(), Some("Carol C"));
        assert_eq!(g[1].users[1].name, None);
        assert_eq!(total(&g), 6);
        assert!(groups(&Message::default(), &presence()).is_empty());
    }

    #[test]
    fn by_user_dedupes_in_first_seen_order() {
        let m = msg(serde_json::json!({
            ":thumbsup:": {"usernames": ["alice", "bob"]},
            ":eyes:": {"usernames": ["bob"]},
            ":tada:": {"usernames": ["carol", "alice"]},
        }));
        let u = by_user(&groups(&m, &presence()));
        let rows: Vec<(&str, Vec<&str>)> = u
            .iter()
            .map(|(r, e)| (r.username.as_str(), e.iter().map(String::as_str).collect()))
            .collect();
        assert_eq!(
            rows,
            [
                ("carol", vec![":tada:"]),
                ("alice", vec![":tada:", ":thumbsup:"]),
                ("bob", vec![":thumbsup:", ":eyes:"]),
            ]
        );
    }
}
