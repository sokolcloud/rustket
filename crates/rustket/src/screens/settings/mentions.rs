//! The Mentions tab: per signed-in account, its mention groups
//! (`rustket_core::mention_groups`, synced through its server) with an
//! inline editor. Before a group is saved its name and members are checked
//! on the server: the app runs the lookups ([`Effect::CheckMentionGroup`])
//! and answers with [`Message::Checked`]. An opened group is checked again
//! right away, so members renamed or deleted on the server show up; its
//! stored members were checked when it was saved, so a save looks up only
//! the members added since (and the ones found missing).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Element, Length, Task};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::mention_groups::{
    self, MAX_GROUP_MEMBERS, MAX_GROUP_NAME_CHARS, MentionGroupEdit, MentionGroupError,
    MentionGroups, clean_group_name, clean_username,
};
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, font_semibold};
use rustket_ui::widget::{
    danger_button, divider, ghost_button, icon_button_with, primary_button, secondary_button,
    text_field,
};

use super::widgets::section;
use super::{Message as SettingsMessage, Settings, SettingsView};
use crate::component::{Action, Effect};
use crate::workspace::service_sync::Status;

/// Id of the editor's name field.
pub const NAME_ID: &str = "settings-mention-group-name";
/// Id of the editor's member field.
pub const MEMBER_ID: &str = "settings-mention-group-member";
/// Wait after typing in the member field before asking the server.
const SUGGEST_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
/// Suggestions shown under the member field.
const MAX_SUGGESTIONS: usize = 6;
/// Members of an opened group looked up again at most (lookups are rate
/// limited per route, by default to 10 a minute).
pub const MAX_RECHECK: usize = 8;
/// Members named in a group's row.
const PREVIEW_MEMBERS: usize = 6;
/// Numbers the checks: unique for the process, so an answer meant for the
/// editor of an earlier Settings screen never matches a later one.
static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u64 {
    NEXT_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// A signed-in account's mention groups, for the tab.
pub struct MentionAccount<'a> {
    pub sid: ServerId,
    /// "alice on Company".
    pub label: String,
    pub groups: &'a MentionGroups,
    pub status: Status,
    /// Connected: names can be checked, so groups can be saved.
    pub online: bool,
}

/// Why a group cannot be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    Name,
    Reserved,
    /// The user's own username.
    Own,
    Duplicate,
    /// A user on the server has the name.
    User,
    /// A team on the server has the name.
    Team,
    Empty,
    TooMany,
    Full,
    TooLarge,
    Offline,
}

impl Issue {
    pub fn from_error(e: &MentionGroupError) -> Self {
        match e {
            MentionGroupError::InvalidId | MentionGroupError::InvalidName => Self::Name,
            MentionGroupError::Reserved => Self::Reserved,
            MentionGroupError::Duplicate => Self::Duplicate,
            MentionGroupError::Empty => Self::Empty,
            MentionGroupError::TooManyMembers => Self::TooMany,
            MentionGroupError::Full => Self::Full,
            MentionGroupError::TooLarge => Self::TooLarge,
        }
    }

    /// The text shown under the editor (`name`: the group name typed).
    pub fn text(&self, name: &str) -> String {
        match self {
            Self::Name => t!(
                "mention_groups.err_name",
                min = mention_groups::MIN_GROUP_NAME_CHARS,
                max = MAX_GROUP_NAME_CHARS
            ),
            Self::Reserved => t!("mention_groups.err_reserved", name = name),
            Self::Own => t!("mention_groups.err_own"),
            Self::Duplicate => t!("mention_groups.err_duplicate"),
            Self::User => t!("mention_groups.err_user", name = name),
            Self::Team => t!("mention_groups.err_team", name = name),
            Self::Empty => t!("mention_groups.err_empty"),
            Self::TooMany => t!("mention_groups.err_too_many", max = MAX_GROUP_MEMBERS),
            Self::Full => t!("mention_groups.err_full", max = mention_groups::MAX_GROUPS),
            Self::TooLarge => t!("mention_groups.err_too_large"),
            Self::Offline => t!("mention_groups.offline"),
        }
        .into_owned()
    }
}

/// Checks the app runs on the server for the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCheck {
    /// Matches the answer to the editor's latest request.
    pub seq: u64,
    /// The group edited (`None`: a new one).
    pub id: Option<String>,
    /// The group name, cleaned.
    pub name: String,
    /// Whether to check that no user or team has the name.
    pub check_name: bool,
    /// Usernames to look up.
    pub lookup: Vec<String>,
    /// The members to save once every check passed (`None`: only check).
    pub save: Option<Vec<String>>,
}

/// The app's answer to a [`GroupCheck`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckReport {
    pub issue: Option<Issue>,
    /// Usernames found.
    pub found: Vec<String>,
    /// Usernames the server does not know.
    pub missing: Vec<String>,
    /// A lookup failed for another reason: its text.
    pub failed: Option<String>,
    /// The group was saved.
    pub saved: bool,
}

/// A user offered under the member field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub username: String,
    pub name: Option<String>,
}

/// The group being created or edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub sid: ServerId,
    /// The group edited (`None`: a new one).
    pub id: Option<String>,
    /// The name when the editor opened.
    original: Option<String>,
    pub name: String,
    /// The member field.
    pub input: String,
    pub members: Vec<String>,
    /// Lowercase usernames known to exist (picked from suggestions, found,
    /// or stored in the opened group).
    verified: BTreeSet<String>,
    /// Lowercase usernames the server does not know.
    missing: BTreeSet<String>,
    pub suggestions: Vec<Suggestion>,
    seq: u64,
    suggest_seq: u64,
    /// A save waits for its checks.
    pub checking: bool,
    pub issue: Option<Issue>,
    pub error: Option<String>,
}

impl Draft {
    fn new(sid: ServerId, id: Option<String>, name: String, members: Vec<String>) -> Self {
        // A stored group's members were looked up when it was saved.
        let verified = match id {
            Some(_) => members.iter().map(|m| m.to_lowercase()).collect(),
            None => BTreeSet::new(),
        };
        Self {
            sid,
            original: id.as_ref().map(|_| name.clone()),
            id,
            name,
            input: String::new(),
            members,
            verified,
            missing: BTreeSet::new(),
            suggestions: Vec::new(),
            seq: 0,
            suggest_seq: 0,
            checking: false,
            issue: None,
            error: None,
        }
    }

    pub fn is_missing(&self, username: &str) -> bool {
        self.missing.contains(&username.to_lowercase())
    }

    fn has(&self, username: &str) -> bool {
        self.members
            .iter()
            .any(|m| m.eq_ignore_ascii_case(username))
    }

    /// Adds a member; `verified`: known to exist.
    fn add(&mut self, username: &str, verified: bool) {
        let Some(u) = clean_username(username) else {
            return;
        };
        if verified {
            self.verified.insert(u.to_lowercase());
        }
        if !self.has(&u) {
            self.members.push(u);
        }
        self.input.clear();
        self.suggestions.clear();
        self.suggest_seq += 1;
        self.issue = None;
    }

    /// What can be told without the server: the cleaned name, or the issue.
    fn local_check(&self) -> Result<String, Issue> {
        let name = clean_group_name(&self.name).ok_or(Issue::Name)?;
        if mention_groups::RESERVED.contains(&name.as_str()) {
            return Err(Issue::Reserved);
        }
        if self.members.is_empty() {
            return Err(Issue::Empty);
        }
        if self.members.len() > MAX_GROUP_MEMBERS {
            return Err(Issue::TooMany);
        }
        Ok(name)
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    New(ServerId),
    /// Open a group in the editor; `online`: check it on the server again.
    Edit {
        sid: ServerId,
        id: String,
        name: String,
        members: Vec<String>,
        online: bool,
    },
    Name(String),
    /// Enter in the name field.
    NameSubmit,
    MemberInput(String),
    /// Enter in the member field: add what was typed.
    AddMember,
    /// A suggestion was picked.
    Pick(String),
    RemoveMember(String),
    SuggestTick(u64),
    Suggestions(u64, Result<Vec<Suggestion>, String>),
    Save,
    Cancel,
    Checked(u64, CheckReport),
    AskDelete(ServerId, String),
    ConfirmDelete,
    CancelDelete,
}

impl From<Message> for SettingsMessage {
    fn from(m: Message) -> Self {
        Self::Mentions(m)
    }
}

impl Settings {
    pub(super) fn update_mentions(&mut self, msg: Message) -> Action<SettingsMessage> {
        match msg {
            Message::New(sid) => {
                self.mention_draft = Some(Draft::new(sid, None, String::new(), Vec::new()));
                self.confirm_mention_delete = None;
                Action::task(iced::widget::operation::focus(iced::widget::Id::new(
                    NAME_ID,
                )))
            }
            Message::Edit {
                sid,
                id,
                name,
                members,
                online,
            } => {
                let mut d = Draft::new(sid.clone(), Some(id.clone()), name.clone(), members);
                self.confirm_mention_delete = None;
                if !online {
                    self.mention_draft = Some(d);
                    return Action::none();
                }
                d.seq = next_seq();
                let check = GroupCheck {
                    seq: d.seq,
                    id: Some(id),
                    name,
                    check_name: true,
                    lookup: d.members.iter().take(MAX_RECHECK).cloned().collect(),
                    save: None,
                };
                self.mention_draft = Some(d);
                Action::effect(Effect::CheckMentionGroup(sid, Box::new(check)))
            }
            Message::Name(s) => {
                if let Some(d) = &mut self.mention_draft {
                    // Room for a leading `@`.
                    d.name = s.chars().take(MAX_GROUP_NAME_CHARS + 1).collect();
                    d.issue = None;
                }
                Action::none()
            }
            Message::NameSubmit => match &self.mention_draft {
                Some(d) if !d.members.is_empty() => self.save_mention_group(),
                Some(_) => Action::task(iced::widget::operation::focus(iced::widget::Id::new(
                    MEMBER_ID,
                ))),
                None => Action::none(),
            },
            Message::MemberInput(s) => {
                let Some(d) = &mut self.mention_draft else {
                    return Action::none();
                };
                d.input = s;
                d.suggest_seq += 1;
                if d.input.trim().trim_start_matches('@').is_empty() {
                    d.suggestions.clear();
                    return Action::none();
                }
                let seq = d.suggest_seq;
                Action::task(Task::perform(
                    async { tokio::time::sleep(SUGGEST_DELAY).await },
                    move |_| SettingsMessage::Mentions(Message::SuggestTick(seq)),
                ))
            }
            Message::SuggestTick(seq) => {
                let Some(d) = self.mention_draft.as_ref().filter(|d| d.suggest_seq == seq) else {
                    return Action::none();
                };
                let term = d.input.trim().trim_start_matches('@').to_owned();
                if term.is_empty() {
                    return Action::none();
                }
                Action::effect(Effect::SuggestMentionMembers(d.sid.clone(), seq, term))
            }
            Message::Suggestions(seq, res) => {
                if let Some(d) = self.mention_draft.as_mut().filter(|d| d.suggest_seq == seq) {
                    let list = res.unwrap_or_default();
                    d.suggestions = list
                        .into_iter()
                        .filter(|s| {
                            !d.members
                                .iter()
                                .any(|m| m.eq_ignore_ascii_case(&s.username))
                        })
                        .take(MAX_SUGGESTIONS)
                        .collect();
                }
                Action::none()
            }
            Message::AddMember => {
                if let Some(d) = &mut self.mention_draft {
                    let typed = d.input.clone();
                    d.add(&typed, false);
                }
                Action::none()
            }
            Message::Pick(u) => {
                if let Some(d) = &mut self.mention_draft {
                    d.add(&u, true);
                }
                Action::task(iced::widget::operation::focus(iced::widget::Id::new(
                    MEMBER_ID,
                )))
            }
            Message::RemoveMember(u) => {
                if let Some(d) = &mut self.mention_draft {
                    d.members.retain(|m| *m != u);
                    d.missing.remove(&u.to_lowercase());
                    d.issue = None;
                }
                Action::none()
            }
            Message::Save => self.save_mention_group(),
            Message::Cancel => {
                self.mention_draft = None;
                Action::none()
            }
            Message::Checked(seq, report) => {
                let Some(d) = self.mention_draft.as_mut().filter(|d| d.seq == seq) else {
                    return Action::none();
                };
                if report.saved {
                    self.mention_draft = None;
                    return Action::none();
                }
                d.checking = false;
                for u in &report.found {
                    d.missing.remove(&u.to_lowercase());
                    d.verified.insert(u.to_lowercase());
                }
                for u in &report.missing {
                    d.verified.remove(&u.to_lowercase());
                    d.missing.insert(u.to_lowercase());
                }
                d.issue = report.issue;
                d.error = report.failed;
                Action::none()
            }
            Message::AskDelete(sid, id) => {
                self.confirm_mention_delete = Some((sid, id));
                Action::none()
            }
            Message::ConfirmDelete => {
                let Some((sid, id)) = self.confirm_mention_delete.take() else {
                    return Action::none();
                };
                if self
                    .mention_draft
                    .as_ref()
                    .is_some_and(|d| d.sid == sid && d.id.as_deref() == Some(id.as_str()))
                {
                    self.mention_draft = None;
                }
                Action::effect(Effect::EditMentionGroup(
                    sid,
                    MentionGroupEdit::Delete { id },
                ))
            }
            Message::CancelDelete => {
                self.confirm_mention_delete = None;
                Action::none()
            }
        }
    }

    /// Save: what can be told locally first, then the server's checks (the
    /// app saves the group when they pass).
    fn save_mention_group(&mut self) -> Action<SettingsMessage> {
        let Some(d) = self.mention_draft.as_mut().filter(|d| !d.checking) else {
            return Action::none();
        };
        let name = match d.local_check() {
            Ok(n) => n,
            Err(issue) => {
                d.issue = Some(issue);
                return Action::none();
            }
        };
        let seq = next_seq();
        d.seq = seq;
        d.checking = true;
        d.issue = None;
        d.error = None;
        let lookup = d
            .members
            .iter()
            .filter(|m| !d.verified.contains(&m.to_lowercase()))
            .cloned()
            .collect();
        let check = GroupCheck {
            seq,
            id: d.id.clone(),
            check_name: d.original.as_deref() != Some(name.as_str()),
            name,
            lookup,
            save: Some(d.members.clone()),
        };
        Action::effect(Effect::CheckMentionGroup(d.sid.clone(), Box::new(check)))
    }
}

/// The Mentions tab's card.
pub(super) fn view<'a>(s: &'a Settings, v: &SettingsView<'a>) -> Vec<Element<'a, SettingsMessage>> {
    let mut rows: Vec<Element<'a, SettingsMessage>> = vec![
        text(t!("mention_groups.caption"))
            .size(TEXT_CAPTION)
            .style(style::text_secondary)
            .into(),
    ];
    if v.mention_groups.is_empty() {
        rows.push(
            text(t!("mention_groups.signed_out"))
                .size(TEXT_CAPTION)
                .style(style::text_hint)
                .into(),
        );
    }
    for a in &v.mention_groups {
        rows.push(account_block(s, a).map(SettingsMessage::Mentions));
    }
    vec![section(t!("mention_groups.title").into_owned(), rows)]
}

fn account_block<'a>(s: &'a Settings, a: &MentionAccount<'a>) -> Element<'a, Message> {
    let mut col = column![
        text(a.label.clone())
            .size(TEXT_BODY)
            .style(style::text_secondary)
    ]
    .spacing(SPACE_2);
    let status = match &a.status {
        Status::Synced => None,
        Status::Pending => Some((t!("mention_groups.pending").into_owned(), false)),
        Status::Failed(e) => Some((t!("mention_groups.failed", error = e).into_owned(), true)),
    };
    if let Some((line, failed)) = status {
        col = col.push(text(line).size(TEXT_CAPTION).style(if failed {
            style::text_danger
        } else {
            style::text_hint
        }));
    }
    let draft = s.mention_draft.as_ref().filter(|d| d.sid == a.sid);
    if a.groups.is_empty() && draft.is_none() {
        col = col.push(
            text(t!("mention_groups.empty"))
                .size(TEXT_CAPTION)
                .style(style::text_hint),
        );
    }
    for g in a.groups.all() {
        let editing = draft.is_some_and(|d| d.id.as_deref() == Some(g.id.as_str()));
        col = col.push(divider());
        if let Some(d) = draft.filter(|_| editing) {
            col = col.push(editor(d, a.online));
            continue;
        }
        col = col.push(group_row(s, a, g));
    }
    // A new group, or one deleted elsewhere while it was being edited.
    let listed = |d: &Draft| {
        d.id.as_deref()
            .is_some_and(|id| a.groups.by_id(id).is_some())
    };
    match draft.filter(|d| !listed(d)) {
        Some(d) => {
            col = col.push(divider()).push(editor(d, a.online));
        }
        None => {
            col = col.push(row![secondary_button(
                t!("mention_groups.new"),
                Some(Message::New(a.sid.clone()))
            )]);
        }
    }
    container(col).width(Length::Fill).into()
}

fn group_row<'a>(
    s: &'a Settings,
    a: &MentionAccount<'a>,
    g: &'a mention_groups::MentionGroup,
) -> Element<'a, Message> {
    let count = g.members.len() as u64;
    let mut names: Vec<String> = g
        .members
        .iter()
        .take(PREVIEW_MEMBERS)
        .map(|m| format!("@{m}"))
        .collect();
    if g.members.len() > PREVIEW_MEMBERS {
        names.push("…".into());
    }
    let head = row![
        text(format!("@{}", g.name))
            .size(TEXT_BODY)
            .font(font_semibold()),
        text(
            t!(
                format!(
                    "mention_groups.count_{}",
                    crate::room::render::plural(count)
                ),
                count = count
            )
            .into_owned()
        )
        .size(TEXT_CAPTION)
        .style(style::text_secondary),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    let edit = Message::Edit {
        sid: a.sid.clone(),
        id: g.id.clone(),
        name: g.name.clone(),
        members: g.members.clone(),
        online: a.online,
    };
    let mut info = column![
        head,
        text(names.join(" "))
            .size(TEXT_CAPTION)
            .style(style::text_secondary),
    ]
    .spacing(SPACE_1)
    .width(Length::Fill);
    if g.shadowed {
        info = info.push(
            text(t!("mention_groups.shadowed"))
                .size(TEXT_CAPTION)
                .style(style::text_danger),
        );
    }
    let r = row![
        info,
        ghost_button(t!("mention_groups.edit"), Some(edit)),
        ghost_button(
            t!("mention_groups.delete"),
            Some(Message::AskDelete(a.sid.clone(), g.id.clone()))
        ),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    let mut col = column![r].spacing(SPACE_2);
    let asked = s
        .confirm_mention_delete
        .as_ref()
        .is_some_and(|(sid, id)| *sid == a.sid && *id == g.id);
    if asked {
        col = col.push(
            column![
                text(t!("mention_groups.confirm_delete", name = g.name.as_str())).size(TEXT_BODY),
                row![
                    danger_button(t!("mention_groups.delete"), Some(Message::ConfirmDelete)),
                    secondary_button(t!("settings.group_cancel"), Some(Message::CancelDelete)),
                ]
                .spacing(SPACE_2),
            ]
            .spacing(SPACE_2),
        );
    }
    col.into()
}

fn editor<'a>(d: &'a Draft, online: bool) -> Element<'a, Message> {
    let shown_name = d.name.trim().trim_start_matches('@').to_lowercase();
    let name_field = text_field(t!("mention_groups.name"), &d.name, Message::Name)
        .placeholder(t!("mention_groups.name_hint"))
        .id(NAME_ID)
        .width(Length::Fixed(280.0))
        .on_submit(Message::NameSubmit);
    let mut col = column![Element::from(name_field)].spacing(SPACE_2);
    if let Some(issue) = &d.issue {
        col = col.push(
            text(issue.text(&shown_name))
                .size(TEXT_CAPTION)
                .style(style::text_danger),
        );
    }
    col = col.push(text(t!("mention_groups.members")).size(TEXT_BODY));
    let mut chips = row![].spacing(SPACE_2).align_y(Alignment::Center);
    for m in &d.members {
        let missing = d.is_missing(m);
        let label = text(format!("@{m}")).size(TEXT_CAPTION).style(if missing {
            style::text_danger
        } else {
            style::text_primary
        });
        let chip = row![
            label,
            icon_button_with(
                Icon::X,
                Some(Message::RemoveMember(m.clone())),
                t!("mention_groups.remove_member"),
                false,
                20.0
            ),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center);
        chips = chips.push(
            container(chip)
                .padding([0.0, SPACE_2])
                .style(style::container_mention(false)),
        );
    }
    if !d.members.is_empty() {
        col = col.push(chips.wrap().vertical_spacing(SPACE_2));
    }
    let missing: Vec<String> = d
        .members
        .iter()
        .filter(|m| d.is_missing(m))
        .map(|m| format!("@{m}"))
        .collect();
    if !missing.is_empty() {
        col = col.push(
            text(t!("mention_groups.err_missing", names = missing.join(", ")))
                .size(TEXT_CAPTION)
                .style(style::text_danger),
        );
    }
    let full = d.members.len() >= MAX_GROUP_MEMBERS;
    let member_field = text_field("", &d.input, Message::MemberInput)
        .placeholder(t!("mention_groups.members_hint"))
        .id(MEMBER_ID)
        .width(Length::Fixed(280.0))
        .disabled(full)
        .on_submit(Message::AddMember);
    col = col.push(Element::from(member_field));
    if !d.suggestions.is_empty() {
        let mut list = column![].spacing(0);
        for sg in &d.suggestions {
            let mut r = row![text(format!("@{}", sg.username)).size(TEXT_BODY)]
                .spacing(SPACE_2)
                .align_y(Alignment::Center);
            if let Some(n) = &sg.name {
                r = r.push(
                    text(n.clone())
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary),
                );
            }
            list = list.push(
                button(r)
                    .width(Length::Fill)
                    .padding([SPACE_1, SPACE_2])
                    .style(style::button_list_item(false))
                    .on_press(Message::Pick(sg.username.clone())),
            );
        }
        col = col.push(
            container(list)
                .width(Length::Fixed(280.0))
                .style(style::container_popover),
        );
    }
    let note = if d.checking {
        Some((t!("mention_groups.checking").into_owned(), false))
    } else if let Some(e) = &d.error {
        Some((t!("mention_groups.err_check", error = e).into_owned(), true))
    } else if !online {
        Some((t!("mention_groups.offline").into_owned(), false))
    } else {
        None
    };
    if let Some((line, failed)) = note {
        col = col.push(text(line).size(TEXT_CAPTION).style(if failed {
            style::text_danger
        } else {
            style::text_hint
        }));
    }
    let can_save = online && !d.checking && !d.members.is_empty();
    col = col.push(
        row![
            primary_button(t!("settings.group_save"), can_save.then_some(Message::Save)),
            secondary_button(t!("settings.group_cancel"), Some(Message::Cancel)),
        ]
        .spacing(SPACE_2),
    );
    container(col.spacing(SPACE_3)).width(Length::Fill).into()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn sid() -> ServerId {
        ServerId("s1".into())
    }

    fn effects(a: Action<SettingsMessage>) -> Vec<Effect> {
        a.effects
    }

    fn settings() -> Settings {
        Settings::with_autostart(None)
    }

    #[test]
    fn save_checks_locally_then_asks_the_app() {
        let mut s = settings();
        let _ = s.update_mentions(Message::New(sid()));
        let _ = s.update_mentions(Message::Name("x".into()));
        let _ = s.update_mentions(Message::MemberInput("bob".into()));
        let _ = s.update_mentions(Message::AddMember);
        assert!(effects(s.update_mentions(Message::Save)).is_empty());
        assert_eq!(s.mention_draft.as_ref().unwrap().issue, Some(Issue::Name));
        let _ = s.update_mentions(Message::Name("@All".into()));
        assert!(effects(s.update_mentions(Message::Save)).is_empty());
        assert_eq!(
            s.mention_draft.as_ref().unwrap().issue,
            Some(Issue::Reserved)
        );
        let _ = s.update_mentions(Message::Name("@Backend".into()));
        let _ = s.update_mentions(Message::Pick("alice".into()));
        let e = effects(s.update_mentions(Message::Save));
        let [Effect::CheckMentionGroup(sid_, check)] = e.as_slice() else {
            panic!("{e:?}");
        };
        assert_eq!(*sid_, sid());
        assert_eq!(check.name, "backend");
        assert!(check.check_name, "a new group's name is checked");
        assert_eq!(check.lookup, ["bob"], "picked members need no lookup");
        assert_eq!(
            check.save.as_deref(),
            Some(&["bob".to_owned(), "alice".to_owned()][..])
        );
        // Checking: a second Save does nothing.
        assert!(effects(s.update_mentions(Message::Save)).is_empty());
        // A stale answer is dropped; the current one shows the issue.
        let seq = check.seq;
        let stale = CheckReport {
            saved: true,
            ..Default::default()
        };
        let _ = s.update_mentions(Message::Checked(seq - 1, stale));
        assert!(s.mention_draft.is_some());
        let report = CheckReport {
            issue: Some(Issue::User),
            found: vec!["bob".into()],
            ..Default::default()
        };
        let _ = s.update_mentions(Message::Checked(seq, report));
        let d = s.mention_draft.as_ref().unwrap();
        assert!(!d.checking);
        assert_eq!(d.issue, Some(Issue::User));
        // Rename and save again: bob is verified now.
        let _ = s.update_mentions(Message::Name("devs".into()));
        let e = effects(s.update_mentions(Message::Save));
        let [Effect::CheckMentionGroup(_, check)] = e.as_slice() else {
            panic!("{e:?}");
        };
        assert!(check.lookup.is_empty());
        let saved = CheckReport {
            saved: true,
            ..Default::default()
        };
        let _ = s.update_mentions(Message::Checked(check.seq, saved));
        assert!(s.mention_draft.is_none(), "saved: the editor closes");
    }

    #[test]
    fn an_opened_group_is_checked_again_and_shows_missing_members() {
        let mut s = settings();
        let open = |online| Message::Edit {
            sid: sid(),
            id: "g1".into(),
            name: "backend".into(),
            members: vec!["alice".into(), "gone".into()],
            online,
        };
        assert!(effects(s.update_mentions(open(false))).is_empty());
        let e = effects(s.update_mentions(open(true)));
        let [Effect::CheckMentionGroup(_, check)] = e.as_slice() else {
            panic!("{e:?}");
        };
        assert_eq!(check.save, None);
        assert!(check.check_name);
        assert_eq!(check.lookup, ["alice", "gone"]);
        let report = CheckReport {
            found: vec!["alice".into()],
            missing: vec!["gone".into()],
            ..Default::default()
        };
        let _ = s.update_mentions(Message::Checked(check.seq, report));
        let d = s.mention_draft.as_ref().unwrap();
        assert!(d.is_missing("gone") && !d.is_missing("alice"));
        // Saving with the unchanged name looks up nobody again but gone.
        let e = effects(s.update_mentions(Message::Save));
        let [Effect::CheckMentionGroup(_, check)] = e.as_slice() else {
            panic!("{e:?}");
        };
        assert!(!check.check_name);
        assert_eq!(check.lookup, ["gone"]);
        let _ = s.update_mentions(Message::RemoveMember("gone".into()));
        assert!(!s.mention_draft.as_ref().unwrap().is_missing("gone"));
    }

    /// Review: a save looked up every stored member again, one by one,
    /// and ran into the `users.info` rate limit with larger groups.
    #[test]
    fn a_save_looks_up_only_members_added_since_the_group_opened() {
        let mut s = settings();
        let members: Vec<String> = (0..20).map(|i| format!("u{i}")).collect();
        let _ = s.update_mentions(Message::Edit {
            sid: sid(),
            id: "g1".into(),
            name: "backend".into(),
            members,
            online: false,
        });
        let _ = s.update_mentions(Message::RemoveMember("u3".into()));
        let _ = s.update_mentions(Message::MemberInput("newbie".into()));
        let _ = s.update_mentions(Message::AddMember);
        let e = effects(s.update_mentions(Message::Save));
        let [Effect::CheckMentionGroup(_, check)] = e.as_slice() else {
            panic!("{e:?}");
        };
        assert_eq!(check.lookup, ["newbie"]);
        assert_eq!(check.save.as_ref().unwrap().len(), 20);
        assert!(!check.check_name);
    }

    /// Review: check numbers restarted with every Settings screen, so an
    /// answer for an earlier screen's editor could close a later one.
    #[test]
    fn answers_for_an_earlier_settings_screen_are_dropped() {
        let mut old = settings();
        let _ = old.update_mentions(Message::New(sid()));
        let _ = old.update_mentions(Message::Name("devs".into()));
        let _ = old.update_mentions(Message::Pick("alice".into()));
        let e = effects(old.update_mentions(Message::Save));
        let [Effect::CheckMentionGroup(_, first)] = e.as_slice() else {
            panic!("{e:?}");
        };
        let mut s = settings();
        let e = effects(s.update_mentions(Message::Edit {
            sid: sid(),
            id: "g2".into(),
            name: "qa".into(),
            members: vec!["bob".into()],
            online: true,
        }));
        let [Effect::CheckMentionGroup(_, second)] = e.as_slice() else {
            panic!("{e:?}");
        };
        assert_ne!(first.seq, second.seq);
        let saved = CheckReport {
            saved: true,
            ..Default::default()
        };
        let _ = s.update_mentions(Message::Checked(first.seq, saved));
        assert!(s.mention_draft.is_some(), "the qa editor stays open");
    }

    #[test]
    fn enter_in_the_name_field_moves_on_to_the_members() {
        let mut s = settings();
        let _ = s.update_mentions(Message::New(sid()));
        let _ = s.update_mentions(Message::Name("devs".into()));
        let a = s.update_mentions(Message::NameSubmit);
        assert!(a.effects.is_empty());
        assert_eq!(
            s.mention_draft.as_ref().unwrap().issue,
            None,
            "no error yet"
        );
        let _ = s.update_mentions(Message::Pick("alice".into()));
        let e = effects(s.update_mentions(Message::NameSubmit));
        assert!(matches!(e.as_slice(), [Effect::CheckMentionGroup(..)]));
    }

    #[test]
    fn delete_asks_first() {
        let mut s = settings();
        assert!(effects(s.update_mentions(Message::AskDelete(sid(), "g1".into()))).is_empty());
        let e = effects(s.update_mentions(Message::ConfirmDelete));
        assert!(matches!(
            e.as_slice(),
            [Effect::EditMentionGroup(_, MentionGroupEdit::Delete { id })] if id == "g1"
        ));
        assert!(effects(s.update_mentions(Message::ConfirmDelete)).is_empty());
    }

    #[test]
    fn suggestions_follow_the_member_field() {
        let mut s = settings();
        let _ = s.update_mentions(Message::New(sid()));
        let _ = s.update_mentions(Message::MemberInput("@bo".into()));
        let seq = s.mention_draft.as_ref().unwrap().suggest_seq;
        assert!(effects(s.update_mentions(Message::SuggestTick(seq - 1))).is_empty());
        let e = effects(s.update_mentions(Message::SuggestTick(seq)));
        assert!(matches!(
            e.as_slice(),
            [Effect::SuggestMentionMembers(_, n, term)] if *n == seq && term == "bo"
        ));
        let list = vec![
            Suggestion {
                username: "bob".into(),
                name: Some("Bob".into()),
            },
            Suggestion {
                username: "bobby".into(),
                name: None,
            },
        ];
        let _ = s.update_mentions(Message::Suggestions(seq, Ok(list)));
        assert_eq!(s.mention_draft.as_ref().unwrap().suggestions.len(), 2);
        let _ = s.update_mentions(Message::Pick("bob".into()));
        let d = s.mention_draft.as_ref().unwrap();
        assert_eq!(d.members, ["bob"]);
        assert!(d.input.is_empty() && d.suggestions.is_empty());
    }
}
