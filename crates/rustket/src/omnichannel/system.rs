//! Texts of the omnichannel system messages (`docs/omnichannel.md` §18, from
//! `packages/message-types/src/registrations/{livechat,omnichannel}.ts`).
//! `command` messages (`connected`, `promptTranscript`, `survey`) are never
//! shown, like in the web client.

use rocketchat::models::{Message, TransferScope};
use rust_i18n::t;
use rustket_ui::widget::sanitize_bidi;
use serde_json::Value;

/// Whether `m` is never shown in a message list (`t == "command"`).
pub fn hidden(m: &Message) -> bool {
    m.t.as_deref() == Some("command")
}

fn clean(s: &str) -> String {
    sanitize_bidi(s).into_owned()
}

fn str_at<'a>(v: Option<&'a Value>, path: &[&str]) -> Option<&'a str> {
    let mut cur = v?;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str().filter(|s| !s.trim().is_empty())
}

/// `name || username` of a `{name, username}` object.
fn person(v: Option<&Value>) -> String {
    str_at(v, &["name"])
        .or_else(|| str_at(v, &["username"]))
        .map(clean)
        .unwrap_or_default()
}

fn transfer_text(m: &Message, loc: &str) -> String {
    let Some(td) = m.transfer_data.as_ref() else {
        return t!("omnichannel.sys.transfer", locale = loc, transfer = "").into_owned();
    };
    let comment = td
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(clean);
    let from = td
        .transferred_by
        .as_ref()
        .map(|a| clean(a.display_name()))
        .unwrap_or_default();
    let to_agent = td
        .transferred_to
        .as_ref()
        .map(|a| clean(a.display_name()))
        .unwrap_or_default();
    let to_dept = td
        .next_department
        .as_ref()
        .map(|d| clean(&d.name))
        .unwrap_or_default();
    let inner = match (td.scope, &comment) {
        (TransferScope::Agent, None) => t!(
            "omnichannel.sys.to_agent",
            locale = loc,
            from = from,
            to = to_agent
        ),
        (TransferScope::Agent, Some(c)) => t!(
            "omnichannel.sys.to_agent_comment",
            locale = loc,
            from = from,
            to = to_agent,
            comment = c
        ),
        (TransferScope::Department, None) => t!(
            "omnichannel.sys.to_department",
            locale = loc,
            from = from,
            to = to_dept
        ),
        (TransferScope::Department, Some(c)) => t!(
            "omnichannel.sys.to_department_comment",
            locale = loc,
            from = from,
            to = to_dept,
            comment = c
        ),
        (TransferScope::Queue, None) => t!("omnichannel.sys.to_queue", locale = loc, from = from),
        (TransferScope::Queue, Some(c)) => t!(
            "omnichannel.sys.to_queue_comment",
            locale = loc,
            from = from,
            comment = c
        ),
        (TransferScope::AutoTransferUnansweredChatsToAgent, c) => t!(
            "omnichannel.sys.auto_to_agent",
            locale = loc,
            from = from,
            to = to_agent,
            duration = c.clone().unwrap_or_default()
        ),
        (TransferScope::AutoTransferUnansweredChatsToQueue, c) => t!(
            "omnichannel.sys.auto_to_queue",
            locale = loc,
            from = from,
            duration = c.clone().unwrap_or_default()
        ),
        // The web renderer throws on unknown scopes; say something generic.
        (TransferScope::Unknown, _) => {
            t!("omnichannel.sys.transferred", locale = loc, from = from)
        }
    };
    t!("omnichannel.sys.transfer", locale = loc, transfer = inner).into_owned()
}

fn transcript_text(m: &Message, loc: &str) -> String {
    let rd = m.request_data.as_ref();
    let guest = person(rd.and_then(|r| r.get("visitor")));
    let inner = match str_at(rd, &["type"]) {
        None if rd.is_none() => String::new(),
        Some("visitor") => t!(
            "omnichannel.sys.transcript_visitor",
            locale = loc,
            guest = guest
        )
        .into_owned(),
        _ => t!(
            "omnichannel.sys.transcript_user",
            locale = loc,
            agent = person(rd.and_then(|r| r.get("user"))),
            guest = guest
        )
        .into_owned(),
    };
    t!(
        "omnichannel.sys.transcript",
        locale = loc,
        transcript = inner
    )
    .into_owned()
}

fn navigation_text(m: &Message, loc: &str) -> String {
    let page = m.navigation.as_ref().and_then(|n| n.get("page"));
    let href = str_at(page, &["location", "href"]).unwrap_or_default();
    let history = match str_at(page, &["title"]) {
        Some(title) => format!("{title} - {href}"),
        None => href.to_owned(),
    };
    t!(
        "omnichannel.sys.navigation",
        locale = loc,
        history = clean(&history)
    )
    .into_owned()
}

fn comment_or_none(m: &Message, loc: &str) -> String {
    m.comment
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(clean)
        .unwrap_or_else(|| t!("omnichannel.sys.no_comment", locale = loc).into_owned())
}

/// The text of an omnichannel system message in locale `loc`; `None` for
/// other message types.
pub fn text(m: &Message, loc: &str) -> Option<String> {
    let s = match m.t.as_deref()? {
        "livechat-started" => t!("omnichannel.sys.started", locale = loc).into_owned(),
        "livechat-close" => {
            let comment = m.msg.trim();
            if comment.is_empty() {
                t!("omnichannel.sys.closed", locale = loc).into_owned()
            } else {
                t!(
                    "omnichannel.sys.closed_comment",
                    locale = loc,
                    comment = clean(comment)
                )
                .into_owned()
            }
        }
        "livechat_video_call" => t!("omnichannel.sys.video_call", locale = loc).into_owned(),
        "livechat_navigation_history" => navigation_text(m, loc),
        "livechat_transfer_history" => transfer_text(m, loc),
        "livechat_transfer_history_fallback" => {
            let td = m.transfer_data.as_ref();
            // The server writes `prevDepartment` (a name) and `department`.
            let from = td
                .and_then(|t| t.prev_department.as_deref())
                .or_else(|| {
                    td.and_then(|t| t.previous_department.as_ref())
                        .and_then(|v| v.as_str().or_else(|| str_at(Some(v), &["name"])))
                })
                .map(clean)
                .unwrap_or_default();
            let to = td
                .and_then(|t| t.department.as_ref().or(t.next_department.as_ref()))
                .map(|d| clean(&d.name))
                .unwrap_or_default();
            let fallback = t!(
                "omnichannel.sys.fallback_reason",
                locale = loc,
                from = from,
                to = to
            );
            t!(
                "omnichannel.sys.fallback",
                locale = loc,
                fallback = fallback
            )
            .into_owned()
        }
        "livechat_transcript_history" => transcript_text(m, loc),
        "omnichannel_placed_chat_on_hold" => t!(
            "omnichannel.sys.on_hold",
            locale = loc,
            comment = comment_or_none(m, loc)
        )
        .into_owned(),
        "omnichannel_on_hold_chat_resumed" => t!(
            "omnichannel.sys.resumed",
            locale = loc,
            comment = comment_or_none(m, loc)
        )
        .into_owned(),
        "omnichannel_priority_change_history" => {
            let pd = m.priority_data.as_ref();
            let user = str_at(pd, &["definedBy", "username"])
                .map(clean)
                .unwrap_or_else(|| t!("omnichannel.sys.unknown_user", locale = loc).into_owned());
            let priority = str_at(pd, &["priority", "name"])
                .or_else(|| str_at(pd, &["priority", "i18n"]))
                .map(clean)
                .unwrap_or_else(|| t!("omnichannel.sys.unprioritized", locale = loc).into_owned());
            t!(
                "omnichannel.sys.priority",
                locale = loc,
                user = user,
                priority = priority
            )
            .into_owned()
        }
        "omnichannel_sla_change_history" => {
            let sd = m.sla_data.as_ref();
            let user = str_at(sd, &["definedBy", "username"])
                .map(clean)
                .unwrap_or_else(|| t!("omnichannel.sys.unknown_user", locale = loc).into_owned());
            let sla = str_at(sd, &["sla", "name"])
                .map(clean)
                .unwrap_or_else(|| t!("omnichannel.sys.without_sla", locale = loc).into_owned());
            t!("omnichannel.sys.sla", locale = loc, user = user, sla = sla).into_owned()
        }
        _ => return None,
    };
    Some(s)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(v: Value) -> Message {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn transfer_texts_follow_the_web() {
        let m = msg(
            json!({"_id":"m","rid":"r","t":"livechat_transfer_history","msg":"",
            "u":{"_id":"a","username":"agent"},
            "transferData":{"scope":"agent","transferredBy":{"_id":"a","username":"alice","name":"Alice"},
                "transferredTo":{"_id":"b","username":"bob"},"comment":"busy"}}),
        );
        assert_eq!(
            text(&m, "en").unwrap(),
            "New Chat Transfer: Alice transferred the chat to bob with a comment: busy"
        );
        let m = msg(
            json!({"_id":"m","rid":"r","t":"livechat_transfer_history","msg":"",
            "u":{"_id":"a"},
            "transferData":{"scope":"queue","transferredBy":{"_id":"a","username":"alice"},"comment":null}}),
        );
        assert_eq!(
            text(&m, "en").unwrap(),
            "New Chat Transfer: alice returned the chat to the queue"
        );
        let m = msg(
            json!({"_id":"m","rid":"r","t":"livechat_transfer_history","msg":"",
            "u":{"_id":"a"},
            "transferData":{"scope":"department","transferredBy":{"_id":"a","username":"alice"},
                "nextDepartment":{"_id":"d","name":"Sales"}}}),
        );
        assert_eq!(
            text(&m, "ru").unwrap(),
            "Новый чат-трансфер: alice перенаправил чат в департамент Sales"
        );
        // Unknown scopes do not break.
        let m = msg(
            json!({"_id":"m","rid":"r","t":"livechat_transfer_history","msg":"",
            "u":{"_id":"a"},"transferData":{"scope":"teleport","transferredBy":{"_id":"a","username":"x"}}}),
        );
        assert!(text(&m, "en").unwrap().contains('x'));
        // EE fallback forward, as saved by the server.
        let m = msg(
            json!({"_id":"m","rid":"r","t":"livechat_transfer_history_fallback","msg":"",
            "u":{"_id":"a"},
            "transferData":{"scope":"department","transferredBy":{"_id":"a","username":"alice"},
                "prevDepartment":"Sales","department":{"_id":"d2","name":"Support"},
                "usingFallbackDep":true}}),
        );
        let s = text(&m, "en").unwrap();
        assert!(s.contains("( Sales )") && s.ends_with("Support"), "{s}");
    }

    #[test]
    fn other_types() {
        let close =
            msg(json!({"_id":"m","rid":"r","t":"livechat-close","msg":"Resolved","u":{"_id":"a"}}));
        assert_eq!(
            text(&close, "en").unwrap(),
            "Conversation closed: Resolved."
        );
        let hold = msg(
            json!({"_id":"m","rid":"r","t":"omnichannel_placed_chat_on_hold","msg":"","u":{"_id":"a"}}),
        );
        assert_eq!(
            text(&hold, "en").unwrap(),
            "Chat On Hold: No comment provided"
        );
        let tr = msg(
            json!({"_id":"m","rid":"r","t":"livechat_transcript_history","msg":"","u":{"_id":"a"},
            "requestData":{"type":"user","user":{"username":"alice"},"visitor":{"name":"Guest"}}}),
        );
        assert_eq!(
            text(&tr, "en").unwrap(),
            "Chat transcript sent: alice sent the chat transcript to Guest"
        );
        let nav = msg(
            json!({"_id":"m","rid":"r","t":"livechat_navigation_history","msg":"","u":{"_id":"a"},
            "navigation":{"page":{"title":"Pricing","location":{"href":"https://x.test/p"}}}}),
        );
        assert_eq!(
            text(&nav, "en").unwrap(),
            "New Navigation: Pricing - https://x.test/p"
        );
        let plain = msg(json!({"_id":"m","rid":"r","t":"uj","msg":"","u":{"_id":"a"}}));
        assert!(text(&plain, "en").is_none());
        let cmd = msg(json!({"_id":"m","rid":"r","t":"command","msg":"connected","u":{"_id":"a"}}));
        assert!(hidden(&cmd) && !hidden(&plain));
    }
}
