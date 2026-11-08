//! The contextual bar (docs/uikit.md §5.6): an app view in the room's right
//! panel (`room::panel::Kind::App(view_id)`), with its own header (app icon,
//! title, close) and stretched footer buttons.

use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::uikit::Surface;
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, HEADER_HEIGHT, SPACE_2, SPACE_4, TEXT_SMALL, TEXT_SUBTITLE,
};
use rustket_ui::widget::{divider, ellipsis, icon_button};

use super::modal::{app_icon, footer_button, title_text, view_env};
use super::render;
use super::{SurfaceKey, UiKitMsg};
use crate::room::ViewCtx;
use crate::room::render::RenderCtx;

/// The bar `view_id` (an empty notice when it is gone).
pub fn view<'a>(view_id: &str, ctx: &ViewCtx<'a>) -> Element<'a, UiKitMsg> {
    let surface = SurfaceKey::Bar(view_id.to_owned());
    let close = UiKitMsg::Close {
        surface: surface.clone(),
        cleared: true,
    };
    let Some(b) = ctx.workspace.uikit().bar(view_id) else {
        return column![
            header(None, String::new(), close),
            container(
                text(t!("uikit.view_gone").into_owned())
                    .size(TEXT_SMALL)
                    .style(style::text_hint)
            )
            .padding(SPACE_4),
        ]
        .into();
    };
    let rc = RenderCtx {
        interactive: false,
        ..RenderCtx::from_view(ctx, &super::modal::NO_STATE)
    };
    let v = &b.view.view;
    let env = view_env(
        rc,
        surface.clone(),
        Surface::ContextualBar,
        b,
        v.submit.is_some(),
    );
    let title = title_text(&env, &v.title);
    let icon = app_icon(&rc, &b.app_id, &title, 24.0);
    let blocks = super::view_blocks(&v.blocks, Surface::ContextualBar);
    let body = render::blocks(&env, None, &blocks);
    let mut col = column![
        header(Some(icon), title, close),
        divider(),
        scrollable(container(body).padding(SPACE_4))
            .height(Length::Fill)
            .style(style::scrollable),
    ]
    .height(Length::Fill);
    let mut footer = row![].spacing(SPACE_2);
    footer = footer.push(
        container(footer_button(
            &env,
            v.close.as_ref(),
            "uikit.cancel",
            false,
            Some(UiKitMsg::Close {
                surface: surface.clone(),
                cleared: false,
            }),
            false,
            true,
        ))
        .width(Length::Fill),
    );
    if v.submit.is_some() {
        footer = footer.push(
            container(footer_button(
                &env,
                v.submit.as_ref(),
                "uikit.submit",
                true,
                (!b.submitting).then(|| UiKitMsg::Submit(surface.clone())),
                b.submitting,
                true,
            ))
            .width(Length::Fill),
        );
    }
    if v.close.is_some() || v.submit.is_some() {
        col = col.push(divider()).push(container(footer).padding(SPACE_4));
    }
    col.into()
}

fn header<'a>(
    icon: Option<Element<'static, UiKitMsg>>,
    title: String,
    close: UiKitMsg,
) -> Element<'a, UiKitMsg> {
    let mut head = row![].spacing(SPACE_2).align_y(Alignment::Center);
    if let Some(i) = icon {
        head = head.push(i);
    }
    head = head
        .push(
            ellipsis(title)
                .size(TEXT_SUBTITLE)
                .font(FONT_SEMIBOLD)
                .width(Length::Fill),
        )
        .push(icon_button(Icon::X, Some(close), t!("uikit.close")));
    container(head)
        .height(HEADER_HEIGHT)
        .padding([0.0, SPACE_4])
        .center_y(HEADER_HEIGHT)
        .into()
}
