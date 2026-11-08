//! Canned responses (`docs/omnichannel.md` §16, EE): the responses the agent
//! may use, filtered by scope and text; "Use" inserts the text into the
//! composer. Without the license module the panel explains why it is empty.

use iced::widget::{Column, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{CannedResponse, CannedScope};
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_SMALL};
use rustket_ui::widget::{ChipKind, chip, divider, secondary_button};

use crate::room::panel::common::{self, filter_pills, search_field};
use crate::room::{Action, ComposeIntent, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Public,
    Department,
    Private,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    Filter(Filter),
    Use(String),
}

#[derive(Default)]
pub struct CannedPanel {
    query: String,
    filter: Filter,
}

/// `query` matches the shortcut or the text (case-insensitive), `filter` the scope.
pub fn matches(r: &CannedResponse, query: &str, filter: Filter) -> bool {
    let q = query.trim().to_lowercase();
    let scope_ok = match filter {
        Filter::All => true,
        Filter::Public => r.scope == CannedScope::Global,
        Filter::Department => r.scope == CannedScope::Department,
        Filter::Private => r.scope == CannedScope::User,
    };
    scope_ok
        && (q.is_empty()
            || r.shortcut.to_lowercase().contains(&q)
            || r.text.to_lowercase().contains(&q))
}

impl CannedPanel {
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        let _ = ctx;
        Action::none()
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        let _ = ctx;
        match msg {
            Msg::Query(q) => self.query = q,
            Msg::Filter(f) => self.filter = f,
            Msg::Use(text) => {
                return Action::effect(Effect::Compose(ComposeIntent::InsertText(text)));
            }
        }
        Action::none()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let omni = crate::omnichannel::room::omni_of(ctx.server_id);
        if !omni.canned_enabled(ctx.workspace) {
            return common::empty(
                Icon::MessageSquareText,
                t!("omnichannel.canned_unavailable").into_owned(),
            );
        }
        let items: Vec<CannedResponse> = omni
            .canned
            .all()
            .into_iter()
            .filter(|r| matches(r, &self.query, self.filter))
            .cloned()
            .collect();
        let mut list: Column<'a, Msg> = column![].spacing(0);
        if items.is_empty() {
            list = list.push(common::empty(
                Icon::MessageSquareText,
                t!("omnichannel.canned_none").into_owned(),
            ));
        }
        for r in items {
            let scope = match r.scope {
                CannedScope::Global => t!("omnichannel.canned_public"),
                CannedScope::Department => t!("omnichannel.canned_department"),
                CannedScope::User => t!("omnichannel.canned_private"),
                CannedScope::Unknown => t!("omnichannel.canned_other"),
            };
            list = list.push(
                container(
                    column![
                        row![
                            text(format!("!{}", r.shortcut))
                                .size(TEXT_BODY)
                                .font(FONT_SEMIBOLD)
                                .width(Length::Fill),
                            chip(scope.into_owned(), ChipKind::Neutral),
                            secondary_button(
                                t!("omnichannel.canned_use").into_owned(),
                                Some(Msg::Use(r.text.clone()))
                            ),
                        ]
                        .spacing(SPACE_2)
                        .align_y(Alignment::Center),
                        text(r.text.clone())
                            .size(TEXT_SMALL)
                            .style(style::text_secondary),
                    ]
                    .spacing(SPACE_1),
                )
                .padding([SPACE_2, SPACE_3])
                .width(Length::Fill),
            );
        }
        column![
            container(
                column![
                    search_field(
                        &self.query,
                        t!("omnichannel.canned_search").into_owned(),
                        Msg::Query
                    ),
                    filter_pills(
                        &[
                            (Filter::All, t!("omnichannel.canned_all").into_owned()),
                            (Filter::Public, t!("omnichannel.canned_public").into_owned()),
                            (
                                Filter::Department,
                                t!("omnichannel.canned_department").into_owned()
                            ),
                            (
                                Filter::Private,
                                t!("omnichannel.canned_private").into_owned()
                            ),
                        ],
                        self.filter,
                        Msg::Filter
                    ),
                ]
                .spacing(SPACE_2)
            )
            .padding(SPACE_3),
            divider(),
            scrollable(list)
                .height(Length::Fill)
                .style(style::scrollable),
        ]
        .height(Length::Fill)
        .into()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters() {
        let r: CannedResponse = serde_json::from_value(
            json!({"_id":"c","shortcut":"Hello","text":"Hi there!","scope":"user"}),
        )
        .unwrap();
        assert!(matches(&r, "", Filter::All));
        assert!(matches(&r, "hell", Filter::Private));
        assert!(matches(&r, "THERE", Filter::All));
        assert!(!matches(&r, "", Filter::Public));
        assert!(!matches(&r, "bye", Filter::All));
    }
}
