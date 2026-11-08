//! Integrations (docs/admin.md §10): incoming and outgoing webhooks — list with
//! type filter, search and pages; the editor with the script editor, webhook
//! URL and curl example; the outgoing history with replay and clear.

use iced::widget::{button, column, container, pick_list, row, text, text_editor};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::Paginated;
use rocketchat::models::admin::{
    Integration, IntegrationHistory, IntegrationParams, IntegrationType, OutgoingEvent, RetryDelay,
};
use rust_i18n::t;
use rustket_core::permissions::has_any_permission;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_MEDIUM, MONOSPACE_FONT, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_SMALL,
};
use rustket_ui::widget::{
    BannerKind, banner, danger_button, ghost_button, icon_button, primary_button, secondary_button,
};

use super::common::{
    Confirm, Cx, List, ListMsg, Step, back_row, card, col, date, err_text, field, input, kv,
    list_view, muted, readonly, search_bar, small, sortable, switch, table, title_row, yes_no,
};
use super::{Message, V};
use crate::error::Res;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event(pub OutgoingEvent);

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&event_label(self.0))
    }
}

fn event_label(e: OutgoingEvent) -> String {
    match e {
        OutgoingEvent::SendMessage => t!("admin.integrations.ev_send_message"),
        OutgoingEvent::FileUploaded => t!("admin.integrations.ev_file_uploaded"),
        OutgoingEvent::RoomArchived => t!("admin.integrations.ev_room_archived"),
        OutgoingEvent::RoomCreated => t!("admin.integrations.ev_room_created"),
        OutgoingEvent::RoomJoined => t!("admin.integrations.ev_room_joined"),
        OutgoingEvent::RoomLeft => t!("admin.integrations.ev_room_left"),
        OutgoingEvent::UserCreated => t!("admin.integrations.ev_user_created"),
        OutgoingEvent::Unknown => t!("admin.integrations.ev_unknown"),
    }
    .into_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delay(pub RetryDelay);

impl std::fmt::Display for Delay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&match self.0 {
            RetryDelay::PowersOfTen => t!("admin.integrations.powers_of_ten"),
            RetryDelay::PowersOfTwo => t!("admin.integrations.powers_of_two"),
            RetryDelay::IncrementsOfTwo => t!("admin.integrations.increments_of_two"),
            RetryDelay::Unknown => t!("admin.integrations.ev_unknown"),
        })
    }
}

const DELAYS: [Delay; 3] = [
    Delay(RetryDelay::PowersOfTen),
    Delay(RetryDelay::PowersOfTwo),
    Delay(RetryDelay::IncrementsOfTwo),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum F {
    Name,
    Channel,
    Username,
    Alias,
    Avatar,
    Emoji,
    TriggerWords,
    TargetRoom,
    Token,
    RetryCount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Enabled,
    Override,
    Script,
    TriggerAnywhere,
    Impersonate,
    RunOnEdits,
    Retry,
}

#[derive(Debug, Clone)]
pub enum Msg {
    List(ListMsg),
    Loaded(u64, Res<Paginated<Integration>>),
    Filter(Option<IntegrationType>),
    New(IntegrationType),
    Open(String),
    Got(Res<Integration>),
    Back,
    Field(F, String),
    Flag(Flag, bool),
    Event(Event),
    Delay(Delay),
    Script(text_editor::Action),
    Urls(text_editor::Action),
    Save,
    Saved(Res<Integration>),
    Copy(String),
    Delete,
    DeleteNow(IntegrationType, String),
    Deleted(Res<()>),
    History,
    HistoryList(ListMsg),
    HistoryLoaded(u64, Res<Paginated<IntegrationHistory>>),
    Replay(String),
    Replayed(Res<()>),
    ClearHistory,
    ClearNow,
    Cleared(Res<()>),
}

pub struct Editor {
    pub integration: Option<Integration>,
    pub p: IntegrationParams,
    pub trigger_words: String,
    pub retry_count: String,
    pub script: text_editor::Content,
    pub urls: text_editor::Content,
    pub busy: bool,
    pub error: Option<String>,
}

impl Editor {
    fn new(kind: IntegrationType, me: &str) -> Self {
        let p = match kind {
            IntegrationType::Outgoing => {
                IntegrationParams::outgoing("", OutgoingEvent::SendMessage, vec![], me)
            }
            _ => IntegrationParams::incoming("", "", me),
        };
        Self::from_params(None, p)
    }

    fn of(i: Integration) -> Self {
        let p = IntegrationParams::from_integration(&i);
        Self::from_params(Some(i), p)
    }

    fn from_params(integration: Option<Integration>, p: IntegrationParams) -> Self {
        Self {
            trigger_words: p.trigger_words.clone().unwrap_or_default().join(", "),
            retry_count: p
                .retry_count
                .map(|c| c.to_string())
                .unwrap_or_else(|| "6".into()),
            script: text_editor::Content::with_text(p.script.as_deref().unwrap_or("")),
            urls: text_editor::Content::with_text(&p.urls.clone().unwrap_or_default().join("\n")),
            integration,
            p,
            busy: false,
            error: None,
        }
    }

    fn outgoing(&self) -> bool {
        self.p.kind == IntegrationType::Outgoing
    }

    fn event(&self) -> OutgoingEvent {
        self.p.event.unwrap_or(OutgoingEvent::SendMessage)
    }

    /// The parameters to send, with the text areas folded in.
    pub fn params(&self) -> Result<IntegrationParams, String> {
        let mut p = self.p.clone();
        if p.name.trim().is_empty() {
            return Err(t!("admin.name_required").into_owned());
        }
        if p.username.trim().is_empty() {
            return Err(t!("admin.integrations.username_required").into_owned());
        }
        let script = self.script.text();
        p.script = (!script.trim().is_empty()).then_some(script);
        if self.outgoing() {
            let ev = self.event();
            let urls: Vec<String> = self
                .urls
                .text()
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect();
            if urls.is_empty() {
                return Err(t!("admin.integrations.urls_required").into_owned());
            }
            p.urls = Some(urls);
            p.trigger_words = ev.uses_trigger_words().then(|| {
                self.trigger_words
                    .split(',')
                    .map(str::trim)
                    .filter(|w| !w.is_empty())
                    .map(str::to_owned)
                    .collect()
            });
            // The schemas require `channel`; send "" when the event has none.
            if !ev.uses_channel() {
                p.channel = String::new();
            }
            if !ev.uses_target_room() {
                p.target_room = None;
            }
            // Required (non-empty) for updates.
            if p.token.as_deref().is_none_or(|t| t.trim().is_empty()) {
                return Err(t!("admin.integrations.token_required").into_owned());
            }
            if p.retry_failed_calls == Some(true) {
                p.retry_count = Some(
                    self.retry_count
                        .trim()
                        .parse()
                        .map_err(|_| t!("admin.settings.error.not_integer").into_owned())?,
                );
            }
        } else if p.channel.trim().is_empty() {
            return Err(t!("admin.integrations.channel_required").into_owned());
        }
        Ok(p)
    }
}

pub enum View {
    List,
    Edit(Box<Editor>),
    History(String, Box<List<IntegrationHistory>>),
}

pub struct State {
    pub list: List<Integration>,
    pub filter: Option<IntegrationType>,
    pub view: View,
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
            filter: None,
            view: View::List,
        }
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let req = self.list.begin();
        let name = self.list.query.trim().to_owned();
        let kind = self.filter;
        let page = self.list.rest_page();
        cx.ctx.rest_task(
            move |r| async move { r.integrations_list(&name, kind, &page).await },
            move |r| Message::Integrations(Msg::Loaded(req, r)),
        )
    }

    fn editor(&mut self) -> Option<&mut Editor> {
        match &mut self.view {
            View::Edit(e) => Some(e),
            _ => None,
        }
    }

    fn history(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let View::History(id, list) = &mut self.view else {
            return Task::none();
        };
        let req = list.begin();
        let (id, page) = (id.clone(), list.rest_page());
        cx.ctx.rest_task(
            move |r| async move { r.integration_history(&id, &page).await },
            move |r| Message::Integrations(Msg::HistoryLoaded(req, r)),
        )
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match m {
            Msg::List(m) => {
                return match self.list.update(m) {
                    Step::None => Task::none(),
                    Step::Wait(t) => t.map(|m| Message::Integrations(Msg::List(m))),
                    Step::Load => self.load(cx),
                };
            }
            Msg::Loaded(req, r) => self.list.finish(req, r.map_err(|f| err_text(&f, cx.i18n))),
            Msg::Filter(f) => {
                self.filter = f;
                self.list.offset = 0;
                return self.load(cx);
            }
            Msg::New(kind) => {
                let me = cx
                    .ctx
                    .workspace
                    .username()
                    .unwrap_or("rocket.cat")
                    .to_owned();
                self.view = View::Edit(Box::new(Editor::new(kind, &me)));
            }
            Msg::Open(id) => {
                return cx.ctx.rest_task(
                    move |r| async move { r.integration_get(&id).await },
                    |r| Message::Integrations(Msg::Got(r)),
                );
            }
            Msg::Got(r) => match r {
                Ok(i) => self.view = View::Edit(Box::new(Editor::of(i))),
                Err(f) => cx.fail(&f),
            },
            Msg::Back => {
                self.view = View::List;
                return self.load(cx);
            }
            Msg::Field(f, v) => {
                if let Some(e) = self.editor() {
                    e.error = None;
                    let opt = |v: String| (!v.is_empty()).then_some(v);
                    match f {
                        F::Name => e.p.name = v,
                        F::Channel => e.p.channel = v,
                        F::Username => e.p.username = v,
                        F::Alias => e.p.alias = opt(v),
                        F::Avatar => e.p.avatar = opt(v),
                        F::Emoji => e.p.emoji = opt(v),
                        F::TriggerWords => e.trigger_words = v,
                        F::TargetRoom => e.p.target_room = opt(v),
                        F::Token => e.p.token = Some(v),
                        F::RetryCount => e.retry_count = v,
                    }
                }
            }
            Msg::Flag(f, b) => {
                if let Some(e) = self.editor() {
                    match f {
                        Flag::Enabled => e.p.enabled = b,
                        Flag::Override => e.p.override_destination_channel_enabled = Some(b),
                        Flag::Script => e.p.script_enabled = b,
                        Flag::TriggerAnywhere => e.p.trigger_word_anywhere = Some(b),
                        Flag::Impersonate => e.p.impersonate_user = Some(b),
                        Flag::RunOnEdits => e.p.run_on_edits = Some(b),
                        Flag::Retry => e.p.retry_failed_calls = Some(b),
                    }
                }
            }
            Msg::Event(ev) => {
                if let Some(e) = self.editor() {
                    e.p.event = Some(ev.0);
                }
            }
            Msg::Delay(d) => {
                if let Some(e) = self.editor() {
                    e.p.retry_delay = Some(d.0);
                }
            }
            Msg::Script(a) => {
                if let Some(e) = self.editor() {
                    e.script.perform(a);
                }
            }
            Msg::Urls(a) => {
                if let Some(e) = self.editor() {
                    e.urls.perform(a);
                }
            }
            Msg::Save => {
                let Some(e) = self.editor() else {
                    return Task::none();
                };
                match e.params() {
                    Err(err) => e.error = Some(err),
                    Ok(p) => {
                        e.busy = true;
                        let update = e.integration.is_some();
                        return cx.ctx.rest_task(
                            move |r| async move {
                                if update {
                                    r.integration_update(&p).await
                                } else {
                                    r.integration_create(&p).await
                                }
                            },
                            |r| Message::Integrations(Msg::Saved(r)),
                        );
                    }
                }
            }
            Msg::Saved(r) => {
                let text = r.as_ref().err().map(|f| err_text(f, cx.i18n));
                match r {
                    Ok(i) => {
                        cx.ok(t!("admin.integrations.saved").into_owned());
                        // Stay in the editor: the URL/token and a script error are shown now.
                        self.view = View::Edit(Box::new(Editor::of(i)));
                    }
                    Err(_) => {
                        if let Some(e) = self.editor() {
                            e.busy = false;
                            e.error = text;
                        }
                    }
                }
            }
            Msg::Copy(s) => {
                cx.ok(t!("admin.copied").into_owned());
                return iced::clipboard::write(s);
            }
            Msg::Delete => {
                if let Some(i) = self.editor().and_then(|e| e.integration.clone()) {
                    cx.ask(Confirm::danger(
                        t!("admin.integrations.delete_title").into_owned(),
                        t!(
                            "admin.integrations.delete_body",
                            name = i.name.clone().unwrap_or_default()
                        )
                        .into_owned(),
                        Message::Integrations(Msg::DeleteNow(i.kind, i.id)),
                    ));
                }
            }
            Msg::DeleteNow(kind, id) => {
                return cx.ctx.rest_task(
                    move |r| async move { r.integration_remove(kind, &id).await },
                    |r| Message::Integrations(Msg::Deleted(r)),
                );
            }
            Msg::Deleted(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.integrations.deleted").into_owned());
                    self.view = View::List;
                    return self.load(cx);
                }
            }
            Msg::History => {
                if let Some(id) = self
                    .editor()
                    .and_then(|e| e.integration.as_ref().map(|i| i.id.clone()))
                {
                    self.view = View::History(id, Box::new(List::new(Some(("_updatedAt", false)))));
                    return self.history(cx);
                }
            }
            Msg::HistoryList(m) => {
                let View::History(_, list) = &mut self.view else {
                    return Task::none();
                };
                return match list.update(m) {
                    Step::None => Task::none(),
                    Step::Wait(t) => t.map(|m| Message::Integrations(Msg::HistoryList(m))),
                    Step::Load => self.history(cx),
                };
            }
            Msg::HistoryLoaded(req, r) => {
                if let View::History(_, list) = &mut self.view {
                    list.finish(req, r.map_err(|f| err_text(&f, cx.i18n)));
                }
            }
            Msg::Replay(hid) => {
                if let View::History(id, _) = &self.view {
                    let id = id.clone();
                    return cx.ctx.rest_task(
                        move |r| async move { r.integration_replay(&id, &hid).await },
                        |r| Message::Integrations(Msg::Replayed(r)),
                    );
                }
            }
            Msg::Replayed(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.integrations.replayed").into_owned());
                    return self.history(cx);
                }
            }
            Msg::ClearHistory => cx.ask(Confirm::danger(
                t!("admin.integrations.clear_title").into_owned(),
                t!("admin.integrations.clear_body").into_owned(),
                Message::Integrations(Msg::ClearNow),
            )),
            Msg::ClearNow => {
                if let View::History(id, _) = &self.view {
                    let id = id.clone();
                    return cx.ctx.rest_task(
                        move |r| async move { r.integration_clear_history(&id).await },
                        |r| Message::Integrations(Msg::Cleared(r)),
                    );
                }
            }
            // `clearHistory` emits no stream event: refetch.
            Msg::Cleared(r) => {
                if cx.check(&r) {
                    return self.history(cx);
                }
            }
        }
        Task::none()
    }

    pub fn view<'a>(&'a self, v: &V<'a>) -> Element<'a, Message> {
        match &self.view {
            View::List => self.list_view(v),
            View::Edit(e) => editor_view(e, v),
            View::History(_, list) => history_view(list),
        }
    }

    fn list_view<'a>(&'a self, v: &V<'a>) -> Element<'a, Message> {
        let ws = v.ctx.workspace;
        let can_in = has_any_permission(
            ws,
            &[
                "manage-incoming-integrations",
                "manage-own-incoming-integrations",
            ],
            None,
        );
        let can_out = has_any_permission(
            ws,
            &[
                "manage-outgoing-integrations",
                "manage-own-outgoing-integrations",
            ],
            None,
        );
        let lm = |m| Message::Integrations(Msg::List(m));
        let mut filters = row![].spacing(SPACE_2);
        for (f, label) in [
            (None, t!("admin.integrations.all")),
            (
                Some(IntegrationType::Incoming),
                t!("admin.integrations.incoming"),
            ),
            (
                Some(IntegrationType::Outgoing),
                t!("admin.integrations.outgoing"),
            ),
        ] {
            filters = filters.push(
                button(text(label).size(TEXT_SMALL))
                    .padding([6, 12])
                    .style(if self.filter == f {
                        style::button_primary
                    } else {
                        style::button_secondary
                    })
                    .on_press(Message::Integrations(Msg::Filter(f))),
            );
        }
        let rows = self
            .list
            .items
            .iter()
            .map(|i| {
                let incoming = i.kind == IntegrationType::Incoming;
                (
                    vec![
                        row![
                            icon::sized(
                                if incoming {
                                    Icon::ArrowDownToLine
                                } else {
                                    Icon::ArrowUpFromLine
                                },
                                16.0
                            )
                            .style(style::text_secondary),
                            text(i.name.clone().unwrap_or_default())
                                .size(TEXT_BODY)
                                .font(FONT_MEDIUM),
                        ]
                        .spacing(SPACE_2)
                        .align_y(Alignment::Center)
                        .into(),
                        muted(if incoming {
                            t!("admin.integrations.incoming").into_owned()
                        } else {
                            i.event.map(event_label).unwrap_or_default()
                        }),
                        small(i.channel.join(", ")),
                        muted(
                            i.created_by
                                .as_ref()
                                .and_then(|u| u.username.clone())
                                .unwrap_or_default(),
                        ),
                        muted(date(i.created_at)),
                        yes_no(i.enabled),
                    ],
                    Some(Message::Integrations(Msg::Open(i.id.clone()))),
                )
            })
            .collect();
        let list = list_view(
            &self.list,
            move || {
                table(
                    vec![
                        sortable(t!("admin.integrations.name"), 3, "name"),
                        col(t!("admin.integrations.type"), 2),
                        col(t!("admin.integrations.channel"), 2),
                        col(t!("admin.integrations.created_by"), 2),
                        sortable(t!("admin.integrations.created"), 2, "_createdAt"),
                        col(t!("admin.integrations.enabled"), 1),
                    ],
                    self.list.sort,
                    |f| Message::Integrations(Msg::List(ListMsg::Sort(f))),
                    rows,
                )
            },
            lm,
            t!("admin.integrations.empty").into_owned(),
        );
        column![
            title_row(
                t!("admin.section.integrations").into_owned(),
                vec![
                    secondary_button(
                        t!("admin.integrations.new_incoming"),
                        can_in
                            .then_some(Message::Integrations(Msg::New(IntegrationType::Incoming)))
                    )
                    .icon(Icon::Plus)
                    .into(),
                    primary_button(
                        t!("admin.integrations.new_outgoing"),
                        can_out
                            .then_some(Message::Integrations(Msg::New(IntegrationType::Outgoing)))
                    )
                    .icon(Icon::Plus)
                    .into(),
                ]
            ),
            filters,
            search_bar(
                &self.list.query,
                t!("admin.search").into_owned(),
                move |s| lm(ListMsg::Search(s)),
                lm(ListMsg::Reload)
            ),
            list,
        ]
        .spacing(SPACE_4)
        .into()
    }
}

fn editor_view<'a>(e: &'a Editor, v: &V<'a>) -> Element<'a, Message> {
    let m = Message::Integrations;
    let fi = |f: F| Some(move |s| Message::Integrations(Msg::Field(f, s)));
    let fl = |f: Flag| Some(move |b| Message::Integrations(Msg::Flag(f, b)));
    let outgoing = e.outgoing();
    let ev = e.event();
    let title = match (&e.integration, outgoing) {
        (Some(i), _) => i.name.clone().unwrap_or_default(),
        (None, true) => t!("admin.integrations.new_outgoing").into_owned(),
        (None, false) => t!("admin.integrations.new_incoming").into_owned(),
    };
    let mut buttons: Vec<Element<'a, Message>> = Vec::new();
    if e.integration.is_some() {
        if outgoing {
            buttons.push(
                secondary_button(t!("admin.integrations.history"), Some(m(Msg::History)))
                    .icon(Icon::History)
                    .into(),
            );
        }
        buttons.push(
            danger_button(t!("admin.delete"), Some(m(Msg::Delete)))
                .icon(Icon::Trash2)
                .into(),
        );
    }
    buttons.push(
        primary_button(t!("admin.save"), (!e.busy).then_some(m(Msg::Save)))
            .loading(e.busy)
            .into(),
    );
    let mut col = column![back_row(title, m(Msg::Back), buttons)].spacing(SPACE_4);
    if let Some(err) = &e.error {
        col = col.push(banner(BannerKind::Error, err.clone(), None));
    }
    if let Some(i) = &e.integration {
        if let Some(se) = &i.script_error {
            col = col.push(banner(
                BannerKind::Warning,
                t!(
                    "admin.integrations.script_error",
                    error = format!(
                        "{}: {}",
                        se.name.clone().unwrap_or_default(),
                        se.message.clone().unwrap_or_default()
                    )
                )
                .into_owned(),
                None,
            ));
        }
        if !outgoing {
            let site = v
                .ctx
                .workspace
                .settings()
                .str("Site_Url")
                .unwrap_or(v.ctx.server.as_str())
                .trim_end_matches('/')
                .to_owned();
            if let Some(url) = i.webhook_url(&site) {
                let token = format!("{}/{}", i.id, i.token.clone().unwrap_or_default());
                let curl = format!(
                    "curl -X POST -H 'Content-Type: application/json' --data '{{\"text\":\"Example message\"}}' {url}"
                );
                let copyable = |label: String, value: String| {
                    field(
                        label,
                        None,
                        row![
                            readonly(value.clone()),
                            icon_button(
                                Icon::Copy,
                                Some(Message::Integrations(Msg::Copy(value))),
                                t!("admin.copy")
                            )
                        ]
                        .spacing(SPACE_2)
                        .align_y(Alignment::Center),
                    )
                };
                col = col.push(card(
                    t!("admin.integrations.webhook").into_owned(),
                    column![
                        copyable(t!("admin.integrations.webhook_url").into_owned(), url),
                        copyable(t!("admin.integrations.token").into_owned(), token),
                        copyable(t!("admin.integrations.curl").into_owned(), curl),
                    ]
                    .spacing(SPACE_3),
                ));
            }
        }
    }
    let mut form = column![switch(
        t!("admin.integrations.enabled").into_owned(),
        e.p.enabled,
        fl(Flag::Enabled)
    )]
    .spacing(SPACE_3);
    if outgoing {
        form = form.push(field(
            t!("admin.integrations.event").into_owned(),
            None,
            pick_list(OutgoingEvent::ALL.map(Event), Some(Event(ev)), |x| {
                Message::Integrations(Msg::Event(x))
            })
            .text_size(TEXT_BODY)
            .width(Length::Fill)
            .style(style::pick_list)
            .menu_style(style::menu),
        ));
    }
    form = form.push(field(
        t!("admin.integrations.name").into_owned(),
        None,
        input(&e.p.name, fi(F::Name)),
    ));
    if !outgoing || ev.uses_channel() {
        form = form.push(field(
            t!("admin.integrations.channel").into_owned(),
            Some(t!("admin.integrations.channel_hint").into_owned()),
            input(&e.p.channel, fi(F::Channel)),
        ));
    }
    if outgoing {
        if ev.uses_trigger_words() {
            form = form
                .push(field(
                    t!("admin.integrations.trigger_words").into_owned(),
                    Some(t!("admin.integrations.trigger_words_hint").into_owned()),
                    input(&e.trigger_words, fi(F::TriggerWords)),
                ))
                .push(switch(
                    t!("admin.integrations.trigger_anywhere").into_owned(),
                    e.p.trigger_word_anywhere.unwrap_or(false),
                    fl(Flag::TriggerAnywhere),
                ));
        }
        if ev.uses_target_room() {
            form = form.push(field(
                t!("admin.integrations.target_room").into_owned(),
                None,
                input(e.p.target_room.as_deref().unwrap_or(""), fi(F::TargetRoom)),
            ));
        }
        form = form
            .push(field(
                t!("admin.integrations.urls").into_owned(),
                Some(t!("admin.integrations.urls_hint").into_owned()),
                text_editor(&e.urls)
                    .on_action(|a| Message::Integrations(Msg::Urls(a)))
                    .height(80)
                    .size(TEXT_BODY)
                    .padding(SPACE_2),
            ))
            .push(switch(
                t!("admin.integrations.impersonate").into_owned(),
                e.p.impersonate_user.unwrap_or(false),
                fl(Flag::Impersonate),
            ));
    }
    form = form
        .push(field(
            t!("admin.integrations.post_as").into_owned(),
            Some(t!("admin.integrations.post_as_hint").into_owned()),
            input(&e.p.username, fi(F::Username)),
        ))
        .push(field(
            t!("admin.integrations.alias").into_owned(),
            None,
            input(e.p.alias.as_deref().unwrap_or(""), fi(F::Alias)),
        ))
        .push(field(
            t!("admin.integrations.avatar").into_owned(),
            None,
            input(e.p.avatar.as_deref().unwrap_or(""), fi(F::Avatar)),
        ))
        .push(field(
            t!("admin.integrations.emoji").into_owned(),
            None,
            input(e.p.emoji.as_deref().unwrap_or(""), fi(F::Emoji)),
        ));
    if outgoing {
        form = form.push(field(
            t!("admin.integrations.token").into_owned(),
            None,
            input(e.p.token.as_deref().unwrap_or(""), fi(F::Token)),
        ));
    } else {
        form = form.push(switch(
            t!("admin.integrations.override").into_owned(),
            e.p.override_destination_channel_enabled.unwrap_or(false),
            fl(Flag::Override),
        ));
    }
    form = form.push(switch(
        t!("admin.integrations.script_enabled").into_owned(),
        e.p.script_enabled,
        fl(Flag::Script),
    ));
    form = form.push(field(
        t!("admin.integrations.script").into_owned(),
        Some(t!("admin.integrations.script_hint").into_owned()),
        text_editor(&e.script)
            .on_action(|a| Message::Integrations(Msg::Script(a)))
            .height(280)
            .font(MONOSPACE_FONT)
            .size(TEXT_SMALL)
            .padding(SPACE_2),
    ));
    if outgoing {
        let retry = e.p.retry_failed_calls.unwrap_or(false);
        form = form
            .push(switch(
                t!("admin.integrations.run_on_edits").into_owned(),
                e.p.run_on_edits.unwrap_or(false),
                fl(Flag::RunOnEdits),
            ))
            .push(switch(
                t!("admin.integrations.retry").into_owned(),
                retry,
                fl(Flag::Retry),
            ));
        if retry {
            form = form
                .push(field(
                    t!("admin.integrations.retry_count").into_owned(),
                    None,
                    input(&e.retry_count, fi(F::RetryCount)),
                ))
                .push(field(
                    t!("admin.integrations.retry_delay").into_owned(),
                    None,
                    pick_list(
                        DELAYS,
                        Some(Delay(e.p.retry_delay.unwrap_or(RetryDelay::PowersOfTen))),
                        |d| Message::Integrations(Msg::Delay(d)),
                    )
                    .text_size(TEXT_BODY)
                    .width(Length::Fill)
                    .style(style::pick_list)
                    .menu_style(style::menu),
                ));
        }
    }
    col.push(container(form).max_width(760)).into()
}

fn history_view<'a>(list: &'a List<IntegrationHistory>) -> Element<'a, Message> {
    let m = Message::Integrations;
    let mut col = column![back_row(
        t!("admin.integrations.history").into_owned(),
        m(Msg::Back),
        vec![
            secondary_button(
                t!("admin.refresh"),
                Some(m(Msg::HistoryList(ListMsg::Reload)))
            )
            .icon(Icon::RefreshCw)
            .into(),
            danger_button(t!("admin.integrations.clear"), Some(m(Msg::ClearHistory))).into(),
        ]
    )]
    .spacing(SPACE_4);
    if list.items.is_empty() && list.loaded {
        return col.push(muted(t!("admin.integrations.no_history"))).into();
    }
    for h in &list.items {
        let json = |v: &Option<serde_json::Value>| {
            v.as_ref()
                .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
        };
        let mut c = column![
            kv(
                t!("admin.integrations.step").into_owned(),
                h.step.clone().unwrap_or_default()
            ),
            kv(
                t!("admin.integrations.event").into_owned(),
                h.event.clone().unwrap_or_default()
            ),
            kv("URL".into(), h.url.clone().unwrap_or_default()),
            kv(
                t!("admin.integrations.finished").into_owned(),
                h.finished.unwrap_or(false).to_string()
            ),
        ]
        .spacing(SPACE_2);
        for (label, v) in [
            (t!("admin.integrations.http_result"), json(&h.http_result)),
            (t!("admin.integrations.http_error"), json(&h.http_error)),
            (t!("admin.integrations.error"), json(&h.error)),
        ] {
            if let Some(v) = v {
                c = c.push(field(
                    label.into_owned(),
                    None,
                    readonly(v.chars().take(2000).collect()),
                ));
            }
        }
        c = c.push(ghost_button(
            t!("admin.integrations.replay"),
            Some(m(Msg::Replay(h.id.clone()))),
        ));
        col = col.push(card(date(h.created_at), c));
    }
    col = col.push(super::common::pager(list, |x| m(Msg::HistoryList(x))));
    col.into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn outgoing_params() {
        let mut e = Editor::new(IntegrationType::Outgoing, "admin");
        e.p.name = "hook".into();
        assert!(e.params().is_err(), "urls required");
        e.urls = text_editor::Content::with_text("https://a.example\n\n https://b.example ");
        e.trigger_words = "deploy, ship,".into();
        let p = e.params().unwrap();
        assert_eq!(p.urls.unwrap().len(), 2);
        assert_eq!(p.trigger_words.unwrap(), vec!["deploy", "ship"]);
        assert!(p.token.is_some_and(|t| t.len() >= 24));
        e.p.event = Some(OutgoingEvent::RoomCreated);
        e.p.channel = "#x".into();
        let p = e.params().unwrap();
        assert_eq!(p.channel, "");
        assert!(p.trigger_words.is_none());
    }

    #[test]
    fn incoming_needs_channel() {
        let mut e = Editor::new(IntegrationType::Incoming, "admin");
        e.p.name = "in".into();
        assert!(e.params().is_err());
        e.p.channel = "#general".into();
        e.script = text_editor::Content::with_text("class Script {}");
        let p = e.params().unwrap();
        assert_eq!(p.script.as_deref(), Some("class Script {}"));
    }
}
