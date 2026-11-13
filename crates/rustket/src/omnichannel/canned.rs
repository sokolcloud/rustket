//! The composer's `!` autocomplete in omnichannel rooms (`docs/omnichannel.md`
//! §16): canned responses whose shortcut contains the typed filter, sorted
//! by shortcut, at most 11 (`rustket_core::omnichannel::canned::suggest`).
//! Selecting one replaces `!filter` with the response text verbatim (the
//! server substitutes the `{{…}}` placeholders).

use iced::widget::{column, text};
use iced::{Element, Length};
use rocketchat::models::RoomType;
use rustket_core::{ServerId, Workspace};
use rustket_ui::style;
use rustket_ui::theme::{TEXT_CAPTION, TEXT_SMALL, font_semibold};

/// `(shortcut, text)` suggestions for `!query` in room `rid`; empty outside
/// omnichannel rooms or without the canned responses feature.
pub fn suggestions(
    server: &ServerId,
    ws: &Workspace,
    rid: &str,
    query: &str,
) -> Vec<(String, String)> {
    if ws.room(rid).map(|e| e.room_type()) != Some(RoomType::Livechat) {
        return Vec::new();
    }
    let Some(omni) = super::shared::get(server) else {
        return Vec::new();
    };
    if !omni.canned_enabled(ws) {
        return Vec::new();
    }
    omni.canned
        .suggest(query)
        .into_iter()
        .map(|r| (r.shortcut.clone(), r.text.clone()))
        .collect()
}

/// A popup row: **shortcut** and the text (one line).
pub fn item_view<'a, M: 'a>(shortcut: &'a str, body: &'a str) -> Element<'a, M> {
    let first = body.lines().next().unwrap_or_default();
    column![
        text(format!("!{shortcut}"))
            .size(TEXT_SMALL)
            .font(font_semibold()),
        text(first)
            .size(TEXT_CAPTION)
            .style(style::text_secondary)
            .width(Length::Fill)
            .wrapping(iced::widget::text::Wrapping::None),
    ]
    .spacing(0)
    .into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::{LicenseInfo, Permission};
    use rustket_core::Change;
    use rustket_core::omnichannel::OmnichannelState;
    use serde_json::json;

    #[test]
    fn only_in_livechat_rooms_with_the_feature() {
        let sid = ServerId("omni-canned-test".into());
        let mut ws = Workspace::new();
        ws.apply_me(
            serde_json::from_value(json!({"_id":"me","username":"me","roles":["livechat-agent"]}))
                .unwrap(),
        );
        ws.apply_room(
            Change::Inserted,
            serde_json::from_value(json!({"_id":"L","t":"l","open":true})).unwrap(),
        );
        ws.apply_room(
            Change::Inserted,
            serde_json::from_value(json!({"_id":"C","t":"c","name":"c"})).unwrap(),
        );
        let p: Permission = serde_json::from_value(
            json!({"_id":"view-canned-responses","roles":["livechat-agent"]}),
        )
        .unwrap();
        ws.apply_permission(Change::Inserted, p);
        let mut omni = OmnichannelState::new();
        omni.canned.replace(vec![
            serde_json::from_value(
                json!({"_id":"1","shortcut":"hello","text":"Hello!","scope":"global"}),
            )
            .unwrap(),
            serde_json::from_value(
                json!({"_id":"2","shortcut":"bye","text":"Bye","scope":"global"}),
            )
            .unwrap(),
        ]);
        super::super::shared::publish(&sid, &omni);
        assert!(suggestions(&sid, &ws, "L", "he").is_empty(), "no license");
        omni.set_license(Some(LicenseInfo {
            active_modules: vec![LicenseInfo::CANNED_RESPONSES.into()],
            ..Default::default()
        }));
        super::super::shared::publish(&sid, &omni);
        assert_eq!(
            suggestions(&sid, &ws, "L", "HE"),
            vec![("hello".to_owned(), "Hello!".to_owned())]
        );
        assert_eq!(suggestions(&sid, &ws, "L", "").len(), 2);
        assert!(suggestions(&sid, &ws, "C", "he").is_empty());
        super::super::shared::forget(&sid);
    }
}
