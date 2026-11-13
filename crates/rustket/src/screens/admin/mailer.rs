//! Mailer (docs/admin.md §12) and e-mail inboxes (§16).

use iced::widget::{column, container, row, text, text_editor};
use iced::{Element, Length, Task};
use rocketchat::models::Paginated;
use rocketchat::models::admin::{EmailInbox, EmailInboxServer, MailerRequest};
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_SMALL, font_mono};
use rustket_ui::widget::{BannerKind, banner, danger_button, primary_button, secondary_button};

use super::common::{
    Confirm, Cx, List, ListMsg, Step, card, col, err_text, field, form_dialog, input, list_view,
    muted, small, switch, table, text_row, title_row, yes_no,
};
use super::{Message, V};
use crate::error::Res;

#[derive(Debug, Clone)]
pub enum Msg {
    From(String),
    Subject(String),
    Query(String),
    DryRun(bool),
    Body(text_editor::Action),
    Send,
    SendNow,
    Sent(Res<()>),
}

pub struct State {
    pub from: String,
    pub subject: String,
    pub query: String,
    pub dry_run: bool,
    pub body: text_editor::Content,
    pub busy: bool,
    pub error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            from: String::new(),
            subject: String::new(),
            query: String::new(),
            dry_run: true,
            body: text_editor::Content::with_text("\n\n[unsubscribe]"),
            busy: false,
            error: None,
        }
    }

    pub fn request(&self) -> Result<MailerRequest, String> {
        let req = MailerRequest {
            from: self.from.trim().to_owned(),
            subject: self.subject.trim().to_owned(),
            body: self.body.text(),
            dryrun: Some(self.dry_run),
            query: (!self.query.trim().is_empty()).then(|| self.query.trim().to_owned()),
        };
        if !req.from.contains('@') {
            return Err(t!("admin.mailer.bad_from").into_owned());
        }
        if req.subject.is_empty() {
            return Err(t!("admin.mailer.subject_required").into_owned());
        }
        if !req.has_unsubscribe() {
            return Err(t!("admin.mailer.unsubscribe_required").into_owned());
        }
        if let Some(q) = &req.query
            && serde_json::from_str::<serde_json::Value>(q).is_err()
        {
            return Err(t!("admin.settings.error.invalid_json").into_owned());
        }
        Ok(req)
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match m {
            Msg::From(s) => self.from = s,
            Msg::Subject(s) => self.subject = s,
            Msg::Query(s) => self.query = s,
            Msg::DryRun(b) => self.dry_run = b,
            Msg::Body(a) => self.body.perform(a),
            Msg::Send => match self.request() {
                Err(e) => self.error = Some(e),
                // A real send cannot be undone: confirm first.
                Ok(req) if req.dryrun != Some(true) => {
                    self.error = None;
                    let body = if req.query.is_some() {
                        t!("admin.mailer.confirm_query")
                    } else {
                        t!("admin.mailer.confirm_all")
                    };
                    cx.ask(Confirm {
                        title: t!("admin.mailer.confirm_title").into_owned(),
                        body: body.into_owned(),
                        danger: true,
                        yes_label: t!("admin.mailer.send").into_owned(),
                        yes: Box::new(Message::Mailer(Msg::SendNow)),
                    });
                }
                Ok(_) => return self.update(Msg::SendNow, cx),
            },
            Msg::SendNow => match self.request() {
                Err(e) => self.error = Some(e),
                Ok(req) => {
                    self.error = None;
                    self.busy = true;
                    return cx.ctx.rest_task(
                        move |r| async move { r.mailer_send(&req).await },
                        |r| Message::Mailer(Msg::Sent(r)),
                    );
                }
            },
            Msg::Sent(r) => {
                self.busy = false;
                match r {
                    Ok(()) => cx.ok(t!("admin.mailer.sent").into_owned()),
                    Err(f) => self.error = Some(err_text(&f, cx.i18n)),
                }
            }
        }
        Task::none()
    }

    pub fn view<'a>(&'a self, _v: &V<'a>) -> Element<'a, Message> {
        let m = Message::Mailer;
        let mut col = column![
            title_row(t!("admin.section.mailer").into_owned(), vec![]),
            text_row(
                t!("admin.mailer.from").into_owned(),
                &self.from,
                move |s| m(Msg::From(s))
            ),
            switch(
                t!("admin.mailer.dry_run").into_owned(),
                self.dry_run,
                Some(move |b| m(Msg::DryRun(b)))
            ),
            muted(t!("admin.mailer.dry_run_hint")),
            field(
                t!("admin.mailer.query").into_owned(),
                Some(t!("admin.mailer.query_hint").into_owned()),
                input(&self.query, Some(move |s| m(Msg::Query(s)))).font(font_mono())
            ),
            text_row(
                t!("admin.mailer.subject").into_owned(),
                &self.subject,
                move |s| m(Msg::Subject(s))
            ),
            field(
                t!("admin.mailer.body").into_owned(),
                Some(t!("admin.mailer.body_hint").into_owned()),
                text_editor(&self.body)
                    .on_action(move |a| m(Msg::Body(a)))
                    .height(240)
                    .size(TEXT_BODY)
                    .padding(SPACE_2)
            ),
        ]
        .spacing(SPACE_3);
        if let Some(e) = &self.error {
            col = col.push(banner(BannerKind::Error, e.clone(), None));
        }
        col = col.push(row![
            container(text("")).width(Length::Fill),
            primary_button(
                t!("admin.mailer.send"),
                (!self.busy).then_some(m(Msg::Send))
            )
            .icon(Icon::Send)
            .loading(self.busy)
        ]);
        col.into()
    }
}

// --- e-mail inboxes -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IF {
    Name,
    Email,
    Description,
    SenderInfo,
    Department,
    SmtpServer,
    SmtpPort,
    SmtpUser,
    SmtpPassword,
    ImapServer,
    ImapPort,
    ImapUser,
    ImapPassword,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IFlag {
    Active,
    SmtpSecure,
    ImapSecure,
}

#[derive(Debug, Clone)]
pub enum InboxMsg {
    List(ListMsg),
    Loaded(u64, Res<Paginated<EmailInbox>>),
    New,
    Open(Box<EmailInbox>),
    CloseDialog,
    Field(IF, String),
    Flag(IFlag, bool),
    Save,
    Duplicate(Res<Option<Box<EmailInbox>>>),
    Saved(Res<String>),
    SendTest,
    TestSent(Res<()>),
    Delete,
    DeleteNow(String),
    Deleted(Res<()>),
}

#[derive(Debug, Clone, Default)]
pub struct InboxForm {
    pub inbox: EmailInbox,
    pub smtp_port: String,
    pub imap_port: String,
    pub busy: bool,
    pub error: Option<String>,
}

impl InboxForm {
    fn of(inbox: EmailInbox) -> Self {
        Self {
            smtp_port: if inbox.smtp.port == 0 {
                String::new()
            } else {
                inbox.smtp.port.to_string()
            },
            imap_port: if inbox.imap.port == 0 {
                String::new()
            } else {
                inbox.imap.port.to_string()
            },
            inbox,
            busy: false,
            error: None,
        }
    }

    fn validate(&mut self) -> Result<(), String> {
        let i = &mut self.inbox;
        if i.name.trim().is_empty() || !i.email.contains('@') {
            return Err(t!("admin.inboxes.required").into_owned());
        }
        let port = |s: &str| s.trim().parse::<u16>().ok().filter(|p| *p > 0);
        i.smtp.port =
            port(&self.smtp_port).ok_or_else(|| t!("admin.inboxes.bad_port").into_owned())?;
        i.imap.port =
            port(&self.imap_port).ok_or_else(|| t!("admin.inboxes.bad_port").into_owned())?;
        if i.smtp.server.trim().is_empty() || i.imap.server.trim().is_empty() {
            return Err(t!("admin.inboxes.required").into_owned());
        }
        Ok(())
    }
}

pub struct Inboxes {
    pub list: List<EmailInbox>,
    pub form: Option<InboxForm>,
}

impl Default for Inboxes {
    fn default() -> Self {
        Self::new()
    }
}

impl Inboxes {
    pub fn new() -> Self {
        Self {
            list: List::new(Some(("name", true))),
            form: None,
        }
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let req = self.list.begin();
        let page = self.list.rest_page();
        cx.ctx.rest_task(
            move |r| async move { r.email_inbox_list(&page).await },
            move |r| Message::Inboxes(InboxMsg::Loaded(req, r)),
        )
    }

    pub fn update(&mut self, m: InboxMsg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let form = self.form.as_mut();
        match m {
            InboxMsg::List(m) => {
                return match self.list.update(m) {
                    Step::None => Task::none(),
                    Step::Wait(t) => t.map(|m| Message::Inboxes(InboxMsg::List(m))),
                    Step::Load => self.load(cx),
                };
            }
            InboxMsg::Loaded(req, r) => {
                if self.list.finish(req, r.map_err(|f| err_text(&f, cx.i18n))) {
                    return self.load(cx);
                }
            }
            InboxMsg::New => {
                self.form = Some(InboxForm::of(EmailInbox {
                    active: true,
                    smtp: EmailInboxServer {
                        port: 587,
                        ..EmailInboxServer::default()
                    },
                    imap: EmailInboxServer {
                        port: 993,
                        secure: true,
                        ..EmailInboxServer::default()
                    },
                    ..EmailInbox::default()
                }));
            }
            InboxMsg::Open(i) => self.form = Some(InboxForm::of(*i)),
            InboxMsg::CloseDialog => self.form = None,
            InboxMsg::Field(f, v) => {
                if let Some(form) = form {
                    form.error = None;
                    let i = &mut form.inbox;
                    let opt = |v: String| (!v.is_empty()).then_some(v);
                    match f {
                        IF::Name => i.name = v,
                        IF::Email => i.email = v,
                        IF::Description => i.description = opt(v),
                        IF::SenderInfo => i.sender_info = opt(v),
                        IF::Department => i.department = opt(v),
                        IF::SmtpServer => i.smtp.server = v,
                        IF::SmtpPort => form.smtp_port = v,
                        IF::SmtpUser => i.smtp.username = v,
                        IF::SmtpPassword => i.smtp.password = v,
                        IF::ImapServer => i.imap.server = v,
                        IF::ImapPort => form.imap_port = v,
                        IF::ImapUser => i.imap.username = v,
                        IF::ImapPassword => i.imap.password = v,
                    }
                }
            }
            InboxMsg::Flag(f, b) => {
                if let Some(form) = form {
                    match f {
                        IFlag::Active => form.inbox.active = b,
                        IFlag::SmtpSecure => form.inbox.smtp.secure = b,
                        IFlag::ImapSecure => form.inbox.imap.secure = b,
                    }
                }
            }
            InboxMsg::Save => {
                let Some(form) = form else {
                    return Task::none();
                };
                if let Err(e) = form.validate() {
                    form.error = Some(e);
                    return Task::none();
                }
                form.busy = true;
                // Warn about an address another inbox already uses.
                let email = form.inbox.email.trim().to_owned();
                return cx.ctx.rest_task(
                    move |r| async move { r.email_inbox_search(&email).await.map(|o| o.map(Box::new)) },
                    |r| Message::Inboxes(InboxMsg::Duplicate(r)),
                );
            }
            InboxMsg::Duplicate(r) => {
                let Some(form) = form else {
                    return Task::none();
                };
                if let Ok(Some(other)) = &r
                    && other.id != form.inbox.id
                {
                    form.busy = false;
                    form.error = Some(t!("admin.inboxes.duplicate").into_owned());
                    return Task::none();
                }
                let inbox = form.inbox.clone();
                return cx.ctx.rest_task(
                    move |r| async move { r.email_inbox_save(&inbox).await },
                    |r| Message::Inboxes(InboxMsg::Saved(r)),
                );
            }
            InboxMsg::Saved(r) => match r {
                Ok(_) => {
                    cx.ok(t!("admin.inboxes.saved").into_owned());
                    self.form = None;
                    return self.load(cx);
                }
                Err(e) => {
                    let text = err_text(&e, cx.i18n);
                    if let Some(f) = form {
                        f.busy = false;
                        f.error = Some(text);
                    }
                }
            },
            InboxMsg::SendTest => {
                if let Some(id) = form.and_then(|f| f.inbox.id.clone()) {
                    return cx.ctx.rest_task(
                        move |r| async move { r.email_inbox_send_test(&id).await },
                        |r| Message::Inboxes(InboxMsg::TestSent(r)),
                    );
                }
            }
            InboxMsg::TestSent(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.inboxes.test_sent").into_owned());
                }
            }
            InboxMsg::Delete => {
                if let Some(f) = form
                    && let Some(id) = f.inbox.id.clone()
                {
                    cx.ask(Confirm::danger(
                        t!("admin.inboxes.delete_title").into_owned(),
                        t!("admin.inboxes.delete_body", name = f.inbox.name.clone()).into_owned(),
                        Message::Inboxes(InboxMsg::DeleteNow(id)),
                    ));
                }
            }
            InboxMsg::DeleteNow(id) => {
                return cx.ctx.rest_task(
                    move |r| async move { r.email_inbox_delete(&id).await },
                    |r| Message::Inboxes(InboxMsg::Deleted(r)),
                );
            }
            InboxMsg::Deleted(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.inboxes.deleted").into_owned());
                    self.form = None;
                    return self.load(cx);
                }
            }
        }
        Task::none()
    }

    pub fn view<'a>(&'a self, _v: &V<'a>) -> Element<'a, Message> {
        let lm = |m| Message::Inboxes(InboxMsg::List(m));
        let rows = self
            .list
            .items
            .iter()
            .map(|i| {
                (
                    vec![
                        small(i.name.clone()),
                        small(i.email.clone()),
                        yes_no(i.active),
                    ],
                    Some(Message::Inboxes(InboxMsg::Open(Box::new(i.clone())))),
                )
            })
            .collect();
        let list = list_view(
            &self.list,
            move || {
                table(
                    vec![
                        col(t!("admin.inboxes.name"), 3),
                        col(t!("admin.inboxes.email"), 3),
                        col(t!("admin.inboxes.active"), 1),
                    ],
                    None,
                    |_| Message::Noop,
                    rows,
                )
            },
            lm,
            t!("admin.inboxes.empty").into_owned(),
        );
        column![
            title_row(
                t!("admin.section.email_inboxes").into_owned(),
                vec![
                    primary_button(
                        t!("admin.inboxes.new"),
                        Some(Message::Inboxes(InboxMsg::New))
                    )
                    .icon(Icon::Plus)
                    .into()
                ]
            ),
            list
        ]
        .spacing(SPACE_4)
        .into()
    }

    pub fn overlay<'a>(&'a self, _v: &V<'a>) -> Option<Element<'a, Message>> {
        let f = self.form.as_ref()?;
        let i = &f.inbox;
        let fi = |x: IF| move |s| Message::Inboxes(InboxMsg::Field(x, s));
        let fl = |x: IFlag| Some(move |b| Message::Inboxes(InboxMsg::Flag(x, b)));
        let server =
            |title: String, s: &'a EmailInboxServer, port: &'a str, ids: [IF; 4], secure: IFlag| {
                card(
                    title,
                    column![
                        text_row(
                            t!("admin.inboxes.server").into_owned(),
                            &s.server,
                            fi(ids[0])
                        ),
                        text_row(t!("admin.inboxes.port").into_owned(), port, fi(ids[1])),
                        text_row(
                            t!("admin.inboxes.username").into_owned(),
                            &s.username,
                            fi(ids[2])
                        ),
                        field(
                            t!("admin.password").into_owned(),
                            None,
                            input(&s.password, Some(fi(ids[3]))).secure(true)
                        ),
                        switch(
                            t!("admin.inboxes.secure").into_owned(),
                            s.secure,
                            fl(secure)
                        ),
                    ]
                    .spacing(SPACE_2),
                )
            };
        let body = column![
            switch(
                t!("admin.inboxes.active").into_owned(),
                i.active,
                fl(IFlag::Active)
            ),
            text_row(t!("admin.inboxes.name").into_owned(), &i.name, fi(IF::Name)),
            text_row(
                t!("admin.inboxes.email").into_owned(),
                &i.email,
                fi(IF::Email)
            ),
            text_row(
                t!("admin.inboxes.description").into_owned(),
                i.description.as_deref().unwrap_or(""),
                fi(IF::Description)
            ),
            text_row(
                t!("admin.inboxes.sender_info").into_owned(),
                i.sender_info.as_deref().unwrap_or(""),
                fi(IF::SenderInfo)
            ),
            text_row(
                t!("admin.inboxes.department").into_owned(),
                i.department.as_deref().unwrap_or(""),
                fi(IF::Department)
            ),
            server(
                "SMTP".into(),
                &i.smtp,
                &f.smtp_port,
                [IF::SmtpServer, IF::SmtpPort, IF::SmtpUser, IF::SmtpPassword],
                IFlag::SmtpSecure
            ),
            server(
                "IMAP".into(),
                &i.imap,
                &f.imap_port,
                [IF::ImapServer, IF::ImapPort, IF::ImapUser, IF::ImapPassword],
                IFlag::ImapSecure
            ),
        ]
        .spacing(SPACE_3);
        let extra: Vec<Element<'a, Message>> = if i.id.is_some() {
            vec![
                danger_button(t!("admin.delete"), Some(Message::Inboxes(InboxMsg::Delete)))
                    .icon(Icon::Trash2)
                    .into(),
                secondary_button(
                    t!("admin.inboxes.send_test"),
                    Some(Message::Inboxes(InboxMsg::SendTest)),
                )
                .into(),
            ]
        } else {
            vec![]
        };
        let _ = (TEXT_SMALL, style::text_secondary);
        Some(form_dialog(
            if i.id.is_some() {
                t!("admin.inboxes.edit").into_owned()
            } else {
                t!("admin.inboxes.new").into_owned()
            },
            container(body).width(Length::Fill).into(),
            f.error.as_deref(),
            f.busy,
            Some(Message::Inboxes(InboxMsg::Save)),
            Message::Inboxes(InboxMsg::CloseDialog),
            extra,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailer_checks() {
        let mut s = State::new();
        s.from = "admin@x.io".into();
        s.subject = "Hi".into();
        assert!(s.request().is_ok(), "the default body has [unsubscribe]");
        s.query = "{bad".into();
        assert!(s.request().is_err());
        s.query = r#"{"roles":"admin"}"#.into();
        s.body = text_editor::Content::with_text("no link");
        assert!(s.request().is_err());
    }
}
