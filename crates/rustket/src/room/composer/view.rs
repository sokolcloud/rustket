//! Composer view and its key bindings.

use iced::keyboard::{self, key};
use iced::widget::text_editor::{self, Binding, KeyPress};
use iced::widget::{button, column, container, row, space, text as label, text_input};
use iced::{Alignment, Background, Border, Color, Element, Length, Theme};
use rocketchat::models::RoomType;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::permissions::{self, PostBlock};
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_CAPTION, TEXT_SMALL, font_semibold,
};
use rustket_ui::widget::{
    Placement, TooltipPosition, above, danger_button, icon_button_with, modal, popover,
    primary_button, secondary_button, tooltip,
};
use rustket_ui::{Icon, icon, style};

use super::autocomplete;
use super::send::{can_join, needs_join_code};
use super::text::Format;
use super::{Composer, MAX_EDITOR_HEIGHT, Message, PickTarget};
use crate::keymap;
use crate::room::{ViewCtx, emoji_picker, upload};

impl Composer {
    // --- view ---------------------------------------------------------------------

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        if let Some(block) = permissions::post_block(ctx.workspace, &self.rid) {
            return self.notice_view(block, ctx);
        }
        if let Some(n) = crate::e2e::room::composer_block(ctx.workspace, &self.rid) {
            return container(n).padding([SPACE_2, SPACE_4]).into();
        }
        // Bars above the editor, in one slot so the editor keeps its tree
        // position (and focus) when they come and go.
        let mut bars: Vec<Element<'a, Message>> = Vec::new();
        if let Some(h) = self.peer_bar(ctx) {
            bars.push(h);
        }
        if let Some(h) = crate::e2e::room::composer_hint(ctx.workspace, &self.rid) {
            bars.push(h);
        }
        let slash = self
            .content
            .line(0)
            .is_some_and(|l| l.text.starts_with('/'));
        if let Some(h) = crate::e2e::room::slash_hint(ctx.workspace, &self.rid, slash) {
            bars.push(h);
        }
        if let Some(l) = ctx
            .workspace
            .typing_label(&self.rid, self.tmid.as_deref(), ctx.now)
        {
            bars.push(
                label(crate::workspace::sidebar::typing_text(&l))
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary)
                    .into(),
            );
        }
        if let Some(t) =
            self.uploads
                .transfers_view(ctx.server_id.as_str(), &self.rid, self.tmid.as_deref())
        {
            bars.push(t.map(Message::Upload));
        }
        if let Some(h) = self.scheduled_bar(ctx) {
            bars.push(h);
        }
        if let Some(e) = &self.editing {
            bars.push(bar(
                Icon::Pencil,
                t!("room_composer.editing").into_owned(),
                e.original.lines().next().unwrap_or_default().to_owned(),
                Message::CancelEdit,
            ));
        }
        for q in &self.quotes {
            let (glyph, title) = if q.forwarded {
                (
                    Icon::Forward,
                    t!("room_composer.forwarding", user = q.author.as_str()),
                )
            } else {
                (
                    Icon::Quote,
                    t!("room_composer.quoting", user = q.author.as_str()),
                )
            };
            bars.push(bar(
                glyph,
                title.into_owned(),
                q.excerpt.clone(),
                Message::RemoveQuote(q.mid.clone()),
            ));
        }
        let frame = container(column![self.editor_view(ctx), self.toolbar_view(ctx)])
            .width(Length::Fill)
            .style(style::container_composer(false));
        let popup: Element<'a, Message> = match &self.popup {
            Some(p) if !p.items.is_empty() => autocomplete::view(p, ctx, Message::AcSelect),
            _ => space().into(),
        };
        let framed = popover(
            frame,
            popup,
            self.popup_visible(),
            Placement::AboveStart,
            Message::AcClose,
        );
        let bars = (!bars.is_empty()).then(|| column(bars).spacing(SPACE_1).into());
        // The emoji picker opens above the whole composer frame.
        let col = above(bars, SPACE_1, self.with_picker(framed, ctx));
        let body = container(col)
            .padding([SPACE_2, SPACE_4])
            .width(Length::Fill);
        // A poll's `@all` confirmation shows over the poll dialog.
        if let Some(c) = &self.confirm_broad {
            return modal(
                body,
                Some(super::broad::dialog(
                    c,
                    Message::BroadCancelled,
                    Message::BroadConfirmed,
                )),
                Some(Message::BroadCancelled),
            );
        }
        if let Some(p) = &self.poll {
            return modal(
                body,
                Some(p.view(Message::Poll)),
                Some(Message::Poll(super::poll::Msg::Cancel)),
            );
        }
        if self.confirm_delete.is_some() {
            return modal(
                body,
                Some(self.delete_dialog()),
                Some(Message::DeleteCancelled),
            );
        }
        if let Some(p) = &self.schedule {
            let content = column![
                p.view(
                    chrono::Local::now(),
                    ctx.prefs.morning_hour(),
                    ctx.prefs.clock_24h
                )
                .map(Message::SchedulePicker)
            ]
            .spacing(SPACE_3);
            return modal(
                body,
                Some(rustket_ui::widget::dialog(
                    t!("scheduled.dialog_title").into_owned(),
                    content,
                    vec![crate::later::when::cancel(Message::ScheduleCancelled)],
                    Some(Message::ScheduleCancelled),
                )),
                Some(Message::ScheduleCancelled),
            );
        }
        let dialog = self.uploads.dialog_view().map(|d| d.map(Message::Upload));
        modal(body, dialog, Some(Message::Upload(self.uploads.dismiss())))
    }

    /// `base` with the emoji picker popover above it (while open).
    fn with_picker<'a>(
        &'a self,
        base: impl Into<Element<'a, Message>>,
        ctx: &ViewCtx<'a>,
    ) -> Element<'a, Message> {
        let picker_view: Element<'a, Message> = match &self.picker {
            Some((p, target)) => {
                let title = match target {
                    PickTarget::React(_) => Some(t!("room_composer.add_reaction").into_owned()),
                    PickTarget::Insert => None,
                };
                p.view(ctx.workspace, ctx.server_id, ctx.server, ctx.images, title)
                    .map(Message::Picker)
            }
            None => space().into(),
        };
        popover(
            base,
            picker_view,
            self.picker.is_some(),
            Placement::AboveStart,
            Message::Picker(emoji_picker::Message::Close),
        )
    }

    /// "It's 03:12 for Bob (UTC+3)" above a 1:1 DM composer when the partner's
    /// clock differs from ours.
    fn peer_bar<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<Element<'a, Message>> {
        let (username, info, clock) = self.peer_clock()?;
        let time = crate::room::panel::user_info::local_time(
            f64::from(clock.offset_secs) / 3600.0,
            ctx.prefs.clock_24h,
        )?;
        let real = ctx.workspace.use_real_names(ctx.prefs.use_real_names);
        let user = rustket_ui::widget::sanitize_bidi(&crate::room::panel::common::user_display(
            ctx.workspace,
            real,
            username,
            info.name.as_deref(),
        ))
        .into_owned();
        let (glyph, line) = if clock.night {
            (
                Icon::Moon,
                t!("room_composer.peer_time_night", time = time, user = user),
            )
        } else {
            (
                Icon::Clock,
                t!("room_composer.peer_time", time = time, user = user),
            )
        };
        Some(
            row![
                icon::sized(glyph, 12.0).style(style::text_secondary),
                label(line.into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary),
            ]
            .spacing(SPACE_1)
            .align_y(Alignment::Center)
            .into(),
        )
    }

    /// "2 scheduled messages" above the composer of a room (or thread) with
    /// scheduled messages; a click opens the Drafts & scheduled view.
    fn scheduled_bar<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<Element<'a, Message>> {
        let n = ctx
            .workspace
            .scheduled()
            .count_in(&self.rid, self.tmid.as_deref());
        if n == 0 {
            return None;
        }
        let line = t!(
            format!("scheduled.hint_{}", crate::room::render::plural(n as u64)),
            count = n
        )
        .into_owned();
        Some(
            button(
                row![
                    icon::sized(Icon::ClockArrowUp, 12.0).style(style::text_link),
                    label(line).size(TEXT_CAPTION).style(style::text_link),
                ]
                .spacing(SPACE_1)
                .align_y(Alignment::Center),
            )
            .padding(0)
            .style(style::button_ghost)
            .on_press(Message::OpenScheduled)
            .into(),
        )
    }

    fn delete_dialog<'a>(&'a self) -> Element<'a, Message> {
        rustket_ui::widget::dialog(
            t!("room_composer.delete.title").into_owned(),
            label(t!("room_composer.delete.body")).size(TEXT_SMALL),
            vec![
                secondary_button(t!("room_composer.cancel"), Some(Message::DeleteCancelled)).into(),
                danger_button(
                    t!("room_composer.delete.confirm"),
                    Some(Message::DeleteConfirmed),
                )
                .into(),
            ],
            Some(Message::DeleteCancelled),
        )
    }

    fn placeholder(&self, ws: &Workspace) -> String {
        if self.tmid.is_some() {
            return t!("room_composer.placeholder_thread").into_owned();
        }
        match ws.room(&self.rid) {
            Some(e) => {
                let name = e.fname().unwrap_or(e.name());
                let room = match e.room_type() {
                    RoomType::Direct => format!("@{name}"),
                    _ => format!("#{name}"),
                };
                t!("room_composer.placeholder_room", room = room).into_owned()
            }
            None => t!("room_composer.placeholder").into_owned(),
        }
    }

    fn editor_view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let keys = Keys {
            popup: self.popup_visible(),
            empty: self.content.is_empty(),
            cancellable: self.editing.is_some()
                || !self.quotes.is_empty()
                || self.schedule.is_some(),
            schedulable: self.can_send() && self.can_schedule(ctx.workspace),
        };
        iced::widget::text_editor(&self.content)
            .id(self.editor_id.clone())
            .placeholder(self.placeholder(ctx.workspace))
            .on_action(Message::Editor)
            .key_binding(move |kp| key_binding(kp, keys))
            .padding([10, 12])
            .size(rustket_ui::theme::TEXT_BODY)
            .max_height(MAX_EDITOR_HEIGHT)
            .style(editor_style)
            .into()
    }

    /// The app actions menu (`messageBoxAction` buttons), if there are any.
    fn apps_menu<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<Element<'a, Message>> {
        use crate::uikit::{UiKitMsg, buttons};
        let ws = ctx.workspace;
        if !buttons::composer_has_buttons(ws, &self.rid) {
            return None;
        }
        let tmid = self.tmid.as_deref();
        let open = buttons::composer_menu_open(ws, &self.rid, tmid);
        let key = buttons::composer_menu_key(&self.rid, tmid);
        let wrap = |m: UiKitMsg| Message::UiKit(Box::new(m));
        let entries = if open {
            buttons::composer_entries(ws, &self.rid, tmid, &self.content.text(), wrap)
        } else {
            Vec::new()
        };
        let trigger = icon_button_with(
            buttons::COMPOSER_ICON,
            Some(wrap(UiKitMsg::Popover((!open).then_some(key)))),
            t!("uikit.apps"),
            open,
            30.0,
        );
        Some(rustket_ui::widget::popover_menu(
            trigger,
            &entries,
            open,
            wrap(UiKitMsg::Popover(None)),
            Placement::AboveEnd,
        ))
    }

    fn toolbar_view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let emoji_btn = icon_button_with(
            Icon::Smile,
            Some(Message::TogglePicker),
            t!("room_composer.emoji"),
            self.picker.is_some(),
            30.0,
        );
        let mut bar = row![emoji_btn].spacing(SPACE_1).align_y(Alignment::Center);
        bar = bar.push(
            container(space())
                .width(Length::Fixed(1.0))
                .height(18)
                .style(style::container_divider),
        );
        // Format buttons take the room left of the apps/attach/send buttons;
        // the ones that do not fit are hidden (narrow room column next to a
        // panel), so send and attach always stay visible.
        let formats = iced::widget::responsive(|size| {
            let fit = ((size.width + SPACE_1) / (28.0 + SPACE_1)).floor().max(0.0) as usize;
            row(Format::ALL.into_iter().take(fit).map(|f| {
                let (ic, tip) = format_icon(f);
                icon_button_with(ic, Some(Message::Format(f)), tip, false, 28.0)
            }))
            .spacing(SPACE_1)
            .align_y(Alignment::Center)
            .into()
        });
        bar = bar.push(container(formats).width(Length::Fill).height(30));
        if let Some(apps) = self.apps_menu(ctx) {
            bar = bar.push(apps);
        }
        bar = bar.push(icon_button_with(
            Icon::ChartColumn,
            self.editing.is_none().then_some(Message::OpenPoll),
            t!("poll.create"),
            self.poll.is_some(),
            30.0,
        ));
        if permissions::can_upload(ctx.workspace, &self.rid) {
            // Files cannot be added to an edited message.
            bar = bar.push(icon_button_with(
                Icon::Paperclip,
                self.editing
                    .is_none()
                    .then_some(Message::Upload(upload::Message::Attach)),
                t!("room_composer.attach"),
                false,
                30.0,
            ));
        }
        let can_send = self.can_send();
        if self.editing.is_none() {
            let allowed = self.can_schedule(ctx.workspace);
            let tip = if allowed {
                match keymap::schedule_chord() {
                    Some(chord) => format!("{} · {chord}", t!("scheduled.button")),
                    None => t!("scheduled.button").into_owned(),
                }
            } else if !ctx.workspace.scheduled_storage() {
                t!("scheduled.no_storage").into_owned()
            } else {
                t!("scheduled.e2e_unavailable").into_owned()
            };
            bar = bar.push(icon_button_with(
                Icon::ClockArrowUp,
                (allowed && can_send).then_some(Message::Schedule),
                tip,
                self.schedule.is_some(),
                30.0,
            ));
        }
        let send_tip = if self.editing.is_some() {
            t!("room_composer.save")
        } else {
            t!("room_composer.send")
        };
        let send = button(
            container(
                icon(if self.editing.is_some() {
                    Icon::Check
                } else {
                    Icon::Send
                })
                .size(16.0),
            )
            .center(Length::Fill),
        )
        .width(Length::Fixed(32.0))
        .height(Length::Fixed(30.0))
        .padding(0)
        .style(style::button_primary)
        .on_press_maybe(can_send.then_some(Message::Send));
        bar = bar.push(tooltip(send, send_tip, TooltipPosition::Top));
        container(bar).padding([SPACE_1, SPACE_2]).into()
    }

    fn notice_view<'a>(&'a self, block: PostBlock, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let channel = can_join(ctx.workspace, &self.rid);
        let text = match block {
            // A previewed channel or discussion: the Join bar.
            PostBlock::NotSubscribed if channel => t!("room_composer.preview_notice"),
            PostBlock::NotSubscribed => t!("room_composer.block.not_subscribed"),
            PostBlock::Archived => t!("room_composer.block.archived"),
            PostBlock::Muted => t!("room_composer.block.muted"),
            PostBlock::ReadOnly => t!("room_composer.block.read_only"),
            PostBlock::Blocker => t!("room_composer.block.you_blocked"),
            PostBlock::Blocked => t!("room_composer.block.blocked"),
        };
        let ic = match block {
            PostBlock::Archived => Icon::Archive,
            PostBlock::NotSubscribed => Icon::Info,
            _ => Icon::Lock,
        };
        let mut r = row![
            icon(ic).size(16.0).style(style::text_secondary),
            label(text).size(TEXT_SMALL).style(style::text_secondary),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        let joinable = block == PostBlock::NotSubscribed && channel;
        if joinable {
            r = r.push(space().width(Length::Fill));
            let with_code = needs_join_code(ctx.workspace, &self.rid);
            if with_code {
                r = r.push(
                    text_input(&t!("room_composer.join_code"), &self.join_code)
                        .on_input(Message::JoinCode)
                        .on_submit(Message::Join)
                        .secure(true)
                        .width(Length::Fixed(180.0))
                        .padding([6, 10])
                        .style(style::text_input),
                );
            }
            let ready = !self.joining && (!with_code || !self.join_code.trim().is_empty());
            r = r.push(
                primary_button(t!("room_composer.join"), ready.then_some(Message::Join))
                    .loading(self.joining),
            );
        }
        let card = container(r)
            .padding([SPACE_3, SPACE_4])
            .width(Length::Fill)
            .style(style::container_card);
        // Reactions stay possible in archived rooms and with reactWhenReadOnly.
        container(self.with_picker(card, ctx))
            .padding([SPACE_2, SPACE_4])
            .width(Length::Fill)
            .into()
    }
}

/// Edit/quote bar above the editor.
fn bar<'a>(ic: Icon, title: String, detail: String, on_close: Message) -> Element<'a, Message> {
    container(
        row![
            icon(ic).size(14.0).style(style::text_link),
            label(title).size(TEXT_CAPTION).font(font_semibold()),
            label(detail)
                .size(TEXT_CAPTION)
                .style(style::text_secondary)
                .width(Length::Fill)
                .wrapping(iced::widget::text::Wrapping::None),
            icon_button_with(
                Icon::X,
                Some(on_close),
                t!("room_composer.cancel"),
                false,
                22.0
            ),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center),
    )
    .padding([SPACE_1, SPACE_2])
    .style(style::container_quote)
    .into()
}

fn format_icon(f: Format) -> (Icon, String) {
    let (ic, key) = match f {
        Format::Bold => (Icon::Bold, t!("room_composer.format.bold")),
        Format::Italic => (Icon::Italic, t!("room_composer.format.italic")),
        Format::Strike => (Icon::Strikethrough, t!("room_composer.format.strike")),
        Format::Code => (Icon::Code, t!("room_composer.format.code")),
        Format::CodeBlock => (Icon::SquareCode, t!("room_composer.format.code_block")),
        Format::Link => (Icon::Link, t!("room_composer.format.link")),
        Format::Quote => (Icon::TextQuote, t!("room_composer.format.quote")),
    };
    match crate::keymap::format_chord(f) {
        Some(chord) => (ic, format!("{key} · {chord}")),
        None => (ic, key.into_owned()),
    }
}

fn editor_style(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let t = rustket_ui::tokens(theme);
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: t.text_hint,
        value: if status == text_editor::Status::Disabled {
            t.text_disabled
        } else {
            t.text_primary
        },
        selection: t.selection,
    }
}

/// Editor state the key bindings depend on.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Keys {
    /// The autocomplete popup is visible.
    pub(super) popup: bool,
    pub(super) empty: bool,
    /// Escape has something to cancel (edit, quotes).
    pub(super) cancellable: bool,
    /// Ctrl/Cmd+Shift+Enter schedules (else it breaks the line).
    pub(super) schedulable: bool,
}

/// Composer keys on top of the default text editor bindings.
pub(super) fn key_binding(kp: KeyPress, k: Keys) -> Option<Binding<Message>> {
    if !matches!(kp.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    let m = kp.modifiers;
    // App-wide chords first (Shift+Esc must not also cancel): swallowed here
    // so they neither type nor copy, and handled by the app listener, which
    // skips the two it only takes uncaptured (`keymap::from_event`).
    if let Some(s) = keymap::global(&kp.key, kp.physical_key, m) {
        return Some(match s {
            keymap::Shortcut::ReactLast => Binding::Custom(Message::ReactLast),
            keymap::Shortcut::MarkAllRead => Binding::Custom(Message::AskReadAll),
            _ => Binding::Sequence(Vec::new()),
        });
    }
    // Before the editor's own Ctrl+Shift+C / X (copy, cut ignore Shift).
    if let Some(f) = keymap::format(&kp.key, kp.physical_key, m, kp.text.as_deref()) {
        return Some(Binding::Custom(Message::Format(f)));
    }
    if let keyboard::Key::Named(named) = kp.key.as_ref() {
        match named {
            key::Named::Enter if k.schedulable && m.command() && m.shift() && !m.alt() => {
                return Some(Binding::Custom(Message::Schedule));
            }
            key::Named::Enter if !m.shift() && !m.alt() => {
                return Some(Binding::Custom(if k.popup {
                    Message::AcConfirm
                } else {
                    Message::Send
                }));
            }
            key::Named::Tab if k.popup && !m.shift() => {
                return Some(Binding::Custom(Message::AcConfirm));
            }
            key::Named::ArrowUp if k.popup => return Some(Binding::Custom(Message::AcMove(-1))),
            key::Named::ArrowDown if k.popup => return Some(Binding::Custom(Message::AcMove(1))),
            key::Named::ArrowUp if k.empty && !m.shift() && !m.command() => {
                return Some(Binding::Custom(Message::EditLast));
            }
            key::Named::Escape if k.popup || k.cancellable => {
                return Some(Binding::Custom(Message::Cancel));
            }
            // A custom binding captures the key: Escape that only unfocuses
            // the editor does not also close the right panel.
            key::Named::Escape if !m.shift() && !m.command() && !m.alt() => {
                return Some(Binding::Custom(Message::Leave));
            }
            _ => {}
        }
    }
    if m.command() && !m.alt() {
        match kp.key.to_latin(kp.physical_key) {
            Some('v') if !m.shift() => {
                // Files/images are uploaded; text is pasted only without them.
                return Some(Binding::Custom(Message::PasteCheck));
            }
            _ => {}
        }
    }
    // Shift+Enter falls through to the default line break.
    Binding::from_key_press(kp)
}
