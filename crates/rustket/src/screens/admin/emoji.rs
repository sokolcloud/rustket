//! Custom emoji (docs/admin.md §7): paged list with search, multipart
//! create/update (the stored extension comes from the part's MIME type) and
//! delete.

use iced::widget::{column, container, image, row, text};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::{CustomEmoji, Paginated};
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::theme::{FONT_MEDIUM, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY};
use rustket_ui::widget::{danger_button, primary_button, secondary_button};

use super::common::{
    Confirm, Cx, List, ListMsg, Picked, Step, col, err_text, field, form_dialog, input, list_view,
    muted, pick_file, search_bar, sortable, table, title_row,
};
use super::{Message, V};
use crate::error::Res;

/// Upload limit we accept locally (the server resizes to 512×512).
const MAX_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum Msg {
    List(ListMsg),
    Loaded(u64, Res<Paginated<CustomEmoji>>),
    New,
    Open(Box<CustomEmoji>),
    CloseDialog,
    Name(String),
    Aliases(String),
    Pick,
    Picked(Result<Option<Picked>, String>),
    Save,
    Saved(Res<()>),
    Delete,
    DeleteNow(String),
    Deleted(Res<()>),
}

#[derive(Debug, Clone, Default)]
pub struct Form {
    pub emoji: Option<CustomEmoji>,
    pub name: String,
    pub aliases: String,
    pub file: Option<Picked>,
    pub busy: bool,
    pub error: Option<String>,
}

/// The name as the server stores it (slugified with `_`, colons removed).
pub fn normalized(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().trim_matches(':').chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.extend(c.to_lowercase());
        } else if c.is_whitespace() && !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_owned()
}

/// Client-side check of the server's name rule (`[\s,:><&"'\/\\()]` invalid).
pub fn name_valid(name: &str) -> bool {
    let n = name.trim().trim_matches(':');
    !n.is_empty()
        && !n
            .chars()
            .any(|c| c.is_whitespace() || ",:><&\"'/\\()".contains(c))
}

pub struct State {
    pub list: List<CustomEmoji>,
    pub form: Option<Form>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            list: List::new(Some(("name", true))),
            form: None,
        }
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        self.fetch(cx)
    }

    fn fetch(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let req = self.list.begin();
        let name = self.list.query.trim().to_owned();
        let page = self.list.rest_page();
        cx.ctx.rest_task(
            move |r| async move { r.emoji_custom_all(&name, &page).await },
            move |r| Message::Emoji(Msg::Loaded(req, r)),
        )
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let form = self.form.as_mut();
        match m {
            Msg::List(m) => {
                return match self.list.update(m) {
                    Step::None => Task::none(),
                    Step::Wait(t) => t.map(|m| Message::Emoji(Msg::List(m))),
                    Step::Load => self.fetch(cx),
                };
            }
            Msg::Loaded(req, r) => self.list.finish(req, r.map_err(|f| err_text(&f, cx.i18n))),
            Msg::New => self.form = Some(Form::default()),
            Msg::Open(e) => {
                self.form = Some(Form {
                    name: e.name.clone(),
                    aliases: e.aliases.join(", "),
                    emoji: Some(*e),
                    ..Form::default()
                });
            }
            Msg::CloseDialog => self.form = None,
            Msg::Name(n) => {
                if let Some(f) = form {
                    f.name = n;
                    f.error = None;
                }
            }
            Msg::Aliases(a) => {
                if let Some(f) = form {
                    f.aliases = a;
                    f.error = None;
                }
            }
            Msg::Pick => {
                let exts = ["png", "jpg", "jpeg", "gif", "svg", "webp"]
                    .map(String::from)
                    .to_vec();
                return pick_file(exts, MAX_BYTES, |r| Message::Emoji(Msg::Picked(r)));
            }
            Msg::Picked(r) => {
                if let Some(f) = form {
                    match r {
                        Ok(Some(p)) => f.file = Some(p),
                        Ok(None) => {}
                        Err(e) => f.error = Some(e),
                    }
                }
            }
            Msg::Save => {
                let Some(f) = form else {
                    return Task::none();
                };
                if !name_valid(&f.name) {
                    f.error = Some(t!("admin.emoji.invalid_name").into_owned());
                    return Task::none();
                }
                if f.aliases.chars().any(|c| ":><&|\"'/\\()".contains(c)) {
                    f.error = Some(t!("admin.emoji.invalid_alias").into_owned());
                    return Task::none();
                }
                if f.emoji.is_none() && f.file.is_none() {
                    f.error = Some(t!("admin.emoji.file_required").into_owned());
                    return Task::none();
                }
                f.busy = true;
                let (name, aliases) = (f.name.trim().to_owned(), f.aliases.clone());
                let upload = f.file.as_ref().map(Picked::upload);
                let id = f.emoji.as_ref().map(|e| e.id.clone());
                return cx.ctx.rest_task(
                    move |r| async move {
                        match (id, upload) {
                            (Some(id), up) => r.emoji_custom_update(&id, &name, &aliases, up).await,
                            (None, Some(up)) => r.emoji_custom_create(up, &name, &aliases).await,
                            (None, None) => Ok(()),
                        }
                    },
                    |r| Message::Emoji(Msg::Saved(r)),
                );
            }
            Msg::Saved(r) => match r {
                Ok(()) => {
                    cx.ok(t!("admin.emoji.saved").into_owned());
                    self.form = None;
                    return self.fetch(cx);
                }
                Err(e) => {
                    let text = err_text(&e, cx.i18n);
                    if let Some(f) = form {
                        f.busy = false;
                        f.error = Some(text);
                    }
                }
            },
            Msg::Delete => {
                if let Some(e) = form.and_then(|f| f.emoji.clone()) {
                    cx.ask(Confirm::danger(
                        t!("admin.emoji.delete_title").into_owned(),
                        t!("admin.emoji.delete_body", name = e.name).into_owned(),
                        Message::Emoji(Msg::DeleteNow(e.id)),
                    ));
                }
            }
            Msg::DeleteNow(id) => {
                return cx.ctx.rest_task(
                    move |r| async move { r.emoji_custom_delete(&id).await },
                    |r| Message::Emoji(Msg::Deleted(r)),
                );
            }
            Msg::Deleted(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.emoji.deleted").into_owned());
                    self.form = None;
                    return self.fetch(cx);
                }
            }
        }
        Task::none()
    }

    fn preview<'a>(e: &CustomEmoji, v: &V<'a>) -> Element<'a, Message> {
        let path = format!(
            "/emoji-custom/{}.{}{}",
            url::form_urlencoded::byte_serialize(e.name.as_bytes()).collect::<String>(),
            e.extension,
            e.etag
                .as_deref()
                .map(|t| format!("?etag={t}"))
                .unwrap_or_default()
        );
        let url = v.ctx.server.absolute(&path);
        match v.ctx.images.get(v.ctx.server_id, &url) {
            Some(l) => image(l.handle.clone()).width(24).height(24).into(),
            None => container(text("")).width(24).height(24).into(),
        }
    }

    pub fn view<'a>(&'a self, v: &V<'a>) -> Element<'a, Message> {
        let lm = |m| Message::Emoji(Msg::List(m));
        let rows = self
            .list
            .items
            .iter()
            .map(|e| {
                (
                    vec![
                        row![
                            Self::preview(e, v),
                            text(format!(":{}:", e.name))
                                .size(TEXT_BODY)
                                .font(FONT_MEDIUM)
                        ]
                        .spacing(SPACE_2)
                        .align_y(Alignment::Center)
                        .into(),
                        muted(e.aliases.join(", ")),
                    ],
                    Some(Message::Emoji(Msg::Open(Box::new(e.clone())))),
                )
            })
            .collect();
        let list = list_view(
            &self.list,
            move || {
                table(
                    vec![
                        sortable(t!("admin.emoji.name"), 3, "name"),
                        col(t!("admin.emoji.aliases"), 4),
                    ],
                    self.list.sort,
                    |f| Message::Emoji(Msg::List(ListMsg::Sort(f))),
                    rows,
                )
            },
            lm,
            t!("admin.emoji.empty").into_owned(),
        );
        column![
            title_row(
                t!("admin.section.emoji").into_owned(),
                vec![
                    primary_button(t!("admin.emoji.new"), Some(Message::Emoji(Msg::New)))
                        .icon(Icon::Plus)
                        .into()
                ]
            ),
            search_bar(
                &self.list.query,
                t!("admin.search").into_owned(),
                move |s| lm(ListMsg::Search(s)),
                lm(ListMsg::Reload)
            ),
            list
        ]
        .spacing(SPACE_4)
        .into()
    }

    pub fn overlay<'a>(&'a self, v: &V<'a>) -> Option<Element<'a, Message>> {
        let f = self.form.as_ref()?;
        let mut body = column![
            field(
                t!("admin.emoji.name").into_owned(),
                (!f.name.is_empty()).then(|| t!(
                    "admin.emoji.stored_as",
                    name = normalized(&f.name)
                )
                .into_owned()),
                input(&f.name, Some(|s| Message::Emoji(Msg::Name(s))))
            ),
            field(
                t!("admin.emoji.aliases").into_owned(),
                Some(t!("admin.emoji.aliases_hint").into_owned()),
                input(&f.aliases, Some(|s| Message::Emoji(Msg::Aliases(s))))
            ),
        ]
        .spacing(SPACE_3);
        let mut file_row = row![
            secondary_button(t!("admin.choose_file"), Some(Message::Emoji(Msg::Pick)))
                .icon(Icon::Upload)
        ]
        .spacing(SPACE_3)
        .align_y(Alignment::Center);
        if let Some(p) = &f.file {
            file_row = file_row.push(muted(format!("{} ({})", p.name, p.mime)));
        } else if let Some(e) = &f.emoji {
            file_row = file_row.push(Self::preview(e, v));
        }
        body = body.push(field(t!("admin.emoji.image").into_owned(), None, file_row));
        let extra = if f.emoji.is_some() {
            vec![
                danger_button(t!("admin.delete"), Some(Message::Emoji(Msg::Delete)))
                    .icon(Icon::Trash2)
                    .into(),
            ]
        } else {
            vec![]
        };
        Some(form_dialog(
            if f.emoji.is_some() {
                t!("admin.emoji.edit").into_owned()
            } else {
                t!("admin.emoji.new").into_owned()
            },
            container(body).width(Length::Fill).into(),
            f.error.as_deref(),
            f.busy,
            Some(Message::Emoji(Msg::Save)),
            Message::Emoji(Msg::CloseDialog),
            extra,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(normalized(":Party Parrot:"), "party_parrot");
        assert!(name_valid("party_parrot"));
        assert!(!name_valid("a b"));
        assert!(!name_valid("a<b"));
        assert!(!name_valid("::"));
    }
}
