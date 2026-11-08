//! The UIKit modal (docs/uikit.md §5.5) and the confirmation dialog of
//! elements with `confirm` (§2.2), both drawn over the whole window by the
//! app (`overlay`).

use iced::widget::{column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Length};
use rocketchat::ServerUrl;
use rocketchat::uikit::{Confirm, Element as El, Surface, TextObject};
use rust_i18n::t;
use rustket_core::config::UiPrefs;
use rustket_core::uikit::OpenView;
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_BODY, TEXT_TITLE,
};
use rustket_ui::widget::{
    AvatarShape, AvatarSource, ButtonKind, action_button, avatar, dialog, ellipsis,
    icon_button_with, sanitize_bidi,
};

use super::elements::Editors;
use super::render::{self, Env};
use super::{Local, SurfaceKey, UiKitMsg, text as tx};
use crate::images::Images;
use crate::room::render::{RenderCtx, RenderState};

/// Width of the modal card.
pub const WIDTH: f32 = 520.0;
/// Tallest scrolling body of the modal (header and footer stay visible).
pub const BODY_MAX_HEIGHT: f32 = 520.0;

pub(super) static NO_STATE: RenderState = RenderState::new();

/// Borrowed app data the app-level surfaces need.
#[derive(Clone, Copy)]
pub struct AppCtx<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub workspace: &'a Workspace,
    pub images: &'a Images,
    pub prefs: &'a UiPrefs,
}

impl<'a> AppCtx<'a> {
    pub fn rc(&self) -> RenderCtx<'a> {
        RenderCtx {
            server_id: self.server_id,
            server: self.server,
            workspace: self.workspace,
            images: self.images,
            state: &NO_STATE,
            clock_24h: self.prefs.clock_24h,
            use_real_names: self.workspace.use_real_names(self.prefs.use_real_names),
            interactive: false,
        }
    }
}

/// The view env of an open modal or bar.
pub fn view_env<'a, V>(
    rc: RenderCtx<'a>,
    surface: SurfaceKey,
    kind: Surface,
    v: &'a OpenView<V>,
    submits: bool,
) -> Env<'a> {
    Env {
        surface,
        kind,
        rc,
        app_id: Some(v.app_id.clone()),
        state: Some(&v.state),
        errors: Some(&v.errors),
        msg: None,
        ephemeral: false,
        disabled: v.submitting,
        submits,
        lang: super::lang(),
    }
}

/// The app's icon (`/api/apps/{id}/icon`; SVG and missing icons show
/// initials of the title).
pub fn app_icon(
    rc: &RenderCtx<'_>,
    app_id: &str,
    name: &str,
    size: f32,
) -> Element<'static, UiKitMsg> {
    let url = rc.server.api_root(&format!(
        "apps/{}/icon",
        url::form_urlencoded::byte_serialize(app_id.as_bytes()).collect::<String>()
    ));
    let handle = rc.images.get(rc.server_id, &url).map(|l| l.handle.clone());
    avatar(
        AvatarSource::initials(name, app_id).with_image(handle),
        size,
        AvatarShape::Rounded,
    )
    .into()
}

/// A footer button of a view (`close` / `submit`): label from its text.
pub fn footer_button(
    env: &Env<'_>,
    el: Option<&El>,
    fallback: &str,
    primary: bool,
    on: Option<UiKitMsg>,
    loading: bool,
    stretch: bool,
) -> Element<'static, UiKitMsg> {
    let (label, danger) = match el {
        Some(El::Button { text: t, style, .. }) => {
            (tx::label(env, t, None), style.as_deref() == Some("danger"))
        }
        _ => (t!(fallback).into_owned(), false),
    };
    let kind = match (primary, danger) {
        (_, true) => ButtonKind::Danger,
        (true, false) => ButtonKind::Primary,
        (false, false) => ButtonKind::Secondary,
    };
    let b = action_button(kind, label, on).loading(loading);
    if stretch {
        b.width(Length::Fill).into()
    } else {
        b.into()
    }
}

/// The open modal with its key and the message of the backdrop/Escape.
pub fn view<'a>(
    app: AppCtx<'a>,
    local: &'a Local,
) -> Option<(String, Element<'a, UiKitMsg>, UiKitMsg)> {
    let m = app.workspace.uikit().modal()?;
    let id = m.id().to_owned();
    let surface = SurfaceKey::Modal(id.clone());
    let rc = app.rc();
    let v = &m.view.view;
    let env = view_env(rc, surface.clone(), Surface::Modal, m, v.submit.is_some());
    let title = title_text(&env, &v.title);
    let mut head = row![].spacing(SPACE_3).align_y(Alignment::Center);
    if v.show_icon {
        head = head.push(app_icon(&rc, &m.app_id, &title, 28.0));
    }
    head = head
        .push(
            ellipsis(title)
                .size(TEXT_TITLE)
                .font(FONT_SEMIBOLD)
                .width(Length::Fill),
        )
        .push(icon_button_with(
            Icon::X,
            Some(UiKitMsg::Close {
                surface: surface.clone(),
                cleared: true,
            }),
            t!("uikit.close").into_owned(),
            false,
            28.0,
        ));
    let blocks = super::view_blocks(&v.blocks, Surface::Modal);
    let editors: Option<&'a Editors> = Some(&local.editors);
    let body = render::blocks(&env, editors, &blocks);
    let mut footer = row![space().width(Length::Fill)]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
    footer = footer.push(footer_button(
        &env,
        v.close.as_ref(),
        "uikit.cancel",
        false,
        Some(UiKitMsg::Close {
            surface: surface.clone(),
            cleared: false,
        }),
        false,
        false,
    ));
    if v.submit.is_some() {
        footer = footer.push(footer_button(
            &env,
            v.submit.as_ref(),
            "uikit.submit",
            true,
            (!m.submitting).then(|| UiKitMsg::Submit(surface.clone())),
            m.submitting,
            false,
        ));
    }
    let card = container(
        column![
            head,
            container(
                scrollable(container(body).padding([0.0, SPACE_1]))
                    .height(Length::Shrink)
                    .spacing(SPACE_2)
                    .style(style::scrollable)
            )
            .max_height(BODY_MAX_HEIGHT),
            footer,
        ]
        .spacing(SPACE_4)
        .padding(SPACE_6),
    )
    .width(Length::Fixed(WIDTH))
    .style(style::container_modal);
    Some((
        id,
        card.into(),
        UiKitMsg::Close {
            surface,
            cleared: true,
        },
    ))
}

/// The title string of a view (i18n applied, one line).
pub fn title_text(env: &Env<'_>, t: &TextObject) -> String {
    sanitize_bidi(&tx::string(env, t, None)).into_owned()
}

/// The confirmation dialog of an element with `confirm`.
pub fn confirm_view<'a>(app: AppCtx<'a>, c: &Confirm) -> Element<'a, UiKitMsg> {
    let rc = app.rc();
    let env = Env {
        surface: SurfaceKey::Modal(String::new()),
        kind: Surface::Modal,
        rc,
        app_id: None,
        state: None,
        errors: None,
        msg: None,
        ephemeral: false,
        disabled: false,
        submits: false,
        lang: super::lang(),
    };
    let or = |t: &TextObject, key: &str| {
        let s = tx::label(&env, t, None);
        if s.trim().is_empty() {
            t!(key).into_owned()
        } else {
            s
        }
    };
    let danger = c.style.as_deref() == Some("danger");
    let ok_kind = if danger {
        ButtonKind::Danger
    } else {
        ButtonKind::Primary
    };
    dialog(
        or(&c.title, "uikit.confirm_title"),
        column![tx::body(&env, &c.text, None)].spacing(SPACE_2),
        vec![
            action_button(
                ButtonKind::Secondary,
                or(&c.deny, "uikit.cancel"),
                Some(UiKitMsg::Confirmed(false)),
            )
            .into(),
            action_button(
                ok_kind,
                or(&c.confirm, "uikit.confirm"),
                Some(UiKitMsg::Confirmed(true)),
            )
            .into(),
        ],
        Some(UiKitMsg::Confirmed(false)),
    )
}

/// The "Loading" pill shown while app requests run (§4.6).
pub fn busy_pill(ws: &Workspace) -> Option<Element<'static, UiKitMsg>> {
    if ws.uikit().busy() == 0 {
        return None;
    }
    Some(
        container(
            container(
                row![
                    rustket_ui::widget::spinner(12.0),
                    text(t!("uikit.loading").into_owned()).size(TEXT_BODY - 1.0)
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
            )
            .padding([2, 12])
            .style(style::container_popover),
        )
        .padding([2, 0])
        .center_x(Length::Fill)
        .into(),
    )
}
