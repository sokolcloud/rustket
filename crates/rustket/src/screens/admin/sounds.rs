//! Custom sounds (docs/admin.md §8): paged list, multipart create/update (MP3
//! or WAV, typed by file name, at most 5 MiB) and delete.

use iced::widget::{column, container, row};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::Paginated;
use rocketchat::models::admin::CustomSound;
use rocketchat::rest::admin::MAX_SOUND_BYTES;
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::theme::{SPACE_3, SPACE_4};
use rustket_ui::widget::{danger_button, primary_button, secondary_button};

use super::common::{
    Confirm, Cx, List, ListMsg, Picked, Step, col, err_text, field, form_dialog, input, list_view,
    muted, pick_file, search_bar, small, sortable, table, title_row,
};
use super::{Message, V};
use crate::error::Res;

#[derive(Debug, Clone)]
pub enum Msg {
    List(ListMsg),
    Loaded(u64, Res<Paginated<CustomSound>>),
    New,
    Open(Box<CustomSound>),
    CloseDialog,
    Name(String),
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
    pub sound: Option<CustomSound>,
    pub name: String,
    pub file: Option<Picked>,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct State {
    pub list: List<CustomSound>,
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
            move |r| async move { r.custom_sounds_list(&name, &page).await },
            move |r| Message::Sounds(Msg::Loaded(req, r)),
        )
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let form = self.form.as_mut();
        match m {
            Msg::List(m) => {
                return match self.list.update(m) {
                    Step::None => Task::none(),
                    Step::Wait(t) => t.map(|m| Message::Sounds(Msg::List(m))),
                    Step::Load => self.fetch(cx),
                };
            }
            Msg::Loaded(req, r) => {
                if self.list.finish(req, r.map_err(|f| err_text(&f, cx.i18n))) {
                    return self.fetch(cx);
                }
            }
            Msg::New => self.form = Some(Form::default()),
            Msg::Open(s) => {
                self.form = Some(Form {
                    name: s.name.clone(),
                    sound: Some(*s),
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
            Msg::Pick => {
                return pick_file(vec!["mp3".into(), "wav".into()], MAX_SOUND_BYTES, |r| {
                    Message::Sounds(Msg::Picked(r))
                });
            }
            Msg::Picked(r) => {
                if let Some(f) = form {
                    match r {
                        Ok(Some(p)) => {
                            if f.name.is_empty() {
                                f.name = p
                                    .name
                                    .rsplit_once('.')
                                    .map(|(n, _)| n.to_owned())
                                    .unwrap_or_else(|| p.name.clone());
                            }
                            f.file = Some(p);
                        }
                        Ok(None) => {}
                        Err(e) => f.error = Some(e),
                    }
                }
            }
            Msg::Save => {
                let Some(f) = form else {
                    return Task::none();
                };
                if f.name.trim().is_empty() {
                    f.error = Some(t!("admin.name_required").into_owned());
                    return Task::none();
                }
                if f.sound.is_none() && f.file.is_none() {
                    f.error = Some(t!("admin.sounds.file_required").into_owned());
                    return Task::none();
                }
                f.busy = true;
                let name = f.name.trim().to_owned();
                let upload = f.file.as_ref().map(Picked::upload);
                let id = f.sound.as_ref().map(|s| s.id.clone());
                return cx.ctx.rest_task(
                    move |r| async move {
                        match (id, upload) {
                            (Some(id), up) => r.custom_sound_update(&id, &name, up).await,
                            (None, Some(up)) => r.custom_sound_create(up, &name).await.map(|_| ()),
                            (None, None) => Ok(()),
                        }
                    },
                    |r| Message::Sounds(Msg::Saved(r)),
                );
            }
            Msg::Saved(r) => match r {
                Ok(()) => {
                    cx.ok(t!("admin.sounds.saved").into_owned());
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
                if let Some(s) = form.and_then(|f| f.sound.clone()) {
                    cx.ask(Confirm::danger(
                        t!("admin.sounds.delete_title").into_owned(),
                        t!("admin.sounds.delete_body", name = s.name).into_owned(),
                        Message::Sounds(Msg::DeleteNow(s.id)),
                    ));
                }
            }
            Msg::DeleteNow(id) => {
                return cx.ctx.rest_task(
                    move |r| async move { r.custom_sound_delete(&id).await },
                    |r| Message::Sounds(Msg::Deleted(r)),
                );
            }
            Msg::Deleted(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.sounds.deleted").into_owned());
                    self.form = None;
                    return self.fetch(cx);
                }
            }
        }
        Task::none()
    }

    pub fn view<'a>(&'a self, _v: &V<'a>) -> Element<'a, Message> {
        let lm = |m| Message::Sounds(Msg::List(m));
        let rows = self
            .list
            .items
            .iter()
            .map(|s| {
                (
                    vec![small(s.name.clone()), muted(s.extension.clone())],
                    Some(Message::Sounds(Msg::Open(Box::new(s.clone())))),
                )
            })
            .collect();
        let list = list_view(
            &self.list,
            move || {
                table(
                    vec![
                        sortable(t!("admin.sounds.name"), 4, "name"),
                        col(t!("admin.sounds.format"), 1),
                    ],
                    self.list.sort,
                    |f| Message::Sounds(Msg::List(ListMsg::Sort(f))),
                    rows,
                )
            },
            lm,
            t!("admin.sounds.empty").into_owned(),
        );
        column![
            title_row(
                t!("admin.section.sounds").into_owned(),
                vec![
                    primary_button(t!("admin.sounds.new"), Some(Message::Sounds(Msg::New)))
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

    pub fn overlay<'a>(&'a self, _v: &V<'a>) -> Option<Element<'a, Message>> {
        let f = self.form.as_ref()?;
        let mut file_row = row![
            secondary_button(t!("admin.choose_file"), Some(Message::Sounds(Msg::Pick)))
                .icon(Icon::Upload)
        ]
        .spacing(SPACE_3)
        .align_y(Alignment::Center);
        if let Some(p) = &f.file {
            file_row = file_row.push(muted(p.name.clone()));
        } else if let Some(s) = &f.sound {
            file_row = file_row.push(muted(format!("{}.{}", s.name, s.extension)));
        }
        let body = column![
            field(
                t!("admin.sounds.name").into_owned(),
                None,
                input(&f.name, Some(|s| Message::Sounds(Msg::Name(s))))
            ),
            field(
                t!("admin.sounds.file").into_owned(),
                Some(t!("admin.sounds.file_hint").into_owned()),
                file_row
            ),
        ]
        .spacing(SPACE_3);
        let extra = if f.sound.is_some() {
            vec![
                danger_button(t!("admin.delete"), Some(Message::Sounds(Msg::Delete)))
                    .icon(Icon::Trash2)
                    .into(),
            ]
        } else {
            vec![]
        };
        Some(form_dialog(
            if f.sound.is_some() {
                t!("admin.sounds.edit").into_owned()
            } else {
                t!("admin.sounds.new").into_owned()
            },
            container(body).width(Length::Fill).into(),
            f.error.as_deref(),
            f.busy,
            Some(Message::Sounds(Msg::Save)),
            Message::Sounds(Msg::CloseDialog),
            extra,
        ))
    }
}
