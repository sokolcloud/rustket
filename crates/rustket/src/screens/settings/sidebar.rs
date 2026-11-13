//! The Sidebar card of the settings: unread group, grouping by type, default
//! sorting, the Threads section mode, and the section list editor (order,
//! per-section sorting, user-defined groups). Every layout change goes
//! through [`sidebar_layout::apply`], like the sidebar's header menu.

use std::collections::BTreeSet;
use std::fmt;

use iced::widget::{Space, checkbox, column, container, pick_list, row, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::config::{SidebarSort, ThreadsMode, UiPrefs};
use rustket_core::room_list::SectionKind;
use rustket_core::sidebar_layout::{
    self, GroupType, Layout, LayoutEdit, MAX_GROUP_NAME_CHARS, MAX_GROUPS, SectionId,
};
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, font_semibold};
use rustket_ui::widget::{
    danger_button, divider, ghost_button, icon_button, primary_button, secondary_button, text_field,
};

use rustket_core::ServerId;
use rustket_core::flags::{FlagColor, FlagEdit, Flags};

use super::{
    Message as SettingsMessage, SIDEBAR_CARD, Settings, SettingsCtx, field, flag_name_id,
    flag_row_id, section, segmented, sidebar_group_row_id, switch,
};
use crate::component::{Action, Effect};
use crate::workspace::sidebar::{sort_label, threads_limit_label};

/// Width of the per-section sort control (fits "По умолчанию (Сначала
/// свежие ответы)" on one line).
const SORT_WIDTH: f32 = 320.0;
/// Width of the Threads mode control.
const MODE_WIDTH: f32 = 360.0;
/// Width of the thread limit control.
const LIMIT_WIDTH: f32 = 200.0;

/// The group being created (`editing: None`) or edited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupDraft {
    pub editing: Option<String>,
    pub name: String,
    pub types: BTreeSet<GroupType>,
}

impl GroupDraft {
    /// A name is required; a new group also needs a room type (an edited one
    /// may be left without, see [`sidebar_layout::SidebarGroup::types`]).
    pub fn is_valid(&self) -> bool {
        !self.name.trim().is_empty() && (self.editing.is_some() || !self.types.is_empty())
    }
}

/// A signed-in account's flags, for the Flags block. Names are per account
/// (synced through its server), so the block is labelled with the account.
#[derive(Debug, Clone)]
pub struct FlagAccount<'a> {
    pub sid: ServerId,
    /// "alice on Company".
    pub label: String,
    pub flags: &'a Flags,
    /// Per color: `(listed, other)` items (`room_list::flag_usage`).
    pub usage: std::collections::BTreeMap<FlagColor, (usize, usize)>,
    pub status: crate::workspace::flags::Status,
    /// Kept threads whose parent is not starred (0 without a session).
    pub kept_unstarred: usize,
}

/// Messages of the section list, the group editor and the layout reset.
#[derive(Debug, Clone)]
pub enum Message {
    /// Section order and per-section sorting.
    Layout(LayoutEdit),
    /// The "Unread first" box of a section; its default stores no override.
    SetUnreadFirst(SectionId, bool),
    GroupNew,
    /// Edit the group with this id.
    GroupEdit(String),
    GroupName(String),
    GroupType(GroupType, bool),
    GroupSave,
    GroupCancel,
    /// Ask to delete the group with this id.
    GroupDelete(String),
    /// Delete the group whose deletion is being confirmed.
    GroupDeleteConfirm,
    GroupDeleteCancel,
    /// A flag name field changed (account, color, text).
    FlagName(ServerId, FlagColor, String),
    /// Store the edited name (blank: back to the default name).
    FlagNameSave(ServerId, FlagColor),
    /// "Move kept threads to Favorites", then its confirmation.
    StarKept,
    StarKeptConfirm,
    StarKeptCancel,
    /// Ask to remove a flag from every room and thread.
    FlagClear(ServerId, FlagColor),
    FlagClearConfirm,
    FlagClearCancel,
    /// Ask to reset the sidebar layout.
    Reset,
    ResetConfirm,
    ResetCancel,
}

/// Whether a section has an "Unread first" box (Unread is all unread).
pub(super) fn has_unread_first(id: &SectionId) -> bool {
    id.kind() != Some(SectionKind::Unread)
}

/// "Unread first" without an override: on only for Threads.
fn unread_first_default(id: &SectionId) -> bool {
    id.kind() == Some(SectionKind::Threads)
}

/// Caption of a room type in the group editor when another group claims it.
pub(super) fn type_caption(layout: &Layout, d: &GroupDraft, ty: GroupType) -> Option<String> {
    let other = layout
        .claimed_by(ty)
        .filter(|g| d.editing.as_deref() != Some(*g))
        .and_then(|g| layout.group_name(g))?;
    Some(if d.types.contains(&ty) {
        t!("settings.group_moves_from", group = other).into_owned()
    } else {
        t!("settings.section_in_group", group = other).into_owned()
    })
}

/// A pick list entry: value plus its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Choice<T> {
    pub(super) value: T,
    pub(super) label: String,
}

impl<T> fmt::Display for Choice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

fn threads_mode_label(m: ThreadsMode) -> String {
    match m {
        ThreadsMode::Off => t!("settings.threads_off"),
        ThreadsMode::Unread => t!("settings.threads_unread"),
        ThreadsMode::UnreadKept => t!("settings.threads_unread_kept"),
        ThreadsMode::Following => t!("settings.threads_following"),
    }
    .into_owned()
}

/// The offered thread limits, plus the configured one when it is another.
fn threads_limit_choices(current: u32) -> Vec<Choice<u32>> {
    sidebar_layout::threads_limit_choices(current)
        .into_iter()
        .map(|n| Choice {
            value: n,
            label: threads_limit_label(n),
        })
        .collect()
}

/// `flags`: the active account's, naming the flag sections.
fn section_name(id: &SectionId, layout: &Layout, flags: Option<&Flags>) -> String {
    match id {
        SectionId::Builtin(k) => t!(k.i18n_key()).into_owned(),
        SectionId::Group(g) => layout.group_name(g).unwrap_or_default().to_owned(),
        SectionId::Flag(c) => match flags {
            Some(f) => crate::workspace::flag_menu::flag_name(f, *c),
            None => t!(c.i18n_key()).into_owned(),
        },
        SectionId::Unknown(s) => s.clone(),
    }
}

fn type_section(k: SectionKind) -> Option<GroupType> {
    GroupType::ALL.into_iter().find(|t| t.section() == k)
}

/// Caption of a section row, and whether the section is claimed by a group
/// (it can then never have rooms: dimmed, controls disabled).
pub(super) fn section_caption(
    id: &SectionId,
    ui: &UiPrefs,
    layout: &Layout,
) -> (Option<String>, bool) {
    match id {
        SectionId::Builtin(SectionKind::Unread) => (
            (!ui.unread_on_top).then(|| t!("settings.section_off").into_owned()),
            false,
        ),
        SectionId::Builtin(SectionKind::Threads) => (
            Some(match ui.sidebar_threads {
                ThreadsMode::Off => t!("settings.section_off").into_owned(),
                m => threads_mode_label(m),
            }),
            false,
        ),
        SectionId::Builtin(SectionKind::Conversations) => {
            let claims: BTreeSet<&str> = GroupType::ALL
                .iter()
                .filter_map(|t| layout.claimed_by(*t))
                .collect();
            let all = GroupType::ALL
                .iter()
                .all(|t| layout.claimed_by(*t).is_some());
            if all {
                let single = (claims.len() == 1)
                    .then(|| claims.first().and_then(|g| layout.group_name(g)))
                    .flatten();
                let caption = match single {
                    Some(name) => t!("settings.section_in_group", group = name),
                    None => t!("settings.section_taken_by_groups"),
                };
                (Some(caption.into_owned()), true)
            } else {
                (
                    ui.group_by_type
                        .then(|| t!("settings.section_conversations_unused").into_owned()),
                    false,
                )
            }
        }
        SectionId::Builtin(k) => match type_section(*k) {
            Some(ty) => match layout.claimed_by(ty).and_then(|g| layout.group_name(g)) {
                Some(name) => (
                    Some(t!("settings.section_in_group", group = name).into_owned()),
                    true,
                ),
                None => (
                    (!ui.group_by_type).then(|| t!("settings.section_unused").into_owned()),
                    false,
                ),
            },
            None => (None, false),
        },
        SectionId::Group(g) => {
            let types: Vec<String> = ui
                .sidebar_groups
                .iter()
                .find(|x| &x.id == g)
                .map(|x| {
                    x.types
                        .iter()
                        .map(|t| t!(t.i18n_key()).into_owned())
                        .collect()
                })
                .unwrap_or_default();
            (
                Some(if types.is_empty() {
                    t!("settings.group_no_types").into_owned()
                } else {
                    types.join(", ")
                }),
                false,
            )
        }
        SectionId::Flag(_) => (Some(t!("settings.section_flag").into_owned()), false),
        SectionId::Unknown(_) => (None, false),
    }
}

impl Settings {
    /// Applies a layout edit; drops the collapse state of removed sections.
    fn apply_layout(ctx: &mut SettingsCtx<'_>, e: LayoutEdit) -> Action<SettingsMessage> {
        let applied = sidebar_layout::apply(&mut ctx.config.ui, e);
        let mut changed = applied.changed;
        for key in &applied.dropped_collapse_keys {
            changed |= ctx.ui_state.collapsed_sections.remove(key);
        }
        if changed {
            Action::effect(Effect::SaveConfig)
        } else {
            Action::none()
        }
    }

    /// Handles the Sidebar card's section list, group editor and reset.
    pub(super) fn update_sidebar(
        &mut self,
        msg: Message,
        ctx: &mut SettingsCtx<'_>,
    ) -> Action<SettingsMessage> {
        match msg {
            Message::Layout(e) => Self::apply_layout(ctx, e),
            Message::SetUnreadFirst(id, on) => {
                let on = (on != unread_first_default(&id)).then_some(on);
                Self::apply_layout(ctx, LayoutEdit::SetUnreadFirst { id, on })
            }
            Message::GroupNew => {
                if ctx.config.ui.sidebar_groups.len() < MAX_GROUPS {
                    self.group_draft = Some(GroupDraft::default());
                    self.confirm_delete = None;
                }
                Action::none()
            }
            Message::GroupEdit(id) => {
                if let Some(g) = ctx.config.ui.sidebar_groups.iter().find(|g| g.id == id) {
                    self.group_draft = Some(GroupDraft {
                        editing: Some(id),
                        name: g.name.clone(),
                        types: g.types.iter().copied().collect(),
                    });
                    self.confirm_delete = None;
                }
                Action::none()
            }
            Message::GroupName(name) => {
                if let Some(d) = &mut self.group_draft {
                    d.name = name.chars().take(MAX_GROUP_NAME_CHARS).collect();
                }
                Action::none()
            }
            Message::GroupType(ty, on) => {
                if let Some(d) = &mut self.group_draft {
                    if on {
                        d.types.insert(ty);
                    } else {
                        d.types.remove(&ty);
                    }
                }
                Action::none()
            }
            Message::GroupSave => {
                let Some(d) = self.group_draft.take_if(|d| d.is_valid()) else {
                    return Action::none();
                };
                let edits =
                    match d.editing {
                        None => vec![LayoutEdit::AddGroup {
                            name: d.name,
                            types: d.types.into_iter().collect(),
                        }],
                        Some(id) => {
                            let mut v = vec![LayoutEdit::RenameGroup {
                                id: id.clone(),
                                name: d.name,
                            }];
                            v.extend(GroupType::ALL.into_iter().map(|ty| {
                                LayoutEdit::SetGroupType {
                                    id: id.clone(),
                                    ty,
                                    on: d.types.contains(&ty),
                                }
                            }));
                            v
                        }
                    };
                let mut changed = false;
                for e in edits {
                    changed |= sidebar_layout::apply(&mut ctx.config.ui, e).changed;
                }
                if changed {
                    Action::effect(Effect::SaveConfig)
                } else {
                    Action::none()
                }
            }
            Message::GroupCancel => {
                self.group_draft = None;
                Action::none()
            }
            Message::GroupDelete(id) => {
                self.confirm_delete = Some(id);
                Action::none()
            }
            Message::GroupDeleteConfirm => {
                let Some(id) = self.confirm_delete.take() else {
                    return Action::none();
                };
                if self
                    .group_draft
                    .as_ref()
                    .is_some_and(|d| d.editing.as_deref() == Some(id.as_str()))
                {
                    self.group_draft = None;
                }
                Self::apply_layout(ctx, LayoutEdit::DeleteGroup { id })
            }
            Message::GroupDeleteCancel => {
                self.confirm_delete = None;
                Action::none()
            }
            Message::FlagName(sid, color, name) => {
                let name: String = name
                    .chars()
                    .take(rustket_core::flags::MAX_FLAG_NAME_CHARS)
                    .collect();
                self.flag_drafts.insert((sid, color), name);
                Action::none()
            }
            Message::FlagNameSave(sid, color) => {
                let Some(name) = self.flag_drafts.remove(&(sid.clone(), color)) else {
                    return Action::none();
                };
                Action::effect(Effect::SetFlag(
                    sid,
                    FlagEdit::Rename {
                        color,
                        name: rustket_core::flags::clean_flag_name(&name),
                    },
                ))
            }
            Message::StarKept => {
                self.confirm_star_kept = true;
                Action::none()
            }
            Message::StarKeptConfirm => {
                self.confirm_star_kept = false;
                Action::effect(Effect::StarKeptThreads)
            }
            Message::StarKeptCancel => {
                self.confirm_star_kept = false;
                Action::none()
            }
            Message::FlagClear(sid, color) => {
                self.confirm_flag_clear = Some((sid, color));
                Action::none()
            }
            Message::FlagClearConfirm => match self.confirm_flag_clear.take() {
                Some((sid, color)) => {
                    Action::effect(Effect::SetFlag(sid, FlagEdit::ClearColor(color)))
                }
                None => Action::none(),
            },
            Message::FlagClearCancel => {
                self.confirm_flag_clear = None;
                Action::none()
            }
            Message::Reset => {
                self.confirm_reset = true;
                Action::none()
            }
            Message::ResetConfirm => {
                if !std::mem::take(&mut self.confirm_reset) {
                    return Action::none();
                }
                self.group_draft = None;
                self.confirm_delete = None;
                Self::apply_layout(ctx, LayoutEdit::Reset)
            }
            Message::ResetCancel => {
                self.confirm_reset = false;
                Action::none()
            }
        }
    }

    /// The Sidebar card. A group's deletion preset by the sidebar's
    /// "Ungroup…" is confirmed in its row here; an edited group's editor is
    /// shown in its row too.
    pub(super) fn sidebar_card<'a>(
        &'a self,
        ui: &'a UiPrefs,
        flags: &[FlagAccount<'a>],
        star_kept_busy: bool,
    ) -> Element<'a, SettingsMessage> {
        let layout = Layout::from_prefs(ui);
        let mut threads_help = t!("settings.threads_mode_help").into_owned();
        if ui.sidebar_threads == ThreadsMode::Following {
            threads_help = format!("{threads_help}\n{}", t!("settings.threads_following_help"));
        }
        let modes: Vec<Choice<ThreadsMode>> = [
            ThreadsMode::Off,
            ThreadsMode::Unread,
            ThreadsMode::UnreadKept,
            ThreadsMode::Following,
        ]
        .into_iter()
        .map(|m| Choice {
            value: m,
            label: threads_mode_label(m),
        })
        .collect();
        let current_mode = Choice {
            value: ui.sidebar_threads,
            label: threads_mode_label(ui.sidebar_threads),
        };
        let limit = ui.sidebar_threads_limit;
        let limit_field = (ui.sidebar_threads != ThreadsMode::Off).then(|| {
            field(
                t!("settings.threads_limit").into_owned(),
                Some(t!("settings.threads_limit_help").into_owned()),
                pick_list(
                    threads_limit_choices(limit),
                    Some(Choice {
                        value: limit,
                        label: threads_limit_label(limit),
                    }),
                    |c: Choice<u32>| {
                        SettingsMessage::Sidebar(Message::Layout(LayoutEdit::SetThreadsLimit(
                            c.value,
                        )))
                    },
                )
                .text_size(TEXT_BODY)
                .width(LIMIT_WIDTH)
                .style(style::pick_list)
                .menu_style(style::menu)
                .into(),
            )
        });
        let mut rows = vec![
            switch(
                t!("settings.unread_on_top").into_owned(),
                Some(t!("settings.unread_on_top_help").into_owned()),
                ui.unread_on_top,
                Some(SettingsMessage::SetUnreadOnTop),
            ),
            switch(
                t!("settings.group_by_type").into_owned(),
                Some(t!("settings.group_by_type_help").into_owned()),
                ui.group_by_type,
                Some(SettingsMessage::SetGroupByType),
            ),
            field(
                t!("settings.sidebar_sort").into_owned(),
                None,
                segmented(
                    &ui.sidebar_sort,
                    vec![
                        (
                            SidebarSort::Activity,
                            t!("settings.sort_newest").into_owned(),
                        ),
                        (
                            SidebarSort::ActivityAsc,
                            t!("settings.sort_oldest").into_owned(),
                        ),
                        (
                            SidebarSort::Alphabetical,
                            t!("settings.sort_alpha").into_owned(),
                        ),
                    ],
                    SettingsMessage::SetSidebarSort,
                ),
            ),
            field(
                t!("settings.threads_mode").into_owned(),
                Some(threads_help),
                pick_list(modes, Some(current_mode), |c: Choice<ThreadsMode>| {
                    SettingsMessage::SetThreadsMode(c.value)
                })
                .text_size(TEXT_BODY)
                .width(MODE_WIDTH)
                .style(style::pick_list)
                .menu_style(style::menu)
                .into(),
            ),
        ];
        rows.extend(limit_field);
        rows.push(switch(
            t!("settings.starred_threads").into_owned(),
            Some(format!(
                "{}\n{}",
                t!("settings.starred_threads_help"),
                t!("settings.starred_threads_sync_help")
            )),
            ui.sidebar_starred_threads,
            Some(SettingsMessage::SetStarredThreads),
        ));
        let kept: usize = flags.iter().map(|a| a.kept_unstarred).sum();
        if ui.sidebar_starred_threads && kept > 0 {
            rows.push(
                self.star_kept_row(kept, star_kept_busy)
                    .map(SettingsMessage::Sidebar),
            );
        }
        let active = flags.first().map(|a| a.flags);
        rows.extend([
            Element::from(
                column![
                    self.sections_list(ui, &layout, active),
                    self.new_group(ui, &layout)
                ]
                .spacing(SPACE_3),
            )
            .map(SettingsMessage::Sidebar),
            self.flags_block(flags).map(SettingsMessage::Sidebar),
            self.reset_row().map(SettingsMessage::Sidebar),
        ]);
        container(section(t!("settings.sidebar").into_owned(), rows))
            .id(SIDEBAR_CARD)
            .into()
    }

    /// The sections in their order. Flag sections are listed only while the
    /// active account (`flags`) uses their color.
    fn sections_list<'a>(
        &'a self,
        ui: &'a UiPrefs,
        layout: &Layout,
        flags: Option<&Flags>,
    ) -> Element<'a, Message> {
        let mut list = column![
            text(t!("settings.sections"))
                .size(TEXT_BODY)
                .font(font_semibold()),
        ]
        .spacing(SPACE_3);
        let ids = listed_sections(layout, flags);
        for (i, id) in ids.iter().enumerate() {
            list = list.push(divider());
            list = list.push(self.section_row(
                ui,
                layout,
                flags,
                id,
                i.checked_sub(1).and_then(|j| ids.get(j).copied()),
                ids.get(i + 1).copied(),
            ));
        }
        list = list.push(
            text(t!("settings.omni_on_top"))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
        );
        list.into()
    }

    /// A section: name, caption and the move/edit/delete buttons, then the
    /// sort and "Unread first" (wrapping in narrow windows). A group's row
    /// shows its deletion confirmation or its editor instead of the controls.
    /// `prev`/`next`: the listed neighbours (moves skip unlisted sections).
    fn section_row<'a>(
        &'a self,
        ui: &'a UiPrefs,
        layout: &Layout,
        flags: Option<&Flags>,
        id: &SectionId,
        prev: Option<&SectionId>,
        next: Option<&SectionId>,
    ) -> Element<'a, Message> {
        let (caption, claimed) = section_caption(id, ui, layout);
        let group = id.group_id().map(str::to_owned);
        let mut head = row![
            text(section_name(id, layout, flags))
                .size(TEXT_BODY)
                .font(font_semibold())
                .style(if claimed {
                    style::text_hint
                } else {
                    style::text_primary
                })
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        head = match caption {
            Some(c) => head.push(text(c).size(TEXT_CAPTION).width(Length::Fill).style(
                if claimed {
                    style::text_hint
                } else {
                    style::text_secondary
                },
            )),
            None => head.push(Space::new().width(Length::Fill)),
        };
        let mv = |anchor: Option<&SectionId>, after: bool| {
            anchor.filter(|_| !claimed).map(|a| {
                Message::Layout(LayoutEdit::MoveTo {
                    id: id.clone(),
                    anchor: a.clone(),
                    after,
                })
            })
        };
        head = head.push(
            row![
                icon_button(Icon::ArrowUp, mv(prev, false), t!("settings.move_up")),
                icon_button(Icon::ArrowDown, mv(next, true), t!("settings.move_down")),
            ]
            .spacing(SPACE_1),
        );
        if let Some(c) = id.flag() {
            head = head.push(rustket_ui::widget::flag_swatch(c.index(), 10.0));
        }
        if let Some(g) = &group {
            head = head.push(
                row![
                    icon_button(
                        Icon::Pencil,
                        Some(Message::GroupEdit(g.clone())),
                        t!("settings.group_edit")
                    ),
                    icon_button(
                        Icon::Trash2,
                        Some(Message::GroupDelete(g.clone())),
                        t!("settings.group_delete")
                    ),
                ]
                .spacing(SPACE_1),
            );
        }

        let deleting = group.is_some() && self.confirm_delete == group;
        let draft = self
            .group_draft
            .as_ref()
            .filter(|d| group.is_some() && d.editing == group);
        let body: Element<'a, Message> = if deleting {
            column![
                text(t!(
                    "settings.group_delete_confirm",
                    name = section_name(id, layout, flags)
                ))
                .size(TEXT_BODY),
                row![
                    danger_button(
                        t!("settings.group_delete"),
                        Some(Message::GroupDeleteConfirm)
                    ),
                    secondary_button(
                        t!("settings.group_cancel"),
                        Some(Message::GroupDeleteCancel)
                    ),
                ]
                .spacing(SPACE_2),
            ]
            .spacing(SPACE_2)
            .into()
        } else if let Some(d) = draft {
            Self::group_editor(d, layout)
        } else {
            self.section_controls(ui, layout, id, claimed)
        };
        let r = column![head, body].spacing(SPACE_2);
        match group {
            Some(g) => container(r).id(sidebar_group_row_id(&g)).into(),
            None => r.into(),
        }
    }

    /// The sort and "Unread first" of a section.
    fn section_controls<'a>(
        &'a self,
        ui: &'a UiPrefs,
        layout: &Layout,
        id: &SectionId,
        claimed: bool,
    ) -> Element<'a, Message> {
        let threads = id.kind() == Some(SectionKind::Threads);
        let key = id.as_config_str();
        let stored = ui.sidebar_section_prefs.get(&key).and_then(|p| p.sort);
        let default_label = t!(
            "sidebar.sort_default",
            sort = sort_label(layout.default_sort, threads)
        )
        .into_owned();
        let label_of = |s: Option<SidebarSort>| match s {
            None => default_label.clone(),
            Some(s) => sort_label(s, threads),
        };
        let sort: Element<'a, Message> = if claimed {
            secondary_button(label_of(stored), None)
                .width(SORT_WIDTH)
                .into()
        } else {
            let choices: Vec<Choice<Option<SidebarSort>>> = [
                None,
                Some(SidebarSort::Activity),
                Some(SidebarSort::ActivityAsc),
                Some(SidebarSort::Alphabetical),
            ]
            .into_iter()
            .map(|s| Choice {
                value: s,
                label: label_of(s),
            })
            .collect();
            let selected = Choice {
                value: stored,
                label: label_of(stored),
            };
            let sid = id.clone();
            pick_list(
                choices,
                Some(selected),
                move |c: Choice<Option<SidebarSort>>| {
                    Message::Layout(LayoutEdit::SetSort {
                        id: sid.clone(),
                        sort: c.value,
                    })
                },
            )
            .text_size(TEXT_BODY)
            .width(SORT_WIDTH)
            .style(style::pick_list)
            .menu_style(style::menu)
            .into()
        };
        let mut controls = row![sort].spacing(SPACE_3).align_y(Alignment::Center);
        if has_unread_first(id) {
            let sid = id.clone();
            controls = controls.push(
                checkbox(layout.is_unread_first(id))
                    .label(t!("settings.unread_first").into_owned())
                    .on_toggle_maybe(
                        (!claimed)
                            .then_some(move |on: bool| Message::SetUnreadFirst(sid.clone(), on)),
                    )
                    .size(16)
                    .text_size(TEXT_BODY)
                    .style(style::checkbox),
            );
        }
        controls.wrap().vertical_spacing(SPACE_2).into()
    }

    /// "New group", or the editor of a group being created.
    fn new_group<'a>(&'a self, ui: &'a UiPrefs, layout: &Layout) -> Element<'a, Message> {
        match &self.group_draft {
            Some(d) if d.editing.is_none() => Self::group_editor(d, layout),
            _ => {
                let can_add = ui.sidebar_groups.len() < MAX_GROUPS;
                row![
                    secondary_button(
                        t!("settings.group_new"),
                        can_add.then_some(Message::GroupNew)
                    )
                    .icon(Icon::Plus)
                ]
                .into()
            }
        }
    }

    /// The inline editor of the group being created or edited.
    fn group_editor<'a>(d: &'a GroupDraft, layout: &Layout) -> Element<'a, Message> {
        let save = d.is_valid().then_some(Message::GroupSave);
        let mut name = text_field(t!("settings.group_name"), &d.name, Message::GroupName);
        if let Some(m) = save.clone() {
            name = name.on_submit(m);
        }
        let mut types = column![text(t!("settings.group_types")).size(TEXT_BODY)].spacing(SPACE_2);
        for ty in GroupType::ALL {
            let mut r = row![
                checkbox(d.types.contains(&ty))
                    .label(t!(ty.i18n_key()).into_owned())
                    .on_toggle(move |v| Message::GroupType(ty, v))
                    .size(16)
                    .text_size(TEXT_BODY)
                    .style(style::checkbox)
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
            if let Some(caption) = type_caption(layout, d, ty) {
                r = r.push(
                    text(caption)
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary),
                );
            }
            types = types.push(r.wrap());
        }
        column![
            name,
            types,
            row![
                primary_button(t!("settings.group_save"), save),
                secondary_button(t!("settings.group_cancel"), Some(Message::GroupCancel)),
            ]
            .spacing(SPACE_2),
        ]
        .spacing(SPACE_3)
        .into()
    }

    /// "Flags": per account, a row per color with its name field, how many
    /// items carry it and "Remove from all" (confirmed in place).
    fn flags_block<'a>(&'a self, accounts: &[FlagAccount<'a>]) -> Element<'a, Message> {
        let mut block = column![
            text(t!("settings.flags"))
                .size(TEXT_BODY)
                .font(font_semibold()),
            text(t!("settings.flags_caption"))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
        ]
        .spacing(SPACE_2);
        if accounts.is_empty() {
            return block
                .push(
                    text(t!("settings.flags_signed_out"))
                        .size(TEXT_CAPTION)
                        .style(style::text_hint),
                )
                .into();
        }
        for a in accounts {
            let mut head = column![
                text(a.label.clone())
                    .size(TEXT_BODY)
                    .style(style::text_secondary)
            ]
            .spacing(SPACE_1);
            let status = match &a.status {
                crate::workspace::flags::Status::Synced => None,
                crate::workspace::flags::Status::Pending => {
                    Some((t!("settings.flags_pending").into_owned(), false))
                }
                crate::workspace::flags::Status::Failed(e) => {
                    Some((t!("settings.flags_failed", error = e).into_owned(), true))
                }
            };
            if let Some((s, failed)) = status {
                head = head.push(text(s).size(TEXT_CAPTION).style(if failed {
                    style::text_danger
                } else {
                    style::text_hint
                }));
            }
            block = block.push(divider()).push(head);
            for c in FlagColor::ALL {
                block = block.push(self.flag_row(a, c));
            }
        }
        block.into()
    }

    /// A color of an account: swatch, name field (saved with Enter, the
    /// check button or on leaving the settings), usage and "Remove from all".
    fn flag_row<'a>(&'a self, a: &FlagAccount<'a>, c: FlagColor) -> Element<'a, Message> {
        let key = (a.sid.clone(), c);
        let stored = a.flags.name(c).unwrap_or_default();
        let draft = self.flag_drafts.get(&key);
        let value = draft.map_or(stored, String::as_str);
        let dirty = draft
            .is_some_and(|d| rustket_core::flags::clean_flag_name(d).as_deref() != a.flags.name(c));
        let sid = a.sid.clone();
        let save = dirty.then(|| Message::FlagNameSave(a.sid.clone(), c));
        let mut name = text_field("", value, move |s| Message::FlagName(sid.clone(), c, s))
            .placeholder(t!(c.i18n_key()))
            .id(flag_name_id(&a.sid, c))
            .width(Length::Fixed(240.0));
        if let Some(m) = save.clone() {
            name = name.on_submit(m);
        }
        let (listed, other) = a.usage.get(&c).copied().unwrap_or_default();
        let mut usage = t!("settings.flag_used", count = listed).into_owned();
        if other > 0 {
            usage = format!(
                "{usage} · {}",
                t!("settings.flag_unavailable", count = other)
            );
        }
        let mut r = row![
            rustket_ui::widget::flag_swatch(c.index(), 12.0),
            Element::from(name),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        if dirty {
            r = r.push(icon_button(Icon::Check, save, t!("settings.flag_save")));
        }
        r = r.push(
            text(usage)
                .size(TEXT_CAPTION)
                .width(Length::Fill)
                .style(style::text_secondary),
        );
        if listed + other > 0 {
            r = r.push(ghost_button(
                t!("settings.flag_clear"),
                Some(Message::FlagClear(a.sid.clone(), c)),
            ));
        }
        let mut body = column![r.wrap().vertical_spacing(SPACE_2)].spacing(SPACE_2);
        if self.confirm_flag_clear.as_ref() == Some(&key) {
            let name = crate::workspace::flag_menu::flag_name(a.flags, c);
            body = body.extend([
                text(t!("settings.flag_clear_confirm", name = name))
                    .size(TEXT_BODY)
                    .into(),
                row![
                    danger_button(t!("settings.flag_clear"), Some(Message::FlagClearConfirm)),
                    secondary_button(t!("settings.group_cancel"), Some(Message::FlagClearCancel)),
                ]
                .spacing(SPACE_2)
                .into(),
            ]);
        }
        container(body).id(flag_row_id(&a.sid, c)).into()
    }

    /// "Move kept threads to Favorites" (`count` of them), confirmed first:
    /// starring shows in other clients too.
    fn star_kept_row<'a>(&'a self, count: usize, busy: bool) -> Element<'a, Message> {
        if self.confirm_star_kept && !busy {
            return column![
                text(t!("settings.star_kept_confirm", count = count)).size(TEXT_BODY),
                row![
                    primary_button(t!("settings.star_kept_do"), Some(Message::StarKeptConfirm)),
                    secondary_button(t!("settings.group_cancel"), Some(Message::StarKeptCancel)),
                ]
                .spacing(SPACE_2),
            ]
            .spacing(SPACE_2)
            .into();
        }
        let caption = if busy {
            t!("settings.star_kept_busy")
        } else {
            t!("settings.star_kept_help", count = count)
        };
        column![
            row![ghost_button(
                t!("settings.star_kept"),
                (!busy).then_some(Message::StarKept)
            )],
            text(caption)
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
        ]
        .spacing(SPACE_1)
        .into()
    }

    fn reset_row<'a>(&'a self) -> Element<'a, Message> {
        if self.confirm_reset {
            return column![
                text(t!("settings.sidebar_reset_confirm")).size(TEXT_BODY),
                row![
                    danger_button(t!("settings.sidebar_reset_do"), Some(Message::ResetConfirm)),
                    secondary_button(t!("settings.group_cancel"), Some(Message::ResetCancel)),
                ]
                .spacing(SPACE_2),
            ]
            .spacing(SPACE_2)
            .into();
        }
        row![ghost_button(
            t!("settings.sidebar_reset"),
            Some(Message::Reset)
        )]
        .into()
    }
}

/// The sections of the list: the layout's order without flag sections
/// whose color the active account does not use (none without an account).
pub(super) fn listed_sections<'l>(layout: &'l Layout, flags: Option<&Flags>) -> Vec<&'l SectionId> {
    let used = flags.map(Flags::used_colors).unwrap_or_default();
    layout
        .order
        .iter()
        .filter(|id| id.flag().is_none_or(|c| used.contains(&c)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threads_limit_choices_are_labelled() {
        let c = threads_limit_choices(30);
        assert_eq!(c.len(), 7);
        assert_eq!(c[3].to_string(), "30");
        assert_eq!(c[6].value, 0);
    }
}
