//! Element widgets (docs/uikit.md §2.4). Each is a function of the element,
//! its current value, its loading/error state and the surface scratch; the
//! value it sends is exactly what the web sends.

use std::collections::{BTreeSet, HashMap};

use chrono::{Datelike, NaiveDate};
use iced::widget::{
    Column, button, checkbox, column, container, mouse_area, row, scrollable, space, text,
    text_editor, text_input, toggler,
};
use iced::{Alignment, Element, Length, Theme, border};
use rocketchat::uikit::{
    Actionable, Element as El, Icon as UiIcon, OptionGroup, TextObject, UiOption,
};
use rust_i18n::t;
use rustket_core::room_list::Avatar;
use rustket_core::uikit::Permit;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, RADIUS_MD, RADIUS_SM, SPACE_1, SPACE_2, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL,
    tokens,
};
use rustket_ui::widget::{
    AvatarShape, ButtonKind, ButtonSize, MenuEntry, MenuItem, Placement, TooltipPosition,
    action_button, avatar, ellipsis, icon_button_with, popover, popover_menu, tooltip,
};
use serde_json::Value;

use super::render::{Ctx, Env, square_image};
use super::{Typed, UiKitMsg, text as tx};
use crate::room::render::{RenderMsg, safe_url};

/// Multiline inputs of the modal (`text_editor` contents by scratch key).
pub type Editors = HashMap<String, text_editor::Content>;

/// Width of a select / picker that does not grow.
const FIELD_WIDTH: f32 = 220.0;
/// Most points of a linear scale.
const MAX_SCALE_POINTS: i64 = rocketchat::uikit::blocks::MAX_SCALE_POINTS;

type W = Element<'static, UiKitMsg>;

fn width(grow: bool) -> Length {
    if grow {
        Length::Fill
    } else {
        Length::Fixed(FIELD_WIDTH)
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn string(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The host of `url` for a link tooltip.
fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}

/// The message a change of `a` publishes: an emitting change needs the trust
/// rule's blessing; a store-only one always goes through.
fn change(env: &Env<'_>, a: &Actionable, ctx: Ctx, value: Option<Value>) -> Option<UiKitMsg> {
    if !env.enabled(a, ctx) {
        return None;
    }
    Some(UiKitMsg::Change {
        req: env.req(a, value),
        emit: env.emits(a, ctx),
        confirm: a.confirm.clone().map(Box::new),
    })
}

/// One element; `None` for elements this context does not render.
pub fn element<'e>(
    env: &Env<'_>,
    editors: Option<&'e Editors>,
    e: &El,
    ctx: Ctx,
    grow: bool,
) -> Option<Element<'e, UiKitMsg>> {
    let w: W = match e {
        El::Button {
            a,
            text: label,
            url,
            value,
            style: st,
            ..
        } => button_el(
            env,
            a,
            label,
            url.as_deref(),
            value.as_deref(),
            st.as_deref(),
            ctx,
        ),
        El::Overflow { a, options } => overflow(env, a, options, ctx),
        El::StaticSelect {
            a,
            placeholder,
            options,
            groups,
            ..
        } => select(env, e, a, placeholder.as_ref(), options, groups, ctx, grow),
        El::MultiStaticSelect {
            a,
            placeholder,
            options,
            groups,
            ..
        } => multi_select(env, e, a, placeholder.as_ref(), options, groups, ctx, grow),
        El::UsersSelect {
            a,
            placeholder,
            multi,
        } => people_select(env, e, a, placeholder.as_ref(), *multi, true, ctx, grow),
        // The web renders no placeholder for channel selects (§2.1).
        El::ChannelsSelect { a, multi, .. } => {
            people_select(env, e, a, None, *multi, false, ctx, grow)
        }
        El::DatePicker { a, placeholder, .. } => {
            date_picker(env, e, a, placeholder.as_ref(), ctx, grow)
        }
        El::TimePicker { a, placeholder, .. } => {
            time_picker(env, e, a, placeholder.as_ref(), ctx, grow)
        }
        El::PlainTextInput {
            a,
            placeholder,
            multiline,
            ..
        } => {
            if *multiline && let Some(content) = editors.and_then(|m| m.get(&env.key(&a.action_id)))
            {
                return Some(editor(env, a, content, placeholder.as_ref(), ctx));
            }
            text_field(env, e, a, placeholder.as_ref(), *multiline, ctx)
        }
        El::LinearScale {
            a,
            min,
            max,
            pre,
            post,
            ..
        } => linear_scale(env, e, a, *min, *max, pre.as_ref(), post.as_ref(), ctx),
        El::Checkbox { a, options, .. } => checks(env, e, a, options, ctx, false),
        El::ToggleSwitch { a, options, .. } => checks(env, e, a, options, ctx, true),
        El::RadioButton { a, options, .. } => radios(env, e, a, options, ctx),
        El::Image {
            image_url,
            alt_text,
        } => match ctx {
            Ctx::Section => square_image(env, image_url, alt_text, 88.0),
            _ => return None,
        },
        El::Icon(i) => icon(i),
        El::IconButton {
            a,
            icon: i,
            label,
            url,
            value,
        } => icon_button_el(
            env,
            a,
            i,
            label.as_deref(),
            url.as_deref(),
            value.as_deref(),
            ctx,
        ),
        El::ConversationsSelect { .. } | El::Tab { .. } | El::Text(_) | El::Unknown(_) => {
            return None;
        }
    };
    Some(w)
}

fn button_el(
    env: &Env<'_>,
    a: &Actionable,
    label: &TextObject,
    url: Option<&str>,
    value: Option<&str>,
    st: Option<&str>,
    ctx: Ctx,
) -> W {
    let link = url.and_then(safe_url);
    let on = press(env, a, ctx, link.clone(), url.is_some(), value);
    let kind = match st {
        Some("primary") => ButtonKind::Primary,
        Some("danger") => ButtonKind::Danger,
        _ => ButtonKind::Secondary,
    };
    let mut b = action_button(kind, tx::label(env, label, None), on)
        .size(ButtonSize::Small)
        .loading(env.loading(a));
    if link.is_some() {
        b = b.icon(Icon::ExternalLink);
    }
    let el: W = b.into();
    match link {
        Some(u) => tooltip(el, host(&u), TooltipPosition::Top),
        None => el,
    }
}

/// What pressing a button / icon button publishes (§2.4, §3.2): a URL
/// button sends no `value`, a button without `value` sends `""`.
fn press(
    env: &Env<'_>,
    a: &Actionable,
    ctx: Ctx,
    link: Option<String>,
    has_url: bool,
    value: Option<&str>,
) -> Option<UiKitMsg> {
    if env.disabled || env.loading(a) {
        return None;
    }
    let value = (!has_url).then(|| Value::String(value.unwrap_or_default().to_owned()));
    let open = link
        .clone()
        .map(|u| UiKitMsg::Render(Box::new(RenderMsg::OpenUrl(u))));
    if !env.emits(a, ctx) {
        // Context-block buttons only touch view state (nothing in messages).
        return match (&env.state, open) {
            (_, Some(o)) => Some(o),
            (Some(_), None) => Some(UiKitMsg::Change {
                req: env.req(a, value),
                emit: false,
                confirm: None,
            }),
            (None, None) => None,
        };
    }
    match env.permit(a, link.is_some()) {
        Permit::Emit => {
            let mut req = env.req(a, value);
            req.url = link;
            Some(UiKitMsg::Change {
                req,
                emit: true,
                confirm: a.confirm.clone().map(Box::new),
            })
        }
        Permit::OpenUrlOnly => open,
        Permit::Local | Permit::Disabled => None,
    }
}

fn overflow(env: &Env<'_>, a: &Actionable, options: &[UiOption], ctx: Ctx) -> W {
    let key = env.key(&a.action_id);
    let open = env.scratch().open.as_deref() == Some(key.as_str());
    let enabled = env.enabled(a, ctx);
    let entries: Vec<MenuEntry<UiKitMsg>> = options
        .iter()
        .map(|o| {
            let link = o.url.as_deref().and_then(safe_url);
            let msg = enabled.then(|| {
                let mut req = env.req(a, Some(Value::String(o.value.clone())));
                req.url = link.clone();
                UiKitMsg::Change {
                    req,
                    emit: env.emits(a, ctx),
                    confirm: a.confirm.clone().map(Box::new),
                }
            });
            let item = MenuItem::new(
                tx::label(env, &o.text, None),
                msg.clone().unwrap_or(UiKitMsg::Popover(None)),
            )
            .disabled(msg.is_none());
            match link {
                Some(_) => item.icon(Icon::ExternalLink).into(),
                None => item.into(),
            }
        })
        .collect();
    let trigger = icon_button_with(
        Icon::EllipsisVertical,
        enabled.then(|| UiKitMsg::Popover((!open).then(|| key.clone()))),
        t!("uikit.more").into_owned(),
        open,
        28.0,
    );
    popover_menu(
        trigger,
        &entries,
        open,
        UiKitMsg::Popover(None),
        Placement::BelowEnd,
    )
}

/// The trigger of a select: current text (or placeholder) and a chevron.
fn select_trigger(
    label: Option<String>,
    placeholder: String,
    error: bool,
    on: Option<UiKitMsg>,
    grow: bool,
) -> W {
    let shown = label.is_some();
    let content = row![
        ellipsis(label.unwrap_or(placeholder))
            .size(TEXT_BODY)
            .width(Length::Fill)
            .style(move |th: &Theme| Some(if shown {
                tokens(th).text_primary
            } else {
                tokens(th).text_hint
            })),
        icon::sized(Icon::ChevronDown, 14.0),
    ]
    .spacing(SPACE_1)
    .align_y(Alignment::Center);
    button(content)
        .width(width(grow))
        .padding([6, 10])
        .style(move |th: &Theme, st| {
            let mut s = style::button_secondary(th, st);
            if error {
                s.border = s.border.color(tokens(th).danger);
            }
            s
        })
        .on_press_maybe(on)
        .into()
}

fn option_label(
    env: &Env<'_>,
    options: &[UiOption],
    groups: &[OptionGroup],
    value: &str,
) -> Option<String> {
    options
        .iter()
        .chain(groups.iter().flat_map(|g| g.options.iter()))
        .find(|o| o.value == value)
        .map(|o| tx::label(env, &o.text, None))
}

/// The filterable option list of an open select.
#[allow(clippy::too_many_arguments)]
fn option_list(
    env: &Env<'_>,
    options: &[UiOption],
    groups: &[OptionGroup],
    selected: &BTreeSet<String>,
    pick: impl Fn(&str) -> Option<UiKitMsg>,
) -> W {
    let filter = env.scratch().filter.to_lowercase();
    let mut list: Column<'static, UiKitMsg> = column![].spacing(0);
    let mut any = false;
    let mut push = |list: Column<'static, UiKitMsg>, o: &UiOption| -> Column<'static, UiKitMsg> {
        let label = tx::label(env, &o.text, None);
        if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
            return list;
        }
        any = true;
        let is_sel = selected.contains(&o.value);
        let mut r = row![ellipsis(label).size(TEXT_BODY).width(Length::Fill)]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
        if is_sel {
            r = r.push(icon::sized(Icon::Check, 14.0));
        }
        list.push(
            button(r)
                .width(Length::Fill)
                .padding([6, 8])
                .style(style::button_menu_item(false))
                .on_press_maybe(pick(&o.value)),
        )
    };
    for o in options {
        list = push(list, o);
    }
    for g in groups {
        list = list.push(
            container(
                text(tx::label(env, &g.label, None))
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
            )
            .padding([SPACE_1, SPACE_2]),
        );
        for o in &g.options {
            list = push(list, o);
        }
    }
    if !any {
        list = list.push(
            container(
                text(t!("uikit.no_options").into_owned())
                    .size(TEXT_SMALL)
                    .style(style::text_hint),
            )
            .padding(SPACE_2),
        );
    }
    let filter_field = text_input(&t!("uikit.filter"), &env.scratch().filter)
        .on_input(UiKitMsg::Filter)
        .size(TEXT_SMALL)
        .padding([4, 8])
        .style(style::text_input);
    container(
        column![
            filter_field,
            scrollable(list)
                .height(Length::Shrink)
                .style(style::scrollable),
        ]
        .spacing(SPACE_1),
    )
    .padding(SPACE_1)
    .width(FIELD_WIDTH + 40.0)
    .max_height(320)
    .style(style::container_popover)
    .into()
}

#[allow(clippy::too_many_arguments)]
fn select(
    env: &Env<'_>,
    e: &El,
    a: &Actionable,
    placeholder: Option<&TextObject>,
    options: &[UiOption],
    groups: &[OptionGroup],
    ctx: Ctx,
    grow: bool,
) -> W {
    let key = env.key(&a.action_id);
    let open = env.scratch().open.as_deref() == Some(key.as_str());
    let current = string(env.value(e).as_ref());
    let label = current
        .as_deref()
        .and_then(|v| option_label(env, options, groups, v));
    let enabled = env.enabled(a, ctx);
    let placeholder = placeholder
        .map(|p| tx::label(env, p, None))
        .unwrap_or_default();
    let trigger = select_trigger(
        label,
        placeholder,
        env.error(&a.action_id).is_some(),
        enabled.then(|| UiKitMsg::Popover((!open).then(|| key.clone()))),
        grow,
    );
    let selected: BTreeSet<String> = current.into_iter().collect();
    let popup = option_list(env, options, groups, &selected, |v| {
        change(env, a, ctx, Some(Value::String(v.to_owned())))
    });
    popover(
        trigger,
        popup,
        open,
        Placement::BelowStart,
        UiKitMsg::Popover(None),
    )
}

#[allow(clippy::too_many_arguments)]
fn multi_select(
    env: &Env<'_>,
    e: &El,
    a: &Actionable,
    placeholder: Option<&TextObject>,
    options: &[UiOption],
    groups: &[OptionGroup],
    ctx: Ctx,
    grow: bool,
) -> W {
    // The `#multi` suffix keeps the list open while toggling.
    let key = format!("{}#multi", env.key(&a.action_id));
    let open = env.scratch().open.as_deref() == Some(key.as_str());
    let current = strings(env.value(e).as_ref());
    let enabled = env.enabled(a, ctx);
    let labels: Vec<String> = current
        .iter()
        .filter_map(|v| option_label(env, options, groups, v))
        .collect();
    let label = (!labels.is_empty()).then(|| labels.join(", "));
    let placeholder = placeholder
        .map(|p| tx::label(env, p, None))
        .unwrap_or_default();
    let trigger = select_trigger(
        label,
        placeholder,
        env.error(&a.action_id).is_some(),
        enabled.then(|| UiKitMsg::Popover((!open).then(|| key.clone()))),
        grow,
    );
    let selected: BTreeSet<String> = current.iter().cloned().collect();
    let popup = option_list(env, options, groups, &selected, |v| {
        let mut next = current.clone();
        match next.iter().position(|x| x == v) {
            Some(i) => {
                next.remove(i);
            }
            None => next.push(v.to_owned()),
        }
        change(
            env,
            a,
            ctx,
            Some(Value::Array(next.into_iter().map(Value::String).collect())),
        )
    });
    popover(
        trigger,
        popup,
        open,
        Placement::BelowStart,
        UiKitMsg::Popover(None),
    )
}

fn small_avatar(env: &Env<'_>, av: &Avatar, name: &str) -> W {
    let src = env
        .rc
        .images
        .avatar(env.rc.server_id, env.rc.server, av, name);
    avatar(src, 20.0, AvatarShape::Rounded).into()
}

fn chip_x(label: String, on: Option<UiKitMsg>) -> W {
    container(
        row![
            text(label).size(TEXT_SMALL),
            button(icon::sized(Icon::X, 12.0))
                .padding(0)
                .style(style::button_bare)
                .on_press_maybe(on),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center),
    )
    .padding([2, 6])
    .style(|th: &Theme| {
        let t = tokens(th);
        container::Style {
            background: Some(t.surface_sunken.into()),
            border: border::rounded(RADIUS_SM).width(1).color(t.border),
            ..container::Style::default()
        }
    })
    .into()
}

/// Users / channels autocomplete (FORM context only, §2.5): values are
/// usernames / room ids.
#[allow(clippy::too_many_arguments)]
fn people_select(
    env: &Env<'_>,
    e: &El,
    a: &Actionable,
    placeholder: Option<&TextObject>,
    multi: bool,
    users: bool,
    ctx: Ctx,
    grow: bool,
) -> W {
    let key = env.key(&a.action_id);
    let s = env.scratch();
    let enabled = env.enabled(a, ctx);
    let current = strings(env.value(e).as_ref());
    let label_of = |v: &str| {
        s.labels
            .get(&format!("{key}|{v}"))
            .cloned()
            .unwrap_or_else(|| v.to_owned())
    };
    let query = s.texts.get(&key).cloned().unwrap_or_default();
    let field = text_input(
        &placeholder.map(|p| p.text.clone()).unwrap_or_default(),
        &query,
    )
    .on_input_maybe(enabled.then(|| {
        let req = env.req(a, None);
        move |text| UiKitMsg::Query {
            req: req.clone(),
            text,
            users,
        }
    }))
    .size(TEXT_BODY)
    .padding([6, 10])
    .width(width(grow))
    .style(if env.error(&a.action_id).is_some() {
        style::text_input_error
    } else {
        style::text_input
    });
    let suggestions = s.suggestions.get(&key).cloned().unwrap_or_default();
    let open = s.open.as_deref() == Some(key.as_str()) && !suggestions.is_empty();
    let mut list: Column<'static, UiKitMsg> = column![].spacing(0);
    for sug in suggestions {
        let value = if multi {
            let mut next = current.clone();
            if !next.contains(&sug.value) {
                next.push(sug.value.clone());
            }
            Value::Array(next.into_iter().map(Value::String).collect())
        } else {
            Value::String(sug.value.clone())
        };
        let mut r = row![].spacing(SPACE_2).align_y(Alignment::Center);
        if let Some(av) = &sug.avatar {
            r = r.push(small_avatar(env, av, &sug.label));
        }
        r = r.push(ellipsis(sug.label.clone()).size(TEXT_BODY));
        if let Some(d) = &sug.detail {
            r = r.push(text(d.clone()).size(TEXT_CAPTION).style(style::text_hint));
        }
        list = list.push(
            button(r)
                .width(Length::Fill)
                .padding([6, 8])
                .style(style::button_menu_item(false))
                .on_press_maybe(change(env, a, ctx, Some(value))),
        );
    }
    let popup = container(scrollable(list).style(style::scrollable))
        .padding(SPACE_1)
        .width(FIELD_WIDTH + 40.0)
        .max_height(280)
        .style(style::container_popover);
    let mut col = column![popover(
        field,
        popup,
        open,
        Placement::BelowStart,
        UiKitMsg::Popover(None)
    )]
    .spacing(SPACE_1);
    if !current.is_empty() {
        let mut chips = row![].spacing(SPACE_1);
        for v in &current {
            let rest: Vec<Value> = current
                .iter()
                .filter(|x| *x != v)
                .cloned()
                .map(Value::String)
                .collect();
            let remove = multi
                .then(|| change(env, a, ctx, Some(Value::Array(rest))))
                .flatten();
            let label = label_of(v);
            let el: W = if multi {
                chip_x(label, remove)
            } else {
                let av = if users {
                    Avatar::User {
                        username: v.clone(),
                    }
                } else {
                    Avatar::Room {
                        rid: v.clone(),
                        etag: None,
                    }
                };
                row![small_avatar(env, &av, &label), text(label).size(TEXT_SMALL)]
                    .spacing(SPACE_1)
                    .align_y(Alignment::Center)
                    .into()
            };
            chips = chips.push(el);
        }
        col = col.push(chips.wrap());
    }
    col.into()
}

/// A text field that turns valid typed text into a value (date / time).
#[allow(clippy::too_many_arguments)]
fn typed_field(
    env: &Env<'_>,
    a: &Actionable,
    current: Option<String>,
    placeholder: String,
    kind: Typed,
    ctx: Ctx,
    grow: bool,
    picker: (Icon, String, W),
) -> W {
    let key = env.key(&a.action_id);
    let s = env.scratch();
    let shown = s.texts.get(&key).cloned().or(current).unwrap_or_default();
    let enabled = env.enabled(a, ctx);
    let emit = env.emits(a, ctx);
    let field = text_input(&placeholder, &shown)
        .on_input_maybe(enabled.then(|| {
            let req = env.req(a, None);
            move |text| UiKitMsg::Typing {
                req: req.clone(),
                text,
                emit,
                kind,
            }
        }))
        .size(TEXT_BODY)
        .padding([6, 10])
        .width(Length::Fill)
        .style(if env.error(&a.action_id).is_some() {
            style::text_input_error
        } else {
            style::text_input
        });
    let (ic, pkey, popup) = picker;
    let open = s.open.as_deref() == Some(pkey.as_str());
    let toggle = icon_button_with(
        ic,
        enabled.then(|| UiKitMsg::Popover((!open).then(|| pkey.clone()))),
        String::new(),
        open,
        30.0,
    );
    let r = row![
        field,
        popover(
            toggle,
            popup,
            open,
            Placement::BelowEnd,
            UiKitMsg::Popover(None)
        )
    ]
    .spacing(SPACE_1)
    .align_y(Alignment::Center);
    container(r).width(width(grow)).into()
}

fn date_picker(
    env: &Env<'_>,
    e: &El,
    a: &Actionable,
    placeholder: Option<&TextObject>,
    ctx: Ctx,
    grow: bool,
) -> W {
    let current = string(env.value(e).as_ref());
    let key = env.key(&a.action_id);
    let month_key = format!("{key}#month");
    let picked = current
        .as_deref()
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let month = env
        .scratch()
        .texts
        .get(&month_key)
        .and_then(|m| NaiveDate::parse_from_str(&format!("{m}-01"), "%Y-%m-%d").ok())
        .or_else(|| picked.and_then(|d| d.with_day(1)))
        .unwrap_or_else(|| {
            let today = chrono::Local::now().date_naive();
            today.with_day(1).unwrap_or(today)
        });
    let placeholder = placeholder
        .map(|p| tx::label(env, p, None))
        .unwrap_or_else(|| "YYYY-MM-DD".into());
    let popup = calendar(env, a, ctx, month, picked);
    typed_field(
        env,
        a,
        current,
        placeholder,
        Typed::Date,
        ctx,
        grow,
        (Icon::Calendar, format!("{key}#cal"), popup),
    )
}

/// Month grid of a date picker.
fn calendar(
    env: &Env<'_>,
    a: &Actionable,
    ctx: Ctx,
    month: NaiveDate,
    picked: Option<NaiveDate>,
) -> W {
    let nav = |m: Option<NaiveDate>, ic: Icon| -> W {
        let msg = m.map(|m| UiKitMsg::Typing {
            req: env.req(a, None),
            text: m.format("%Y-%m").to_string(),
            emit: false,
            kind: Typed::Month,
        });
        icon_button_with(ic, msg, String::new(), false, 26.0)
    };
    let prev = month.checked_sub_months(chrono::Months::new(1));
    let next = month.checked_add_months(chrono::Months::new(1));
    let head = row![
        nav(prev, Icon::ChevronLeft),
        text(month.format("%Y-%m").to_string())
            .size(TEXT_SMALL)
            .font(FONT_SEMIBOLD)
            .width(Length::Fill)
            .align_x(Alignment::Center),
        nav(next, Icon::ChevronRight),
    ]
    .align_y(Alignment::Center);
    let mut grid = column![].spacing(2);
    let offset = month.weekday().num_days_from_monday();
    let mut day = month;
    let mut cells: Vec<W> = (0..offset)
        .map(|_| space().width(30).height(26).into())
        .collect();
    while day.month() == month.month() {
        let d = day;
        let sel = picked == Some(d);
        let label = text(d.day().to_string())
            .size(TEXT_SMALL)
            .align_x(Alignment::Center);
        cells.push(
            button(container(label).center_x(Length::Fill))
                .width(30)
                .height(26)
                .padding([4, 0])
                .style(if sel {
                    style::button_primary
                } else {
                    style::button_ghost
                })
                .on_press_maybe(change(
                    env,
                    a,
                    ctx,
                    Some(Value::String(d.format("%Y-%m-%d").to_string())),
                ))
                .into(),
        );
        match day.succ_opt() {
            Some(n) => day = n,
            None => break,
        }
    }
    let mut cells = cells.into_iter().peekable();
    while cells.peek().is_some() {
        let week: Vec<W> = cells.by_ref().take(7).collect();
        grid = grid.push(row(week).spacing(2));
    }
    container(column![head, grid].spacing(SPACE_1))
        .padding(SPACE_2)
        .style(style::container_popover)
        .into()
}

fn time_picker(
    env: &Env<'_>,
    e: &El,
    a: &Actionable,
    placeholder: Option<&TextObject>,
    ctx: Ctx,
    grow: bool,
) -> W {
    let current = string(env.value(e).as_ref());
    let key = env.key(&a.action_id);
    let (h, m) = current.as_deref().and_then(parse_time).unwrap_or((0, 0));
    let cell = |label: String, value: String, sel: bool| -> W {
        button(text(label).size(TEXT_SMALL))
            .width(34)
            .padding([3, 6])
            .style(if sel {
                style::button_primary
            } else {
                style::button_ghost
            })
            .on_press_maybe(change(env, a, ctx, Some(Value::String(value))))
            .into()
    };
    let has = current.is_some();
    let hours: Vec<W> = (0..24)
        .map(|x| cell(format!("{x:02}"), format!("{x:02}:{m:02}"), has && x == h))
        .collect();
    let minutes: Vec<W> = (0..12)
        .map(|i| i * 5)
        .map(|x| cell(format!(":{x:02}"), format!("{h:02}:{x:02}"), has && x == m))
        .collect();
    let grid = |cells: Vec<W>| -> W {
        let mut col = column![].spacing(2);
        let mut it = cells.into_iter().peekable();
        while it.peek().is_some() {
            col = col.push(row(it.by_ref().take(6).collect::<Vec<_>>()).spacing(2));
        }
        col.into()
    };
    let popup: W = container(column![grid(hours), grid(minutes)].spacing(SPACE_2))
        .padding(SPACE_2)
        .style(style::container_popover)
        .into();
    let placeholder = placeholder
        .map(|p| tx::label(env, p, None))
        .unwrap_or_else(|| "HH:MM".into());
    typed_field(
        env,
        a,
        current,
        placeholder,
        Typed::Time,
        ctx,
        grow,
        (Icon::Clock, format!("{key}#time"), popup),
    )
}

/// `HH:MM` (24 h) → (hour, minute).
pub fn parse_time(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.trim().split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

/// A valid `YYYY-MM-DD`.
pub fn parse_date(s: &str) -> Option<String> {
    let s = s.trim();
    (s.len() == 10)
        .then(|| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .flatten()
        .map(|d| d.format("%Y-%m-%d").to_string())
}

fn dispatches(a: &Actionable, ctx: Ctx) -> bool {
    a.dispatches_on_character() || matches!(ctx, Ctx::Action | Ctx::Section)
}

fn text_field(
    env: &Env<'_>,
    e: &El,
    a: &Actionable,
    placeholder: Option<&TextObject>,
    multiline: bool,
    ctx: Ctx,
) -> W {
    let key = env.key(&a.action_id);
    let value = if env.state.is_some() {
        string(env.value(e).as_ref()).unwrap_or_default()
    } else {
        env.scratch()
            .texts
            .get(&key)
            .cloned()
            .or_else(|| string(env.value(e).as_ref()))
            .unwrap_or_default()
    };
    let dispatch = dispatches(a, ctx);
    // Typing never waits for the app (`on_character_entered` inputs are not
    // disabled while a request runs).
    let enabled = !env.disabled;
    let req = env.req(a, None);
    let mut field = text_input(
        &placeholder
            .map(|p| tx::label(env, p, None))
            .unwrap_or_default(),
        &value,
    )
    .id(iced::widget::Id::from(key))
    .on_input_maybe(enabled.then_some(move |text| {
        UiKitMsg::Text {
            req: req.clone(),
            dispatch,
        }
        .with_text(text)
    }))
    .size(TEXT_BODY)
    .padding(if multiline { [10, 10] } else { [6, 10] })
    .style(if env.error(&a.action_id).is_some() {
        style::text_input_error
    } else {
        style::text_input
    });
    if env.submits && !multiline {
        field = field.on_submit(UiKitMsg::Submit(env.surface.clone()));
    }
    field.into()
}

impl UiKitMsg {
    /// A `Text` message with the typed text as its value.
    fn with_text(mut self, text: String) -> Self {
        if let UiKitMsg::Text { req, .. } = &mut self {
            req.value = Some(Value::String(text));
        }
        self
    }
}

fn editor<'e>(
    env: &Env<'_>,
    a: &Actionable,
    content: &'e text_editor::Content,
    placeholder: Option<&TextObject>,
    ctx: Ctx,
) -> Element<'e, UiKitMsg> {
    let req = env.req(a, None);
    let dispatch = dispatches(a, ctx);
    let enabled = !env.disabled;
    let mut ed = text_editor(content)
        .placeholder(
            placeholder
                .map(|p| tx::label(env, p, None))
                .unwrap_or_default(),
        )
        .height(Length::Fixed(132.0))
        .padding(10)
        .size(TEXT_BODY);
    if enabled {
        ed = ed.on_action(move |action| UiKitMsg::Editor {
            req: req.clone(),
            dispatch,
            action,
        });
    }
    ed.into()
}

#[allow(clippy::too_many_arguments)]
fn linear_scale(
    env: &Env<'_>,
    e: &El,
    a: &Actionable,
    min: i64,
    max: i64,
    pre: Option<&TextObject>,
    post: Option<&TextObject>,
    ctx: Ctx,
) -> W {
    let current = string(env.value(e).as_ref()).and_then(|s| s.parse::<i64>().ok());
    let top = max.max(min).min(min.saturating_add(MAX_SCALE_POINTS - 1));
    let error = env.error(&a.action_id).is_some();
    let mut points = row![].spacing(SPACE_1);
    for n in min..=top {
        let sel = current == Some(n);
        points = points.push(
            button(text(n.to_string()).size(TEXT_SMALL))
                .padding([4, 8])
                .style(move |th: &Theme, st| {
                    let mut s = if sel {
                        style::button_primary(th, st)
                    } else {
                        style::button_secondary(th, st)
                    };
                    if error && !sel {
                        s.border = s.border.color(tokens(th).danger);
                    }
                    s
                })
                .on_press_maybe(change(env, a, ctx, Some(Value::String(n.to_string())))),
        );
    }
    let mut r = row![].spacing(SPACE_2).align_y(Alignment::Center);
    if let Some(p) = pre {
        r = r.push(
            text(tx::label(env, p, None))
                .size(TEXT_CAPTION)
                .style(style::text_hint),
        );
    }
    r = r.push(points.wrap());
    if let Some(p) = post {
        r = r.push(
            text(tx::label(env, p, None))
                .size(TEXT_CAPTION)
                .style(style::text_hint),
        );
    }
    r.into()
}

/// Checkbox or toggle-switch list: the option label may be mrkdwn.
fn checks(
    env: &Env<'_>,
    e: &El,
    a: &Actionable,
    options: &[UiOption],
    ctx: Ctx,
    switch: bool,
) -> W {
    let selected = strings(env.value(e).as_ref());
    let enabled = env.enabled(a, ctx);
    let emit = env.emits(a, ctx);
    let mut col = column![].spacing(SPACE_1);
    for o in options {
        let checked = selected.contains(&o.value);
        let next: Vec<Value> = if checked {
            selected
                .iter()
                .filter(|v| **v != o.value)
                .cloned()
                .map(Value::String)
                .collect()
        } else {
            selected
                .iter()
                .cloned()
                .chain([o.value.clone()])
                .map(Value::String)
                .collect()
        };
        let msg = enabled.then(|| UiKitMsg::Toggle {
            req: env.req(a, Some(Value::Array(next))),
            option: o.value.clone(),
            emit,
        });
        let control: W = if switch {
            let m = msg.clone();
            toggler(checked)
                .size(18)
                .on_toggle_maybe(m.map(|m| move |_| m.clone()))
                .style(style::toggler)
                .into()
        } else {
            let m = msg.clone();
            checkbox(checked)
                .size(16)
                .on_toggle_maybe(m.map(|m| move |_| m.clone()))
                .style(style::checkbox)
                .into()
        };
        let label = mouse_area(tx::body(env, &o.text, None));
        let label = match msg {
            Some(m) => label.on_press(m),
            None => label,
        };
        col = col.push(
            row![control, label]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
        );
    }
    col.into()
}

fn radios(env: &Env<'_>, e: &El, a: &Actionable, options: &[UiOption], ctx: Ctx) -> W {
    let current = string(env.value(e).as_ref());
    let mut col = column![].spacing(SPACE_1);
    for o in options {
        let sel = current.as_deref() == Some(o.value.as_str());
        let r = row![
            icon::sized(if sel { Icon::CircleDot } else { Icon::Circle }, 16.0),
            tx::body(env, &o.text, None),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        col = col.push(
            button(r)
                .padding([2, 0])
                .style(style::button_bare)
                .on_press_maybe(change(env, a, ctx, Some(Value::String(o.value.clone())))),
        );
    }
    col.into()
}

fn glyph(name: &str) -> Icon {
    match name {
        "phone" => Icon::Phone,
        "phone-off" => Icon::PhoneOff,
        "phone-issue" => Icon::PhoneMissed,
        "clock" => Icon::Clock,
        "arrow-forward" => Icon::ArrowRight,
        "info" => Icon::Info,
        "phone-question-mark" => Icon::CircleQuestionMark,
        _ => Icon::Circle,
    }
}

/// `icon` element: 20 px, coloured by variant; `framed` draws a tinted box.
pub fn icon(i: &UiIcon) -> W {
    let variant = i.variant.clone();
    let color = move |th: &Theme| {
        let t = tokens(th);
        match variant.as_str() {
            "danger" => t.danger,
            "warning" => t.warning_fg,
            "secondary" => t.text_secondary,
            _ => t.text_primary,
        }
    };
    let c2 = color.clone();
    let g = icon::sized(glyph(&i.icon), 20.0).style(move |th: &Theme| text::Style {
        color: Some(c2(th)),
    });
    if !i.framed {
        return g.into();
    }
    let v = i.variant.clone();
    container(g)
        .padding(SPACE_1)
        .style(move |th: &Theme| {
            let t = tokens(th);
            let bg = match v.as_str() {
                "danger" => t.error_bg,
                "warning" => t.warning_bg,
                _ => t.surface_sunken,
            };
            container::Style {
                background: Some(bg.into()),
                border: border::rounded(RADIUS_MD),
                ..container::Style::default()
            }
        })
        .into()
}

fn icon_button_el(
    env: &Env<'_>,
    a: &Actionable,
    i: &UiIcon,
    label: Option<&str>,
    url: Option<&str>,
    value: Option<&str>,
    ctx: Ctx,
) -> W {
    let link = url.and_then(safe_url);
    let on = press(env, a, ctx, link.clone(), url.is_some(), value);
    // Danger-coloured unless the icon says `default`, or it is a link.
    let danger = url.is_none() && i.variant != "default";
    let g = icon::sized(glyph(&i.icon), 16.0).style(move |th: &Theme| text::Style {
        color: Some(if danger {
            tokens(th).danger
        } else {
            tokens(th).text_primary
        }),
    });
    let b: W = button(container(g).center(Length::Fill))
        .width(28)
        .height(28)
        .padding(0)
        .style(style::button_icon)
        .on_press_maybe(on)
        .into();
    let tip = label
        .map(str::to_owned)
        .or(link.as_deref().map(host))
        .unwrap_or_default();
    if tip.is_empty() {
        b
    } else {
        tooltip(b, tip, TooltipPosition::Top)
    }
}
