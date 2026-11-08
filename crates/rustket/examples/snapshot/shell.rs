//! App shell states: the rail marker of a signed-out server and the
//! collapsed rows of an ignored user's messages (local state only: a fake
//! second server and a local message; nothing is changed on the server).

use std::path::Path;

use rocketchat::RcDate;
use rocketchat::models::{Message as RcMessage, UserRef};
use rustket_core::config::{ServerEntry, Theme};
use rustket_core::{Change, ServerId};

use super::Driver;

fn both(d: &mut Driver, out: &Path, name: &str) {
    d.app.config.ui.theme = Theme::Light;
    d.shoot(out, &format!("{name}-light"));
    d.app.config.ui.theme = Theme::Dark;
    d.shoot(out, &format!("{name}-dark"));
}

pub async fn shell_shots(d: &mut Driver, out: &Path, sid: &ServerId, rid: &str) {
    // An ignored user's messages in the open room.
    let now = chrono::Utc::now().timestamp_millis();
    let ws = &mut d.app.servers.get_mut(sid).expect("server").workspace;
    let before_sub = ws.subscription(rid).cloned().expect("subscription");
    for (i, text) in ["Buy cheap followers!!!", "Still here, still spamming"]
        .iter()
        .enumerate()
    {
        ws.apply_message(RcMessage {
            id: format!("rk-snap-ignored-{i}"),
            rid: rid.to_owned(),
            msg: (*text).to_owned(),
            ts: RcDate::from_millis(now + i as i64),
            u: UserRef {
                id: "rk-snap-troll".into(),
                username: Some("troll".into()),
                name: Some("Troll".into()),
            },
            ..Default::default()
        });
    }
    both(d, out, "shell-ignored-before");
    let ws = &mut d.app.servers.get_mut(sid).expect("server").workspace;
    let mut sub = before_sub.clone();
    sub.ignored = Some(vec!["rk-snap-troll".into()]);
    ws.apply_subscription(Change::Updated, sub);
    both(d, out, "shell-ignored-after");
    let ws = &mut d.app.servers.get_mut(sid).expect("server").workspace;
    ws.apply_subscription(Change::Updated, before_sub);
    for i in 0..2 {
        ws.apply_message_deleted(rid, &format!("rk-snap-ignored-{i}"));
    }

    // A second, signed-out server in the rail.
    let entry = ServerEntry::new("http://localhost:3999", "Signed out workspace");
    let other = d.app.config.add_server(entry.clone());
    let mut state = rustket::workspace::ServerState::from_entry(&entry).expect("state");
    state.restoring = false;
    d.app.servers.insert(other.clone(), state);
    both(d, out, "shell-rail-signed-out");
    d.app.servers.remove(&other);
    d.app.config.remove_server(&other);
}
