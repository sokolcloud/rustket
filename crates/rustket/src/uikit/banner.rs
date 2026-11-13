//! The banner surface (docs/uikit.md §5.7): one banner at a time above the
//! room area; block actions emit and close it, the X sends `viewClosed`.
//! Unlike the web, a banner whose title is a text object is shown too.

use iced::widget::{column, container, row, text};
use iced::{Alignment, Element, Length, Theme, border};
use rocketchat::uikit::{StrOrText, Surface};
use rust_i18n::t;
use rustket_ui::icon::{self, Icon};
use rustket_ui::theme::{SPACE_2, SPACE_3, TEXT_BODY, font_semibold, radius_md, tokens};
use rustket_ui::widget::icon_button_with;

use super::modal::AppCtx;
use super::render::{self, Env};
use super::{SurfaceKey, UiKitMsg, text as tx};

/// The current banner, if any.
pub fn view<'a>(app: AppCtx<'a>) -> Option<Element<'a, UiKitMsg>> {
    let b = app.workspace.uikit().current_banner()?;
    let v = &b.view.view;
    let surface = SurfaceKey::Banner(b.view_id().to_owned());
    let env = Env {
        surface: surface.clone(),
        kind: Surface::Banner,
        rc: app.rc(),
        app_id: Some(b.app_id.clone()),
        state: None,
        errors: None,
        msg: None,
        ephemeral: false,
        disabled: false,
        submits: false,
        lang: super::lang(),
    };
    let variant = v.variant.clone().unwrap_or_else(|| "neutral".into());
    let ic = match variant.as_str() {
        "danger" => Icon::CircleAlert,
        "warning" => Icon::TriangleAlert,
        "success" => Icon::CircleCheck,
        _ => Icon::Info,
    };
    let mut body = column![].spacing(SPACE_2).width(Length::Fill);
    match &v.title {
        Some(StrOrText::Str(s)) if !s.trim().is_empty() => {
            body = body.push(text(s.clone()).size(TEXT_BODY).font(font_semibold()));
        }
        Some(StrOrText::Text(t)) => {
            body = body.push(
                text(tx::label(&env, t, None))
                    .size(TEXT_BODY)
                    .font(font_semibold()),
            );
        }
        _ => {}
    }
    let blocks = super::view_blocks(&v.blocks, Surface::Banner);
    body = body.push(render::blocks(&env, None, &blocks));
    let close = icon_button_with(
        Icon::X,
        Some(UiKitMsg::Close {
            surface,
            cleared: true,
        }),
        t!("uikit.dismiss").into_owned(),
        false,
        28.0,
    );
    let v2 = variant.clone();
    let content = row![
        icon::sized(ic, 18.0).style(move |th: &Theme| text::Style {
            color: Some(fg(th, &v2)),
        }),
        body,
        close
    ]
    .spacing(SPACE_3)
    .align_y(Alignment::Start);
    Some(
        container(
            container(content)
                .padding([SPACE_2, SPACE_3])
                .width(Length::Fill)
                .style(move |th: &Theme| {
                    let t = tokens(th);
                    let bg = match variant.as_str() {
                        "danger" => t.error_bg,
                        "warning" => t.warning_bg,
                        "success" => t.success_bg,
                        "info" => t.info_bg,
                        _ => t.surface_raised,
                    };
                    container::Style {
                        background: Some(bg.into()),
                        border: border::rounded(radius_md()).width(1).color(t.border),
                        ..container::Style::default()
                    }
                }),
        )
        .padding([SPACE_2, SPACE_3])
        .into(),
    )
}

fn fg(th: &Theme, variant: &str) -> iced::Color {
    let t = tokens(th);
    match variant {
        "danger" => t.error_fg,
        "warning" => t.warning_fg,
        "success" => t.success_fg,
        "info" => t.info_fg,
        _ => t.text_secondary,
    }
}
