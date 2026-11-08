//! Text objects (docs/uikit.md §2.1): `plain_text` is literal (the `emoji`
//! flag is ignored, like the web), `mrkdwn` is parsed client side and drawn
//! by the shared message renderer; `i18n` keys resolve in the app's bundle.

use iced::Element;
use iced::widget::text;
use rocketchat::uikit::TextObject;
use rustket_ui::style;
use rustket_ui::theme::{TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::sanitize_bidi;

use super::render::Env;
use super::{UiKitMsg, mrkdwn};
use crate::room::render;

/// The string of `t` after i18n (`app_id`: the namespace when `t` names none).
pub fn string(env: &Env<'_>, t: &TextObject, app_id: Option<&str>) -> String {
    env.rc
        .workspace
        .uikit()
        .translations()
        .text(t, app_id.or(env.app_id.as_deref()), &env.lang)
}

/// A one-line label (buttons, options, placeholders): markup is not parsed.
pub fn label(env: &Env<'_>, t: &TextObject, app_id: Option<&str>) -> String {
    sanitize_bidi(&string(env, t, app_id)).into_owned()
}

/// Renders `t` as body text.
pub fn body(env: &Env<'_>, t: &TextObject, app_id: Option<&str>) -> Element<'static, UiKitMsg> {
    let s = string(env, t, app_id);
    if t.is_mrkdwn() {
        render::md_tree(&mrkdwn::parse_cached(&s), &env.surface.prefix(), &env.rc)
            .map(|r| UiKitMsg::Render(Box::new(r)))
    } else {
        text(sanitize_bidi(&s).into_owned()).size(TEXT_BODY).into()
    }
}

/// Renders `t` small in the hint colour (context blocks, captions).
pub fn small(env: &Env<'_>, t: &TextObject, app_id: Option<&str>) -> Element<'static, UiKitMsg> {
    let s = string(env, t, app_id);
    if t.is_mrkdwn() {
        render::md_tree_sized(
            &mrkdwn::parse_cached(&s),
            &env.surface.prefix(),
            &env.rc,
            TEXT_CAPTION,
        )
        .map(|r| UiKitMsg::Render(Box::new(r)))
    } else {
        text(sanitize_bidi(&s).into_owned())
            .size(TEXT_CAPTION)
            .style(style::text_hint)
            .into()
    }
}
