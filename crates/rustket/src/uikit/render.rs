//! The block renderer shared by every surface (docs/uikit.md §2.3–§2.5,
//! §5.2): one recursive pass with the block context of each element (like
//! `UK:rendering/BlockContext.ts`), deciding what an element emits.
//! Everything is owned (`'static`) except multiline editors of the modal,
//! so message rows can live inside `lazy`.

use std::collections::HashMap;

use iced::widget::{Column, button, column, container, image, row, space, text};
use iced::{Alignment, Element, Length, Theme, border};
use rocketchat::models::Message as RcMessage;
use rocketchat::uikit::{
    Actionable, CalloutVariant, Element as El, InfoCardRow, LayoutBlock, PreviewBlock, Slot,
    Surface, TextObject,
};
use rust_i18n::t;
use rustket_core::uikit::{Permit, Scratch, ViewState, element_permit};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, RADIUS_MD, RADIUS_SM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY,
    TEXT_CAPTION, TEXT_SMALL, tokens,
};
use rustket_ui::widget::{divider, ellipsis};
use serde_json::Value;

use super::elements::{self, Editors};
use super::{ActionRequest, SurfaceKey, UiKitMsg, text as tx};
use crate::room::render::{RenderCtx, RenderMsg, safe_url};

/// Elements shown in an actions block before "Show more..." (§2.3).
pub const ACTIONS_SHOWN: usize = 5;
/// Width of message blocks (the web's fixed `MessageBlock`).
pub const MESSAGE_WIDTH: f32 = 560.0;
/// Longest side of block images.
const IMAGE_MAX: f32 = 360.0;

/// Where an element sits (`BlockContext`): decides what a change emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    /// Section or callout accessory: emits.
    Section,
    /// Actions block, info-card row action: emits.
    Action,
    /// Input block: stores (emits only with `dispatchActionConfig`).
    Form,
    /// Context block: stores (emits only with `on_item_selected`).
    Context,
}

/// What the renderer needs to know about the surface.
pub struct Env<'a> {
    pub surface: SurfaceKey,
    pub kind: Surface,
    pub rc: RenderCtx<'a>,
    /// App of the view (i18n namespace and emit fallback); `None` in messages.
    pub app_id: Option<String>,
    /// Values of a modal / bar.
    pub state: Option<&'a ViewState>,
    pub errors: Option<&'a HashMap<String, String>>,
    /// The message of a message surface (trust rule, §3.2).
    pub msg: Option<&'a RcMessage>,
    pub ephemeral: bool,
    /// The whole surface is inert (a submit is in flight).
    pub disabled: bool,
    /// The view has a submit button (Enter in single-line fields submits).
    pub submits: bool,
    pub lang: String,
}

impl<'a> Env<'a> {
    pub fn message(msg: &'a RcMessage, surface: SurfaceKey, rc: &RenderCtx<'a>) -> Self {
        Self {
            ephemeral: rc.workspace.is_ephemeral(&msg.rid, &msg.id),
            surface,
            kind: Surface::Message,
            rc: *rc,
            app_id: None,
            state: None,
            errors: None,
            msg: Some(msg),
            disabled: false,
            submits: false,
            lang: super::lang(),
        }
    }

    pub fn scratch(&self) -> &'a Scratch {
        self.rc.workspace.uikit().scratch()
    }

    pub fn key(&self, action_id: &str) -> String {
        self.surface.element(action_id)
    }

    /// The request of element `a` (already inherited) carrying `value`.
    pub fn req(&self, a: &Actionable, value: Option<Value>) -> ActionRequest {
        ActionRequest {
            surface: self.surface.clone(),
            app_id: a
                .app_id
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| self.app_id.clone()),
            block_id: a.block_id.clone().unwrap_or_default(),
            action_id: a.action_id.clone(),
            value,
            url: None,
            in_video_conf: false,
        }
    }

    /// Whether a change of `a` in `ctx` sends a block action (§5.2).
    pub fn emits(&self, a: &Actionable, ctx: Ctx) -> bool {
        match ctx {
            Ctx::Section | Ctx::Action => true,
            Ctx::Form | Ctx::Context => a.dispatches_on_select(),
        }
    }

    /// The §3.2 trust rule for an emitting element (views always emit).
    pub fn permit(&self, a: &Actionable, has_url: bool) -> Permit {
        match self.msg {
            Some(m) => element_permit(
                m,
                self.ephemeral,
                false,
                a.app_id.as_deref(),
                &a.action_id,
                has_url,
            ),
            None => Permit::Emit,
        }
    }

    pub fn loading(&self, a: &Actionable) -> bool {
        self.scratch().is_loading(&self.key(&a.action_id))
    }

    /// Whether element `a` takes input (not loading, surface not busy, and —
    /// when a change emits — allowed by the trust rule).
    pub fn enabled(&self, a: &Actionable, ctx: Ctx) -> bool {
        !self.disabled
            && !self.loading(a)
            && (!self.emits(a, ctx) || self.permit(a, false) == Permit::Emit)
    }

    /// Current value of element `e`: view state, else the local widget value
    /// (messages, banners), else its initial value.
    pub fn value(&self, e: &El) -> Option<Value> {
        let a = e.actionable()?;
        if let Some(v) = self.state.and_then(|s| s.get(&a.action_id)) {
            return Some(v.clone());
        }
        if let Some(v) = self.scratch().values.get(&self.key(&a.action_id)) {
            return Some(v.clone());
        }
        e.initial_value()
    }

    pub fn error(&self, action_id: &str) -> Option<&'a str> {
        self.errors
            .and_then(|e| e.get(action_id))
            .map(String::as_str)
    }

    pub fn text(&self, t: &TextObject) -> String {
        tx::string(self, t, None)
    }
}

/// Renders `blocks` (already filtered by [`Surface::renderable`]).
pub fn blocks<'e>(
    env: &Env<'_>,
    editors: Option<&'e Editors>,
    blocks: &[LayoutBlock],
) -> Element<'e, UiKitMsg> {
    let gap = if env.kind == Surface::Banner {
        SPACE_2
    } else {
        SPACE_4
    };
    let mut col: Column<'e, UiKitMsg> = column![].spacing(gap).width(Length::Fill);
    for b in blocks {
        if let Some(e) = block(env, editors, b) {
            col = col.push(e);
        }
    }
    if env.kind == Surface::Message {
        return container(col).max_width(MESSAGE_WIDTH).into();
    }
    col.into()
}

fn block<'e>(
    env: &Env<'_>,
    editors: Option<&'e Editors>,
    b: &LayoutBlock,
) -> Option<Element<'e, UiKitMsg>> {
    let app = b.meta().and_then(|m| m.app_id.clone());
    let app = app.as_deref();
    Some(match b {
        LayoutBlock::Section {
            meta,
            text,
            fields,
            accessory,
        } => {
            let mut left = column![].spacing(SPACE_2).width(Length::Fill);
            if let Some(t) = text {
                left = left.push(tx::body(env, t, app));
            }
            for pair in fields.chunks(2) {
                let mut r = row![].spacing(SPACE_4).width(Length::Fill);
                for f in pair {
                    r = r.push(container(tx::body(env, f, app)).width(Length::Fill));
                }
                if pair.len() == 1 {
                    r = r.push(space().width(Length::Fill));
                }
                left = left.push(r);
            }
            let mut r = row![left].spacing(SPACE_3).align_y(Alignment::Start);
            if let Some(acc) = accessory
                .as_deref()
                .filter(|e| e.allowed_in(Slot::SectionAccessory))
            {
                let e = acc.inherited(meta);
                if let Some(w) = element(env, editors, &e, Ctx::Section, true) {
                    r = r.push(w);
                }
            }
            r.into()
        }
        LayoutBlock::Divider { .. } => divider(),
        LayoutBlock::Image {
            image_url,
            alt_text,
            title,
            ..
        } => {
            let mut col = column![].spacing(SPACE_1);
            if let Some(t) = title {
                col = col.push(
                    ellipsis(tx::label(env, t, app))
                        .size(TEXT_CAPTION)
                        .style(|th: &Theme| Some(tokens(th).text_hint)),
                );
            }
            col = col.push(block_image(env, image_url, alt_text, IMAGE_MAX));
            let centred = matches!(env.kind, Surface::Modal | Surface::ContextualBar);
            if centred {
                container(col).center_x(Length::Fill).into()
            } else {
                col.into()
            }
        }
        LayoutBlock::Actions { meta, elements } => {
            let list: Vec<El> = elements
                .iter()
                .filter(|e| e.allowed_in(Slot::Actions))
                .map(|e| e.inherited(meta))
                .collect();
            let key = format!(
                "{}#actions{}",
                env.surface.prefix(),
                meta.block_id.clone().unwrap_or_default()
            );
            let expanded = env.kind == Surface::Banner || env.scratch().expanded.contains(&key);
            let shown = if expanded {
                list.len()
            } else {
                list.len().min(ACTIONS_SHOWN)
            };
            let mut items: Vec<Element<'e, UiKitMsg>> = list[..shown]
                .iter()
                .filter_map(|e| element(env, editors, e, Ctx::Action, false))
                .collect();
            if shown < list.len() {
                items.push(
                    button(text(t!("uikit.show_more").into_owned()).size(TEXT_SMALL))
                        .style(style::button_link)
                        .on_press(UiKitMsg::ShowMore(key))
                        .into(),
                );
            }
            row(items)
                .spacing(SPACE_2)
                .align_y(Alignment::Center)
                .wrap()
                .vertical_spacing(SPACE_2)
                .into()
        }
        LayoutBlock::Context { meta, elements } => context_row(env, meta, elements, app),
        LayoutBlock::Input {
            meta,
            label,
            element: el,
            hint,
            ..
        } => {
            let e = el.inherited(meta);
            if !e.allowed_in(Slot::Input) {
                return None;
            }
            let mut col = column![
                text(tx::label(env, label, app))
                    .size(TEXT_SMALL)
                    .font(FONT_SEMIBOLD)
            ]
            .spacing(SPACE_1);
            if let Some(w) = element(env, editors, &e, Ctx::Form, true) {
                col = col.push(w);
            }
            if let Some(err) = e.action_id().and_then(|a| env.error(a)) {
                col = col.push(
                    text(err.to_owned())
                        .size(TEXT_CAPTION)
                        .style(style::text_danger),
                );
            }
            if let Some(h) = hint {
                col = col.push(tx::small(env, h, app));
            }
            col.into()
        }
        LayoutBlock::Preview(p) => preview(env, p, app),
        // Video-conference blocks are rendered by the video-conference module
        // (teams agent); this renderer leaves them out on purpose.
        LayoutBlock::VideoConf { .. } => return None,
        LayoutBlock::Callout {
            meta,
            title,
            text: body,
            variant,
            accessory,
        } => callout(
            env,
            editors,
            meta,
            title.as_ref(),
            body,
            *variant,
            accessory.as_deref(),
            app,
        ),
        LayoutBlock::TabNavigation { meta, tabs } => tab_strip(env, meta, tabs, app),
        LayoutBlock::InfoCard { meta, rows } => info_card(env, editors, meta, rows, app),
        LayoutBlock::Conditional { .. } | LayoutBlock::Unknown(_) => return None,
    })
}

/// An image of a block or element (`max` = longest side). Relative URLs
/// resolve against the server; SVG and unloaded images show the alt text.
pub fn block_image(env: &Env<'_>, url: &str, alt: &str, max: f32) -> Element<'static, UiKitMsg> {
    let Some(abs) = image_url(env, url) else {
        return alt_box(alt, max.min(160.0), 40.0);
    };
    match env.rc.images.get(env.rc.server_id, &abs) {
        Some(l) => {
            let (w, h) = (l.width as f32, l.height as f32);
            let scale = (max / w.max(h).max(1.0)).min(1.0);
            image(l.handle.clone())
                .width(w * scale)
                .height(h * scale)
                .border_radius(RADIUS_SM)
                .into()
        }
        None => alt_box(alt, max.min(360.0), (max * 154.0 / 360.0).min(154.0)),
    }
}

/// A square cropped image (`size` px; section accessory 88, context 20).
pub fn square_image(env: &Env<'_>, url: &str, alt: &str, size: f32) -> Element<'static, UiKitMsg> {
    let loaded = image_url(env, url).and_then(|u| env.rc.images.get(env.rc.server_id, &u));
    match loaded {
        Some(l) => image(l.handle.clone())
            .width(size)
            .height(size)
            .content_fit(iced::ContentFit::Cover)
            .border_radius(if size > 30.0 { RADIUS_SM } else { size / 2.0 })
            .into(),
        None => alt_box(if size > 30.0 { alt } else { "" }, size, size),
    }
}

fn image_url(env: &Env<'_>, url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    if url.starts_with('/') && !url.starts_with("//") {
        return Some(env.rc.server.absolute(url));
    }
    safe_url(url).filter(|u| u.starts_with("http"))
}

fn alt_box(alt: &str, w: f32, h: f32) -> Element<'static, UiKitMsg> {
    container(
        text(alt.to_owned())
            .size(TEXT_CAPTION)
            .style(style::text_hint),
    )
    .width(w)
    .height(h)
    .center(Length::Fixed(w))
    .center_y(Length::Fixed(h))
    .style(|th: &Theme| {
        let t = tokens(th);
        container::Style {
            background: Some(t.surface_sunken.into()),
            border: border::rounded(RADIUS_SM),
            ..container::Style::default()
        }
    })
    .into()
}

/// Every image URL a block list may show (for row cache keys).
pub fn image_urls(blocks: &[LayoutBlock]) -> Vec<String> {
    fn el_url(e: &El) -> Option<String> {
        match e {
            El::Image { image_url, .. } => Some(image_url.clone()),
            _ => None,
        }
    }
    let mut out = Vec::new();
    for b in rocketchat::uikit::resolve_conditionals(blocks, rocketchat::uikit::ENGINE) {
        match &b {
            LayoutBlock::Image { image_url, .. } => out.push(image_url.clone()),
            LayoutBlock::Section { accessory, .. } => {
                out.extend(accessory.as_deref().and_then(el_url));
            }
            LayoutBlock::Context { elements, .. } => out.extend(elements.iter().filter_map(el_url)),
            LayoutBlock::Preview(p) => {
                out.extend(p.thumb.iter().map(|t| t.url.clone()));
                out.extend(p.preview.iter().map(|t| t.url.clone()));
                out.extend(p.footer.iter().flatten().filter_map(el_url));
            }
            _ => {}
        }
    }
    out
}

/// One element in context `ctx` (`None` for elements the slot does not
/// render).
fn element<'e>(
    env: &Env<'_>,
    editors: Option<&'e Editors>,
    e: &El,
    ctx: Ctx,
    grow: bool,
) -> Option<Element<'e, UiKitMsg>> {
    elements::element(env, editors, e, ctx, grow)
}

fn context_row(
    env: &Env<'_>,
    meta: &rocketchat::uikit::BlockMeta,
    elements: &[El],
    app: Option<&str>,
) -> Element<'static, UiKitMsg> {
    let items: Vec<El> = elements
        .iter()
        .filter(|e| e.allowed_in(Slot::Context))
        .map(|e| e.inherited(meta))
        .collect();
    // Text items flow as one paragraph (images stay in front) unless a plain
    // item contains characters the markdown subset would interpret.
    let texts: Vec<&TextObject> = items
        .iter()
        .filter_map(|e| match e {
            El::Text(t) => Some(t),
            _ => None,
        })
        .collect();
    let joinable = texts.len() > 1
        && texts.iter().all(|t| {
            t.is_mrkdwn() || !tx::string(env, t, app).contains(['*', '_', '~', '`', '[', '<'])
        });
    let mut r = row![].spacing(SPACE_2).align_y(Alignment::Center);
    for e in &items {
        if let El::Image {
            image_url,
            alt_text,
        } = e
        {
            r = r.push(square_image(env, image_url, alt_text, 20.0));
        }
    }
    if joinable {
        let joined = texts
            .iter()
            .map(|t| tx::string(env, t, app))
            .collect::<Vec<_>>()
            .join("   ");
        return r
            .push(tx::small(env, &TextObject::mrkdwn(joined), None))
            .into();
    }
    for t in texts {
        r = r.push(tx::small(env, t, app));
    }
    r.into()
}

fn card_style(secondary: bool) -> impl Fn(&Theme) -> container::Style {
    move |th: &Theme| {
        let t = tokens(th);
        container::Style {
            background: Some(
                if secondary {
                    t.surface_sunken
                } else {
                    t.surface_raised
                }
                .into(),
            ),
            border: border::rounded(RADIUS_MD).width(1).color(t.border),
            ..container::Style::default()
        }
    }
}

fn preview(env: &Env<'_>, p: &PreviewBlock, app: Option<&str>) -> Element<'static, UiKitMsg> {
    let link = p.external_url.as_deref().and_then(safe_url);
    let mut info = column![].spacing(SPACE_1).width(Length::Fill);
    for t in &p.title {
        let label = tx::label(env, t, app);
        info = info.push(match &link {
            Some(u) => Element::from(
                button(
                    text(label)
                        .size(TEXT_BODY)
                        .font(FONT_SEMIBOLD)
                        .style(style::text_link),
                )
                .padding(0)
                .style(style::button_bare)
                .on_press(UiKitMsg::Render(Box::new(RenderMsg::OpenUrl(u.clone())))),
            ),
            None => text(label).size(TEXT_BODY).font(FONT_SEMIBOLD).into(),
        });
    }
    for d in &p.description {
        info = info.push(tx::small(env, d, app));
    }
    if let Some(f) = &p.footer {
        info = info.push(context_row(env, &p.meta, f, app));
    }
    let mut col = column![].spacing(SPACE_2);
    if p.shows_cover()
        && let Some(pv) = &p.preview
    {
        col = col.push(block_image(env, &pv.url, "", IMAGE_MAX));
    }
    let body: Element<'static, UiKitMsg> = match &p.thumb {
        Some(th) => row![square_image(env, &th.url, "", 72.0), info]
            .spacing(SPACE_3)
            .align_y(Alignment::Center)
            .into(),
        None => info.into(),
    };
    col = col.push(body);
    container(col)
        .padding(SPACE_3)
        .max_width(368)
        .style(card_style(false))
        .into()
}

#[allow(clippy::too_many_arguments)]
fn callout<'e>(
    env: &Env<'_>,
    editors: Option<&'e Editors>,
    meta: &rocketchat::uikit::BlockMeta,
    title: Option<&TextObject>,
    body: &TextObject,
    variant: CalloutVariant,
    accessory: Option<&El>,
    app: Option<&str>,
) -> Element<'e, UiKitMsg> {
    let ic = match variant {
        CalloutVariant::Info => Icon::Info,
        CalloutVariant::Danger => Icon::CircleAlert,
        CalloutVariant::Warning => Icon::TriangleAlert,
        CalloutVariant::Success => Icon::CircleCheck,
    };
    let mut col = column![].spacing(SPACE_1).width(Length::Fill);
    if let Some(t) = title {
        // The raw title text: no i18n, no markdown (§2.1).
        col = col.push(text(t.text.clone()).size(TEXT_BODY).font(FONT_SEMIBOLD));
    }
    col = col.push(tx::body(env, body, app));
    let mut r = row![
        icon::sized(ic, 18.0).style(move |th: &Theme| {
            let t = tokens(th);
            text::Style {
                color: Some(match variant {
                    CalloutVariant::Info => t.info_fg,
                    CalloutVariant::Danger => t.error_fg,
                    CalloutVariant::Warning => t.warning_fg,
                    CalloutVariant::Success => t.success_fg,
                }),
            }
        }),
        col
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Start);
    if let Some(acc) = accessory.filter(|e| e.allowed_in(Slot::CalloutAccessory)) {
        let e = acc.inherited(meta);
        if let Some(w) = element(env, editors, &e, Ctx::Section, false) {
            r = r.push(w);
        }
    }
    container(r)
        .padding(SPACE_3)
        .width(Length::Fill)
        .style(move |th: &Theme| {
            let t = tokens(th);
            let (bg, fg) = match variant {
                CalloutVariant::Info => (t.info_bg, t.info_fg),
                CalloutVariant::Danger => (t.error_bg, t.error_fg),
                CalloutVariant::Warning => (t.warning_bg, t.warning_fg),
                CalloutVariant::Success => (t.success_bg, t.success_fg),
            };
            container::Style {
                background: Some(bg.into()),
                border: border::rounded(RADIUS_MD)
                    .width(1)
                    .color(fg.scale_alpha(0.35)),
                ..container::Style::default()
            }
        })
        .into()
}

fn tab_strip(
    env: &Env<'_>,
    meta: &rocketchat::uikit::BlockMeta,
    tabs: &[El],
    app: Option<&str>,
) -> Element<'static, UiKitMsg> {
    let key = format!(
        "{}#tabs{}",
        env.surface.prefix(),
        meta.block_id.clone().unwrap_or_default()
    );
    let chosen = env.scratch().tabs.get(&key).cloned();
    let mut r = row![].spacing(SPACE_1);
    for tab in tabs.iter().filter(|e| e.allowed_in(Slot::TabNavigation)) {
        let El::Tab {
            a,
            title,
            disabled,
            selected,
        } = tab.inherited(meta)
        else {
            continue;
        };
        let is_selected = match &chosen {
            Some(c) => *c == a.action_id,
            None => selected,
        };
        let on = (!disabled && !env.disabled).then(|| UiKitMsg::Change {
            req: env.req(&a, None),
            emit: true,
            confirm: None,
        });
        let label = text(tx::label(env, &title, app))
            .size(TEXT_SMALL)
            .font(FONT_SEMIBOLD);
        let tab_btn = button(container(label).padding([SPACE_1, SPACE_2]))
            .padding(0)
            .style(style::button_list_item(is_selected))
            .on_press_maybe(on);
        r = r.push(tab_btn);
    }
    column![r, divider()].spacing(0).into()
}

fn info_card<'e>(
    env: &Env<'_>,
    editors: Option<&'e Editors>,
    meta: &rocketchat::uikit::BlockMeta,
    rows: &[InfoCardRow],
    app: Option<&str>,
) -> Element<'e, UiKitMsg> {
    let mut col = column![].spacing(0);
    for r in rows {
        let mut line = row![].spacing(SPACE_2).align_y(Alignment::Center);
        for e in r
            .elements
            .iter()
            .filter(|e| e.allowed_in(Slot::InfoCardElement))
        {
            match e {
                El::Icon(i) => line = line.push(elements::icon(i)),
                El::Text(t) => line = line.push(tx::body(env, t, app)),
                _ => {}
            }
        }
        line = line.push(space().width(Length::Fill));
        if let Some(act) = r
            .action
            .as_deref()
            .filter(|e| e.allowed_in(Slot::InfoCardAction))
        {
            let e = act.inherited(meta);
            if let Some(w) = element(env, editors, &e, Ctx::Action, false) {
                line = line.push(w);
            }
        }
        let secondary = r.background != "default";
        col = col.push(container(line).padding(SPACE_4).width(Length::Fill).style(
            move |th: &Theme| {
                let t = tokens(th);
                container::Style {
                    background: Some(
                        if secondary {
                            t.surface_sunken
                        } else {
                            t.surface_raised
                        }
                        .into(),
                    ),
                    ..container::Style::default()
                }
            },
        ));
    }
    container(col)
        .max_width(345)
        .clip(true)
        .style(card_style(false))
        .into()
}
