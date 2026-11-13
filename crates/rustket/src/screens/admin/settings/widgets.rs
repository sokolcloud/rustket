//! One widget per setting type (docs/admin.md §14.5) with its label, hint
//! (inline Markdown), alert, reset button, EE/env notes, validation error
//! and remote-change conflict.

use iced::widget::text::Span;
use iced::widget::{
    Row, button, checkbox, column, container, image, pick_list, rich_text, row, slider, span, text,
    text_editor, text_input,
};
use iced::{Alignment, Background, Border, Color, Element, Font, Length};
use rocketchat::models::admin::{SettingDef, SettingType};
use rust_i18n::t;
use rustket_core::admin::server_i18n::LANGUAGES;
use rustket_core::admin::timespan::TimeUnit;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL, font_medium, font_mono,
    font_semibold,
};
use rustket_ui::widget::{
    BannerKind, ChipKind, banner, chip, ghost_button, icon_button, secondary_button,
};
use serde_json::Value;

use super::super::common::{disabled_value, readonly, strip_tags};
use super::super::{Message, V};
use super::{GroupView, Msg, State, room_ids};

/// Passes only pointer presses, drags and releases to `content`: the iced
/// slider also reacts to arrow keys and Ctrl+wheel whenever the pointer
/// merely rests over it (no focus needed), which would edit a setting while
/// the admin types or scrolls elsewhere.
fn pointer_only<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    Element::new(PointerOnly {
        content: content.into(),
    })
}

struct PointerOnly<'a> {
    content: Element<'a, Message>,
}

mod pointer_only_impl {
    use iced::advanced::layout::{self, Layout};
    use iced::advanced::widget::{Operation, Tree, Widget, tree};
    use iced::advanced::{Clipboard, Shell, overlay, renderer};
    use iced::{Event, Length, Rectangle, Size, Theme, Vector, mouse};

    use super::{Message, PointerOnly};

    impl Widget<Message, Theme, iced::Renderer> for PointerOnly<'_> {
        fn tag(&self) -> tree::Tag {
            self.content.as_widget().tag()
        }

        fn state(&self) -> tree::State {
            self.content.as_widget().state()
        }

        fn children(&self) -> Vec<Tree> {
            self.content.as_widget().children()
        }

        fn diff(&self, tree: &mut Tree) {
            self.content.as_widget().diff(tree);
        }

        fn size(&self) -> Size<Length> {
            self.content.as_widget().size()
        }

        fn size_hint(&self) -> Size<Length> {
            self.content.as_widget().size_hint()
        }

        fn layout(
            &mut self,
            tree: &mut Tree,
            renderer: &iced::Renderer,
            limits: &layout::Limits,
        ) -> layout::Node {
            self.content.as_widget_mut().layout(tree, renderer, limits)
        }

        fn draw(
            &self,
            tree: &Tree,
            renderer: &mut iced::Renderer,
            theme: &Theme,
            style: &renderer::Style,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            viewport: &Rectangle,
        ) {
            self.content
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        }

        fn operate(
            &mut self,
            tree: &mut Tree,
            layout: Layout<'_>,
            renderer: &iced::Renderer,
            operation: &mut dyn Operation,
        ) {
            self.content
                .as_widget_mut()
                .operate(tree, layout, renderer, operation);
        }

        fn update(
            &mut self,
            tree: &mut Tree,
            event: &Event,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            renderer: &iced::Renderer,
            clipboard: &mut dyn Clipboard,
            shell: &mut Shell<'_, Message>,
            viewport: &Rectangle,
        ) {
            if matches!(
                event,
                Event::Keyboard(_) | Event::Mouse(mouse::Event::WheelScrolled { .. })
            ) {
                return;
            }
            self.content.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }

        fn mouse_interaction(
            &self,
            tree: &Tree,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            viewport: &Rectangle,
            renderer: &iced::Renderer,
        ) -> mouse::Interaction {
            self.content
                .as_widget()
                .mouse_interaction(tree, layout, cursor, viewport, renderer)
        }

        fn overlay<'b>(
            &'b mut self,
            tree: &'b mut Tree,
            layout: Layout<'b>,
            renderer: &iced::Renderer,
            viewport: &Rectangle,
            translation: Vector,
        ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
            self.content
                .as_widget_mut()
                .overlay(tree, layout, renderer, viewport, translation)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub key: String,
    pub label: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit(pub TimeUnit);

impl std::fmt::Display for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&match self.0 {
            TimeUnit::Days => t!("admin.settings.days"),
            TimeUnit::Hours => t!("admin.settings.hours"),
            TimeUnit::Minutes => t!("admin.settings.minutes"),
        })
    }
}

/// IANA zones offered by the `timezone` widget (any other stored value is kept).
const TIMEZONES: &[&str] = &[
    "UTC",
    "Africa/Cairo",
    "Africa/Johannesburg",
    "Africa/Lagos",
    "America/Chicago",
    "America/Denver",
    "America/Los_Angeles",
    "America/Mexico_City",
    "America/New_York",
    "America/Sao_Paulo",
    "America/Toronto",
    "Asia/Dubai",
    "Asia/Hong_Kong",
    "Asia/Kolkata",
    "Asia/Shanghai",
    "Asia/Singapore",
    "Asia/Tokyo",
    "Australia/Sydney",
    "Europe/Berlin",
    "Europe/Istanbul",
    "Europe/Kyiv",
    "Europe/London",
    "Europe/Madrid",
    "Europe/Moscow",
    "Europe/Paris",
    "Pacific/Auckland",
];

fn s(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

/// `#rgb` / `#rrggbb` → colour.
pub fn parse_hex(s: &str) -> Option<Color> {
    let h = s.trim().strip_prefix('#')?;
    let h = match h.len() {
        3 => h.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => h.to_owned(),
        _ => return None,
    };
    let n = u32::from_str_radix(&h, 16).ok()?;
    Some(Color::from_rgb8((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

/// The setting row, or `None` for types that are never rendered (`group`).
pub fn setting<'a>(
    st: &'a State,
    g: &'a GroupView,
    id: &'a str,
    v: &V<'a>,
    conflict: bool,
) -> Option<Element<'a, Message>> {
    let store = &st.store;
    let def = store.get(id)?;
    if def.kind == SettingType::Group {
        return None;
    }
    let i18n = v.i18n;
    let m = Message::Settings;
    let editable = g.session.is_editable(store, &st.edit_ctx, id);
    let ee_locked = st.edit_ctx.is_ee_locked(def);
    let value = g.session.effective(store, id);
    let label = i18n.label_for(def);
    let sid = id.to_owned();
    let set = move |val: Value| Message::Settings(Msg::Set(sid.clone(), val));

    let mut head: Row<'a, Message> = row![
        text(label.clone())
            .size(TEXT_BODY)
            .font(font_medium())
            .width(Length::Fill)
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    if g.session.is_changed(store, id) {
        head = head.push(chip(t!("admin.settings.changed"), ChipKind::Primary));
    }
    if ee_locked {
        head = head.push(chip(t!("admin.premium"), ChipKind::Warning));
    }
    if g.session.can_reset(store, &st.edit_ctx, id) {
        head = head.push(icon_button(
            Icon::RotateCcw,
            Some(m(Msg::Reset(id.to_owned()))),
            t!("admin.settings.reset"),
        ));
    }

    let control: Option<Element<'a, Message>> = match &def.kind {
        SettingType::Boolean => {
            let on = value.and_then(Value::as_bool).unwrap_or(false);
            head = head.push(
                iced::widget::toggler(on)
                    .size(20)
                    .style(style::toggler)
                    .on_toggle_maybe(editable.then_some(move |b| set(Value::Bool(b)))),
            );
            None
        }
        SettingType::String | SettingType::Font | SettingType::Date if !def.multiline => {
            Some(text_box(
                &s_ref(value),
                def.placeholder.as_deref(),
                editable.then_some(set),
                false,
                None,
            ))
        }
        SettingType::String | SettingType::Code => {
            let code = def.kind == SettingType::Code;
            g.editors.get(id).map(|c| {
                let sid = id.to_owned();
                text_editor(c)
                    .on_action(move |a| Message::Settings(Msg::Editor(sid.clone(), a)))
                    .height(if code { 200 } else { 110 })
                    .font(if code { font_mono() } else { Font::DEFAULT })
                    .size(if code { TEXT_SMALL } else { TEXT_BODY })
                    .padding(SPACE_2)
                    .into()
            })
        }
        SettingType::RelativeUrl => {
            let site = store
                .value("Site_Url")
                .and_then(Value::as_str)
                .unwrap_or(v.ctx.server.as_str())
                .trim_end_matches('/')
                .to_owned();
            Some(readonly(format!(
                "{site}/{}",
                s(value).trim_start_matches('/')
            )))
        }
        SettingType::Password => {
            let shown = g.reveal.contains(id);
            Some(
                row![
                    text_box(&s_ref(value), None, editable.then_some(set), !shown, None),
                    icon_button(
                        if shown { Icon::EyeOff } else { Icon::Eye },
                        Some(m(Msg::Reveal(id.to_owned()))),
                        t!("admin.settings.reveal")
                    ),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center)
                .into(),
            )
        }
        SettingType::Int => {
            let sid = id.to_owned();
            let txt = g.texts.get(id).map(String::as_str).unwrap_or("");
            Some(text_box_ref(
                txt,
                editable.then_some(move |x: String| Message::Settings(Msg::Text(sid.clone(), x))),
            ))
        }
        SettingType::Select
        | SettingType::Language
        | SettingType::Lookup
        | SettingType::Timezone => {
            let mut opts: Vec<Choice> = match &def.kind {
                SettingType::Select => def
                    .values
                    .iter()
                    .map(|o| Choice {
                        key: o.key.clone(),
                        label: strip_tags(i18n.option_label(o)),
                    })
                    .collect(),
                SettingType::Language => std::iter::once(Choice {
                    key: String::new(),
                    label: t!("admin.settings.default_language").into_owned(),
                })
                .chain(LANGUAGES.iter().map(|(k, n)| Choice {
                    key: (*k).to_owned(),
                    label: format!("{n} ({k})"),
                }))
                .collect(),
                SettingType::Lookup => def
                    .lookup_endpoint
                    .as_ref()
                    .and_then(|ep| st.lookups.get(ep))
                    .map(|l| {
                        l.iter()
                            .map(|o| Choice {
                                key: o.key.clone(),
                                label: o.label.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => std::iter::once(Choice {
                    key: String::new(),
                    label: t!("admin.settings.default_timezone").into_owned(),
                })
                .chain(TIMEZONES.iter().map(|z| Choice {
                    key: (*z).to_owned(),
                    label: (*z).to_owned(),
                }))
                .collect(),
            };
            let cur = s(value);
            if !opts.iter().any(|o| o.key == cur) {
                opts.push(Choice {
                    key: cur.clone(),
                    label: if cur.is_empty() {
                        "—".into()
                    } else {
                        cur.clone()
                    },
                });
            }
            let sel = opts.iter().find(|o| o.key == cur).cloned();
            let sid = id.to_owned();
            let pick = pick_list(opts, sel, move |c: Choice| {
                Message::Settings(Msg::Set(sid.clone(), Value::String(c.key)))
            })
            .text_size(TEXT_BODY)
            .width(Length::Fill)
            .style(style::pick_list)
            .menu_style(style::menu);
            Some(if editable {
                pointer_only(pick)
            } else {
                // A disabled pick list: show the value only.
                disabled_value(sel_label(def, &cur, st, v))
            })
        }
        SettingType::MultiSelect => {
            let chosen: Vec<String> = value
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let mut r = row![].spacing(SPACE_3);
            for o in &def.values {
                let on = chosen.contains(&o.key);
                let (sid, key) = (id.to_owned(), o.key.clone());
                r = r.push(
                    checkbox(on)
                        .label(strip_tags(i18n.option_label(o)))
                        .size(16)
                        .text_size(TEXT_SMALL)
                        .style(style::checkbox)
                        .on_toggle_maybe(editable.then_some(move |b| {
                            Message::Settings(Msg::Multi(sid.clone(), key.clone(), b))
                        })),
                );
            }
            Some(r.wrap().into())
        }
        SettingType::Color => {
            let cur = s(value);
            let editor = g.session.effective_editor(store, id).map(str::to_owned);
            let swatch: Element<'a, Message> = match parse_hex(&cur) {
                Some(c) if editor.as_deref() != Some("expression") => container(text(""))
                    .width(28)
                    .height(28)
                    .style(move |_| container::Style {
                        background: Some(Background::Color(c)),
                        border: Border {
                            radius: 6.0.into(),
                            width: 1.0,
                            color: Color::from_rgba(0.0, 0.0, 0.0, 0.2),
                        },
                        ..container::Style::default()
                    })
                    .into(),
                _ => container(text("")).width(0).into(),
            };
            let mut r = row![
                swatch,
                text_box(
                    &cur,
                    None,
                    editable.then_some(set),
                    false,
                    Some(font_mono())
                )
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
            if def.editor.is_some() {
                let opts = vec![
                    Choice {
                        key: "color".into(),
                        label: t!("admin.settings.color").into_owned(),
                    },
                    Choice {
                        key: "expression".into(),
                        label: t!("admin.settings.expression").into_owned(),
                    },
                ];
                let sel = opts
                    .iter()
                    .find(|o| Some(o.key.as_str()) == editor.as_deref())
                    .cloned();
                let sid = id.to_owned();
                r = r.push(
                    pick_list(opts, sel, move |c: Choice| {
                        Message::Settings(Msg::SetEditor(sid.clone(), c.key))
                    })
                    .text_size(TEXT_SMALL)
                    .width(140)
                    .style(style::pick_list)
                    .menu_style(style::menu),
                );
            }
            Some(r.into())
        }
        SettingType::Action => {
            let label = def
                .action_text
                .as_deref()
                .map(|k| i18n.t_or_key(k).to_owned())
                .unwrap_or_else(|| t!("admin.settings.run").into_owned());
            let dirty = g.session.is_dirty(store);
            let can_run = !g.session.is_disabled(store, &st.edit_ctx, id) && !dirty && !g.busy;
            let mut c = column![secondary_button(
                label,
                can_run.then(|| m(Msg::Action(id.to_owned())))
            )]
            .spacing(SPACE_1);
            if dirty {
                c = c.push(
                    text(
                        i18n.t("Save_to_enable_this_action")
                            .map(str::to_owned)
                            .unwrap_or_else(|| t!("admin.settings.save_first").into_owned()),
                    )
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary),
                );
            }
            Some(c.into())
        }
        SettingType::Asset => {
            let url = value
                .and_then(|v| v.get("url"))
                .and_then(Value::as_str)
                .filter(|u| !u.is_empty());
            let can =
                rustket_core::permissions::has_permission(v.ctx.workspace, "manage-assets", None)
                    && !g.session.is_disabled(store, &st.edit_ctx, id);
            let preview: Element<'a, Message> = match url {
                Some(u) => {
                    let abs = v.ctx.server.absolute(&format!(
                        "/{}?rev={}",
                        u.trim_start_matches('/'),
                        st.asset_rev
                    ));
                    match v.ctx.images.get(v.ctx.server_id, &abs) {
                        Some(l) => image(l.handle.clone()).height(48).into(),
                        None => text(u.to_owned())
                            .size(TEXT_SMALL)
                            .style(style::text_secondary)
                            .into(),
                    }
                }
                None => text(t!("admin.settings.no_asset"))
                    .size(TEXT_SMALL)
                    .style(style::text_secondary)
                    .into(),
            };
            Some(
                row![
                    container(preview).width(Length::Fill),
                    secondary_button(
                        t!("admin.choose_file"),
                        (can && !g.busy).then(|| m(Msg::AssetPick(id.to_owned())))
                    )
                    .icon(Icon::Upload),
                    ghost_button(
                        t!("admin.delete"),
                        (can && url.is_some() && !g.busy)
                            .then(|| m(Msg::AssetDelete(id.to_owned())))
                    ),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center)
                .into(),
            )
        }
        SettingType::RoomPick => {
            let ids = room_ids(value);
            let mut chips = row![].spacing(SPACE_1);
            for rid in ids {
                let name = st
                    .room_names
                    .get(&rid)
                    .cloned()
                    .unwrap_or_else(|| rid.clone());
                chips = chips.push(
                    button(
                        row![
                            text(format!("#{name}")).size(TEXT_SMALL),
                            rustket_ui::icon::sized(Icon::X, 12.0)
                        ]
                        .spacing(SPACE_1)
                        .align_y(Alignment::Center),
                    )
                    .padding([2, 8])
                    .style(style::button_secondary)
                    .on_press_maybe(
                        editable.then(|| m(Msg::RoomRemove(id.to_owned(), rid.clone()))),
                    ),
                );
            }
            let q = g.room_query.get(id).map(String::as_str).unwrap_or("");
            let sid = id.to_owned();
            let mut c = column![
                chips.wrap(),
                text_box_ref(
                    q,
                    editable.then_some(move |x: String| Message::Settings(Msg::RoomQuery(
                        sid.clone(),
                        x
                    )))
                )
            ]
            .spacing(SPACE_2);
            if let Some(results) = g.room_results.get(id) {
                for r in results.iter().take(8) {
                    c = c.push(
                        button(text(format!("#{}", r.display_name())).size(TEXT_SMALL))
                            .padding([4, 8])
                            .width(Length::Fill)
                            .style(style::button_list_item(false))
                            .on_press(m(Msg::RoomAdd(
                                id.to_owned(),
                                r.id.clone(),
                                r.display_name(),
                            ))),
                    );
                }
            }
            Some(c.into())
        }
        SettingType::Range => {
            let lo = def.min_value.unwrap_or(0);
            let hi = def.max_value.unwrap_or(100).max(lo);
            let n = value.and_then(Value::as_i64).unwrap_or(lo).clamp(lo, hi);
            let sid = id.to_owned();
            let sl = slider(lo as f64..=hi as f64, n as f64, move |x: f64| {
                Message::Settings(Msg::Set(sid.clone(), Value::from(x.round() as i64)))
            })
            .step(1.0);
            Some(
                row![
                    if editable {
                        pointer_only(sl)
                    } else {
                        Element::from(slider(lo as f64..=hi as f64, n as f64, |_| Message::Noop))
                    },
                    text(n.to_string()).size(TEXT_BODY).width(48),
                ]
                .spacing(SPACE_3)
                .align_y(Alignment::Center)
                .into(),
            )
        }
        SettingType::Timespan => {
            let sid = id.to_owned();
            let sid2 = id.to_owned();
            let txt = g.texts.get(id).map(String::as_str).unwrap_or("");
            let unit = g.units.get(id).copied().unwrap_or(TimeUnit::Minutes);
            let mut r = row![text_box_ref(
                txt,
                editable.then_some(move |x: String| Message::Settings(Msg::Text(sid.clone(), x)))
            )]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
            r = r.push(
                pick_list(TimeUnit::ALL.map(Unit), Some(Unit(unit)), move |u: Unit| {
                    Message::Settings(Msg::Unit(sid2.clone(), u.0))
                })
                .text_size(TEXT_BODY)
                .width(140)
                .style(style::pick_list)
                .menu_style(style::menu),
            );
            Some(r.into())
        }
        SettingType::Group => None,
        // Unknown types: read-only text.
        _ => Some(readonly(s(value))),
    };

    let mut col = column![head].spacing(SPACE_1);
    if let Some(c) = control {
        col = col.push(c);
    }
    if let Some(e) = g.errors.get(id) {
        col = col.push(text(e.clone()).size(TEXT_CAPTION).style(style::text_danger));
    }
    if let Some(h) = i18n.hint_for(def) {
        col = col.push(markdown(h, TEXT_CAPTION, false));
    }
    if ee_locked
        && let Some(inv) = &def.invalid_value
        && Some(inv) != def.value.as_ref()
    {
        col = col.push(
            text(t!("admin.settings.effective", value = s(Some(inv))))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
        );
    }
    if def.value_source.as_deref() == Some("processEnvValue") {
        col = col.push(
            text(t!("admin.settings.env"))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
        );
    }
    if def.blocked {
        col = col.push(
            text(t!("admin.settings.blocked"))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
        );
    }
    if let Some(a) = i18n.alert_for(def) {
        col = col.push(banner(BannerKind::Warning, strip_tags(a), None));
    }
    if conflict {
        col = col.push(
            column![
                banner(
                    BannerKind::Warning,
                    t!("admin.settings.conflict").into_owned(),
                    None
                ),
                row![
                    ghost_button(
                        t!("admin.settings.keep_mine"),
                        Some(m(Msg::KeepMine(id.to_owned())))
                    ),
                    ghost_button(
                        t!("admin.settings.take_server"),
                        Some(m(Msg::TakeServer(id.to_owned())))
                    ),
                ]
                .spacing(SPACE_2),
            ]
            .spacing(SPACE_1),
        );
    }
    let _ = font_semibold();
    Some(col.into())
}

fn s_ref(v: Option<&Value>) -> String {
    s(v)
}

fn sel_label(def: &SettingDef, cur: &str, st: &State, v: &V<'_>) -> String {
    match &def.kind {
        SettingType::Select => def
            .values
            .iter()
            .find(|o| o.key == cur)
            .map(|o| strip_tags(v.i18n.option_label(o)))
            .unwrap_or_else(|| cur.to_owned()),
        SettingType::Lookup => def
            .lookup_endpoint
            .as_ref()
            .and_then(|ep| st.lookups.get(ep))
            .and_then(|l| l.iter().find(|o| o.key == cur))
            .map(|o| o.label.clone())
            .unwrap_or_else(|| cur.to_owned()),
        _ if cur.is_empty() => "—".to_owned(),
        _ => cur.to_owned(),
    }
}

/// Text input owning its value (the value is rebuilt per frame).
fn text_box<'a>(
    value: &str,
    placeholder: Option<&str>,
    on_input: Option<impl Fn(Value) -> Message + 'a>,
    secure: bool,
    font: Option<Font>,
) -> Element<'a, Message> {
    let mut i = text_input(placeholder.unwrap_or(""), value)
        .padding([6, 10])
        .size(TEXT_BODY)
        .secure(secure)
        .style(style::text_input);
    if let Some(f) = font {
        i = i.font(f);
    }
    if let Some(f) = on_input {
        i = i.on_input(move |s| f(Value::String(s)));
    }
    i.into()
}

fn text_box_ref<'a>(
    value: &'a str,
    on_input: Option<impl Fn(String) -> Message + 'a>,
) -> Element<'a, Message> {
    let mut i = text_input("", value)
        .padding([6, 10])
        .size(TEXT_BODY)
        .style(style::text_input);
    if let Some(f) = on_input {
        i = i.on_input(f);
    }
    i.into()
}

/// Inline Markdown of server hints: `` `code` ``, `**bold**`, `[text](url)`.
/// `{{…}}` is left as is. Links send [`Msg::Link`].
pub fn markdown<'a>(src: &str, size: f32, _block: bool) -> Element<'a, Message> {
    let mut spans: Vec<Span<'a, Message>> = Vec::new();
    let mut rest = strip_tags(src);
    let push_plain = |spans: &mut Vec<Span<'a, Message>>, s: &str| {
        if !s.is_empty() {
            spans.push(span(s.to_owned()));
        }
    };
    loop {
        let next = ["`", "**", "["]
            .iter()
            .filter_map(|p| rest.find(p).map(|i| (i, *p)))
            .min_by_key(|(i, _)| *i);
        let Some((i, p)) = next else {
            push_plain(&mut spans, &rest);
            break;
        };
        let (before, after) = rest.split_at(i);
        let after = &after[p.len()..];
        match p {
            "`" => match after.find('`') {
                Some(j) => {
                    push_plain(&mut spans, before);
                    spans.push(span(after[..j].to_owned()).font(font_mono()));
                    rest = after[j + 1..].to_owned();
                }
                None => {
                    push_plain(&mut spans, &rest);
                    break;
                }
            },
            "**" => match after.find("**") {
                Some(j) => {
                    push_plain(&mut spans, before);
                    spans.push(span(after[..j].to_owned()).font(font_semibold()));
                    rest = after[j + 2..].to_owned();
                }
                None => {
                    push_plain(&mut spans, &rest);
                    break;
                }
            },
            _ => {
                let parsed = after.find("](").and_then(|j| {
                    let close = after[j + 2..].find(')')?;
                    Some((j, j + 2 + close))
                });
                match parsed {
                    Some((j, k)) => {
                        push_plain(&mut spans, before);
                        let label = after[..j].to_owned();
                        let url = after[j + 2..k].to_owned();
                        spans.push(
                            span(label)
                                .underline(true)
                                .link(Message::Settings(Msg::Link(url))),
                        );
                        rest = after[k + 1..].to_owned();
                    }
                    None => {
                        push_plain(&mut spans, &format!("{before}["));
                        rest = after.to_owned();
                    }
                }
            }
        }
    }
    rich_text(spans)
        .on_link_click(|m: Message| m)
        .size(size)
        .style(style::text_secondary)
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex("#fff"), Some(Color::from_rgb8(255, 255, 255)));
        assert_eq!(
            parse_hex("#156FF5"),
            Some(Color::from_rgb8(0x15, 0x6f, 0xf5))
        );
        assert_eq!(parse_hex("red"), None);
    }
}
