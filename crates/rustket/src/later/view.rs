//! Rendering of the Later view: tabs, rows (the timeline's row layout with
//! the shared body renderer) and the view's dialogs.

use std::time::Instant;

use chrono::{DateTime, Local};
use iced::widget::{button, checkbox, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::ServerUrl;
use rocketchat::models::Message as RcMessage;
use rust_i18n::t;
use rustket_core::config::UiPrefs;
use rustket_core::later::LaterStatus;
use rustket_core::room_list::{self, Avatar};
use rustket_core::workspace::{LaterRow, LaterTab};
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL, font_semibold,
};
use rustket_ui::widget::{
    AvatarShape, MessageRowProps, TooltipPosition, avatar, danger_button, dialog, empty_state,
    header_bar, icon_button, icon_button_with, message_row, message_toolbar, modal_keyed,
    secondary_button, tooltip,
};

use super::{Dialog, LaterView, Msg};
use crate::images::Images;
use crate::room::ViewCtx;
use crate::room::panel::common;
use crate::room::render::plural;
use crate::workspace::service_sync::Status;

/// Id of the list's scrollable.
const LIST_ID: &str = "later-list";

fn row_id(mid: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("later-row-{mid}"))
}

/// Scrolls the list so that the row of `mid` shows (in the middle, like a
/// message the timeline jumps to).
pub fn reveal<M: Send + 'static>(mid: &str) -> iced::Task<M> {
    crate::room::timeline::list::reveal(iced::widget::Id::new(LIST_ID), row_id(mid), false)
}

/// What the view reads besides its state.
pub struct Ctx<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub workspace: &'a Workspace,
    pub images: &'a Images,
    pub prefs: &'a UiPrefs,
    /// The session is connected (unstarring needs it).
    pub online: bool,
    /// Sync state of the Later and reminder keys (the worse of both).
    pub sync: Status,
    /// Rooms scanned for starred messages / to scan, while scans run.
    pub scanning: Option<(usize, usize)>,
    pub now: Instant,
    pub local_now: DateTime<Local>,
}

impl<'a> Ctx<'a> {
    /// The room context of a body (`rid` lives as long as the data).
    fn view_ctx(&self, rid: &'a str) -> ViewCtx<'a> {
        ViewCtx {
            server_id: self.server_id,
            server: self.server,
            rid,
            workspace: self.workspace,
            images: self.images,
            prefs: self.prefs,
            connected: self.online,
            panel: None,
            now: self.now,
        }
    }

    fn room_ctx<'r>(&'r self, rid: &'r str) -> ViewCtx<'r> {
        ViewCtx {
            server_id: self.server_id,
            server: self.server,
            rid,
            workspace: self.workspace,
            images: self.images,
            prefs: self.prefs,
            connected: self.online,
            panel: None,
            now: self.now,
        }
    }

    fn use_real(&self) -> bool {
        self.workspace.use_real_names(self.prefs.use_real_names)
    }

    fn room_name(&self, rid: &str) -> String {
        self.workspace
            .room(rid)
            .map(|e| room_list::display_name(self.workspace, e, self.use_real()))
            .unwrap_or_default()
    }

    fn moment(&self, at_ms: i64) -> String {
        crate::format::moment(at_ms, self.local_now, self.prefs.clock_24h)
    }

    fn now_ms(&self) -> i64 {
        self.local_now.timestamp_millis()
    }
}

fn tab_label(t: LaterTab) -> String {
    match t {
        LaterTab::InProgress => t!("later.tab_in_progress"),
        LaterTab::Reminders => t!("later.tab_reminders"),
        LaterTab::Done => t!("later.tab_done"),
        LaterTab::Archived => t!("later.tab_archived"),
    }
    .into_owned()
}

fn chip(icon_: Icon, label: String, danger: bool) -> Element<'static, Msg> {
    let style = if danger {
        style::text_danger
    } else {
        style::text_secondary
    };
    row![
        icon::sized(icon_, 12.0).style(style),
        text(label).size(TEXT_CAPTION).style(style),
    ]
    .spacing(SPACE_1)
    .align_y(Alignment::Center)
    .into()
}

fn tool(icon_: Icon, label: String, on: Option<Msg>) -> Element<'static, Msg> {
    icon_button_with(icon_, on, label, false, 28.0)
}

impl LaterView {
    pub fn view<'a>(&'a self, c: &Ctx<'a>) -> Element<'a, Msg> {
        let counts = self.counts(c.workspace);
        let tabs = row(LaterTab::ALL.iter().zip(counts).map(|(t, n)| {
            let label = if n > 0 && matches!(t, LaterTab::InProgress | LaterTab::Reminders) {
                format!("{} ({n})", tab_label(*t))
            } else {
                tab_label(*t)
            };
            button(text(label).size(TEXT_SMALL).font(font_semibold()))
                .padding([4.0, SPACE_3])
                .style(style::button_reaction(self.tab == *t))
                .on_press(Msg::Tab(*t))
                .into()
        }))
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        let actions = vec![
            icon_button(Icon::RefreshCw, Some(Msg::Refresh), t!("later.refresh")),
            icon_button(Icon::X, Some(Msg::Close), t!("later.close")),
        ];
        let header = header_bar(
            Some(icon::sized(Icon::BookmarkCheck, 18.0).into()),
            t!("later.title").into_owned(),
            None,
            actions,
        );
        let mut top = column![container(tabs).padding([SPACE_2, SPACE_4])].spacing(SPACE_1);
        match &c.sync {
            Status::Synced => {}
            Status::Pending => {
                top = top.push(
                    container(
                        text(t!("later.sync_pending").into_owned())
                            .size(TEXT_CAPTION)
                            .style(style::text_hint),
                    )
                    .padding([0.0, SPACE_4]),
                )
            }
            Status::Failed(e) => {
                top = top.push(
                    container(
                        text(t!("later.sync_failed", error = e.as_str()).into_owned())
                            .size(TEXT_CAPTION)
                            .style(style::text_danger),
                    )
                    .padding([0.0, SPACE_4]),
                )
            }
        }
        if let Some((done, total)) = c.scanning.filter(|_| self.tab != LaterTab::Reminders) {
            top = top.push(
                container(
                    text(t!("later.scanning", done = done, total = total).into_owned())
                        .size(TEXT_CAPTION)
                        .style(style::text_hint),
                )
                .padding([0.0, SPACE_4]),
            );
        }
        let body = self.list(c);
        let main = column![header, top, body]
            .width(Length::Fill)
            .height(Length::Fill);
        let user = self
            .user
            .as_ref()
            .map(|u| u.view(&c.view_ctx(&u.rid)).map(Msg::User));
        let main = crate::room::panel::user_info::beside(main.into(), user);
        let (overlay, dismiss) = match self.overlay(c) {
            Some((k, e)) => (Some((k, e)), Some(Msg::CloseDialog)),
            None => match self.lightbox(c) {
                Some((e, d)) => (Some((4, e)), Some(d)),
                None => (None, None),
            },
        };
        container(modal_keyed(main, overlay, dismiss))
            .style(style::container_room)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn list<'a>(&'a self, c: &Ctx<'_>) -> Element<'a, Msg> {
        let ws = c.workspace;
        if self.tab != LaterTab::Reminders && !ws.settings().allow_starring() {
            return container(empty_state(
                Icon::StarOff,
                t!("later.starring_off"),
                String::new(),
            ))
            .center(Length::Fill)
            .into();
        }
        let rows = self.rows(ws);
        if rows.is_empty() {
            let (icon_, title, body) = match self.tab {
                LaterTab::InProgress => (
                    Icon::BookmarkCheck,
                    t!("later.empty_title"),
                    t!("later.empty_body"),
                ),
                LaterTab::Reminders => (
                    Icon::Clock,
                    t!("later.empty_reminders_title"),
                    t!("later.empty_reminders_body"),
                ),
                LaterTab::Done | LaterTab::Archived => {
                    (Icon::Inbox, t!("later.empty_other"), "".into())
                }
            };
            return container(empty_state(icon_, title, body))
                .center(Length::Fill)
                .into();
        }
        let mut col = column![].spacing(0);
        for r in rows.iter().take(self.shown) {
            col = col.push(self.row(c, r));
        }
        let left = rows.len().saturating_sub(self.shown);
        if left > 0 {
            col = col.push(
                container(secondary_button(
                    t!("later.show_more", count = left.min(super::PAGE)),
                    Some(Msg::ShowMore),
                ))
                .center_x(Length::Fill)
                .padding(SPACE_2),
            );
        }
        if self.tab == LaterTab::Done {
            let clear =
                secondary_button(t!("later.clear_done"), c.online.then_some(Msg::ClearDone))
                    .icon(Icon::Trash2);
            let clear: Element<'a, Msg> = if c.online {
                clear.into()
            } else {
                tooltip(clear, t!("later.not_connected"), TooltipPosition::Top)
            };
            col = col.push(container(clear).padding([SPACE_3, SPACE_4]));
        }
        scrollable(col)
            .id(iced::widget::Id::new(LIST_ID))
            .height(Length::Fill)
            .into()
    }

    /// The chips under a row's author line: room, due date, reminder.
    fn chips(&self, c: &Ctx<'_>, r: &LaterRow) -> Element<'static, Msg> {
        let now = c.now_ms();
        let room = c.room_name(&r.rid);
        let place = match &r.tmid {
            Some(_) => t!("later.thread_in", room = room.as_str()).into_owned(),
            None => room,
        };
        let mut chips = row![chip(
            if r.starred { Icon::Star } else { Icon::Bell },
            place,
            false
        )]
        .spacing(SPACE_3)
        .align_y(Alignment::Center);
        if let Some(flag) = r
            .tmid
            .as_deref()
            .and_then(|t| c.workspace.thread_flag(t))
            .or_else(|| c.workspace.room_flag(&r.rid))
        {
            chips = chips.push(rustket_ui::widget::flag_glyph(flag.index(), 10.0));
        }
        if let Some(due) = r.due_ms.filter(|_| r.starred) {
            let label = if due <= now {
                t!("later.overdue", time = c.moment(due).as_str())
            } else {
                t!("later.due_on", time = c.moment(due).as_str())
            };
            chips = chips.push(chip(Icon::CalendarClock, label.into_owned(), due <= now));
        }
        if let Some(at) = r.reminder_ms {
            chips = chips.push(chip(Icon::AlarmClock, c.moment(at), at <= now));
        }
        chips.into()
    }

    fn toolbar(&self, c: &Ctx<'_>, r: &LaterRow) -> Vec<Element<'static, Msg>> {
        let mid = r.mid.clone();
        let mut t = vec![tool(
            Icon::ArrowUpRight,
            t!("later.act_open").into_owned(),
            Some(Msg::Open(mid.clone())),
        )];
        if r.starred && self.tab != LaterTab::Reminders {
            match r.status {
                LaterStatus::InProgress => {
                    t.push(tool(
                        Icon::CircleCheck,
                        t!("later.act_done").into_owned(),
                        Some(Msg::SetStatus(mid.clone(), LaterStatus::Done)),
                    ));
                    t.push(tool(
                        Icon::Archive,
                        t!("later.act_archive").into_owned(),
                        Some(Msg::SetStatus(mid.clone(), LaterStatus::Archived)),
                    ));
                }
                LaterStatus::Done | LaterStatus::Archived => t.push(tool(
                    Icon::RotateCcw,
                    t!("later.act_reopen").into_owned(),
                    Some(Msg::SetStatus(mid.clone(), LaterStatus::InProgress)),
                )),
            }
            t.push(tool(
                Icon::CalendarClock,
                t!("later.act_due").into_owned(),
                Some(Msg::Due(mid.clone())),
            ));
        }
        if r.reminder_ms.is_some() {
            if self.tab == LaterTab::Reminders {
                t.push(tool(
                    Icon::AlarmClockPlus,
                    t!("later.act_snooze_60").into_owned(),
                    Some(Msg::Snooze(mid.clone(), 60)),
                ));
            }
            t.push(tool(
                Icon::AlarmClock,
                t!("later.act_edit_reminder").into_owned(),
                Some(Msg::Remind(mid.clone())),
            ));
            t.push(tool(
                Icon::AlarmClockOff,
                t!("later.act_remove_reminder").into_owned(),
                Some(Msg::RemoveReminder(mid.clone())),
            ));
        } else {
            t.push(tool(
                Icon::Clock,
                t!("later.act_remind").into_owned(),
                Some(Msg::Remind(mid.clone())),
            ));
        }
        if r.starred {
            let label = if c.online {
                t!("later.act_remove").into_owned()
            } else {
                format!("{} ({})", t!("later.act_remove"), t!("later.not_connected"))
            };
            t.push(tool(
                Icon::StarOff,
                label,
                c.online.then(|| Msg::Unstar(mid.clone())),
            ));
        }
        t
    }

    fn row(&self, c: &Ctx<'_>, r: &LaterRow) -> Element<'static, Msg> {
        let ws = c.workspace;
        let toolbar = self.toolbar(c, r);
        let chips = self.chips(c, r);
        let selected = self.selected.as_deref() == Some(r.mid.as_str());
        let body: Element<'static, Msg> = match ws.later_message(&r.mid) {
            Some(m) => self.message(c, m, toolbar, chips, selected),
            None => {
                // A reminder set on another device whose message this one
                // has not seen: only where it is.
                let room = c.room_name(&r.rid);
                let p = MessageRowProps {
                    actions_toolbar: Some(message_toolbar(toolbar)),
                    is_selected: selected,
                    ..MessageRowProps::new(
                        container(icon::sized(Icon::Bell, 20.0).style(style::text_hint))
                            .center_x(36.0),
                        t!("later.message_in", room = room.as_str()).into_owned(),
                        String::new(),
                        chips,
                    )
                };
                message_row(p)
            }
        };
        let mid = r.mid.clone();
        container(iced::widget::mouse_area(body).on_press(Msg::Select(mid)))
            .id(row_id(&r.mid))
            .into()
    }

    fn message(
        &self,
        c: &Ctx<'_>,
        m: &RcMessage,
        toolbar: Vec<Element<'static, Msg>>,
        chips: Element<'static, Msg>,
        selected: bool,
    ) -> Element<'static, Msg> {
        let ws = c.workspace;
        let vc = c.room_ctx(&m.rid);
        let use_real = c.use_real();
        let username = m.u.username.clone().unwrap_or_default();
        let time = common::time_label(m.ts, &vc);
        let name = m
            .alias
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| common::user_display(ws, use_real, &username, m.u.name.as_deref()));
        let rid = m.rid.clone();
        let body: Element<'static, Msg> = if crate::e2e::shown::undecrypted(m) {
            text(t!("later.encrypted").into_owned())
                .size(TEXT_BODY)
                .style(style::text_hint)
                .into()
        } else {
            self.viewer.body(m, &vc).map(move |v| Msg::View {
                rid: rid.clone(),
                m: v,
            })
        };
        let src = c.images.avatar(
            c.server_id,
            c.server,
            &Avatar::User {
                username: username.clone(),
            },
            &name,
        );
        let mut p = MessageRowProps::new(
            avatar(src, 36.0, AvatarShape::Rounded),
            name,
            time,
            column![body, chips].spacing(SPACE_1),
        );
        p.edited = m.edited_at.is_some();
        p.edited_label = t!("room_timeline.edited").into_owned();
        p.actions_toolbar = Some(message_toolbar(toolbar));
        p.is_selected = selected;
        message_row(p)
    }

    /// The image lightbox of a row's body, with the message closing it.
    fn lightbox(&self, c: &Ctx<'_>) -> Option<(Element<'static, Msg>, Msg)> {
        let rid = self.image_rid.clone()?;
        let e = self.viewer.overlay(&c.room_ctx(&rid))?;
        let r2 = rid.clone();
        Some((
            e.map(move |m| Msg::View { rid: r2.clone(), m }),
            Msg::View {
                rid,
                m: crate::room::timeline::ViewerMsg::CloseImage,
            },
        ))
    }

    /// The open dialog with a key for the modal.
    fn overlay<'a>(&'a self, c: &Ctx<'_>) -> Option<(u8, Element<'a, Msg>)> {
        let close = Msg::CloseDialog;
        match self.dialog.as_ref()? {
            Dialog::Remind { mid, picker, .. } => {
                let content = column![
                    excerpt(c, mid),
                    picker
                        .view(c.local_now, c.prefs.morning_hour(), c.prefs.clock_24h)
                        .map(Msg::Picker),
                ]
                .spacing(SPACE_3);
                Some((
                    1,
                    dialog(
                        t!("room_timeline.remind_title").into_owned(),
                        content,
                        vec![super::when::cancel(close.clone())],
                        Some(close),
                    ),
                ))
            }
            Dialog::Due {
                mid,
                picker,
                also_remind,
                ..
            } => {
                let content = column![
                    excerpt(c, mid),
                    picker
                        .view(c.local_now, c.prefs.morning_hour(), c.prefs.clock_24h)
                        .map(Msg::Picker),
                    checkbox(*also_remind)
                        .label(t!("later.due_also_remind").into_owned())
                        .on_toggle(Msg::AlsoRemind)
                        .text_size(TEXT_SMALL),
                ]
                .spacing(SPACE_3);
                Some((
                    2,
                    dialog(
                        t!("later.due_title").into_owned(),
                        content,
                        vec![super::when::cancel(close.clone())],
                        Some(close),
                    ),
                ))
            }
            Dialog::ClearDone(items) => {
                let n = items.len() as u64;
                Some((
                    3,
                    dialog(
                        t!("later.clear_done_title").into_owned(),
                        text(
                            t!(format!("later.clear_done_body_{}", plural(n)), count = n)
                                .into_owned(),
                        )
                        .size(TEXT_BODY),
                        vec![
                            super::when::cancel(close.clone()),
                            danger_button(
                                t!("later.clear_done_confirm").into_owned(),
                                c.online.then_some(Msg::ConfirmClearDone),
                            )
                            .into(),
                        ],
                        Some(close),
                    ),
                ))
            }
        }
    }
}

/// One line of the message a dialog is about.
fn excerpt(c: &Ctx<'_>, mid: &str) -> Element<'static, Msg> {
    let line = match c.workspace.later_message(mid) {
        Some(m) if crate::e2e::shown::undecrypted(m) => t!("later.encrypted").into_owned(),
        Some(m) => crate::room::ForwardRef::of(m).excerpt,
        None => String::new(),
    };
    text(line)
        .size(TEXT_SMALL)
        .style(style::text_secondary)
        .into()
}
