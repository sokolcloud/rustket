# Room components contract

The room area of the active server is one `RoomView` (`room/mod.rs`) per open
room. It owns four components, each in its own module, with its own `Message`
and its own locale file:

| Module | Struct | Locale file | Key prefix |
|---|---|---|---|
| `room/header.rs` | `Header` | `locales/room_header.yml` | `room_header.` |
| `room/timeline.rs` | `Timeline` | `locales/room_timeline.yml` | `room_timeline.` |
| `room/composer.rs` | `Composer` | `locales/room_composer.yml` | `room_composer.` |
| `room/panel.rs` | `Panel` (right contextual panel) | `locales/room_panel.yml` | `room_panel.` |

Implementing a component means editing **only** its module and its locale file.
Do not change the signatures below; add variants to your own `Message`, fields
to your own struct, and private helpers/submodules (`room/timeline/…` is fine).

## Interface every component implements

```rust
pub struct X { /* component state, per open room */ }

#[derive(Debug, Clone)]
pub enum Message { /* … */ }

impl X {
    pub fn new(rid: &str) -> Self;
    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message>;
    /// Called after realtime events / syncs changed the workspace.
    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message>;
    pub fn subscription(&self) -> Subscription<Message>;
    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message>;
}
```

Exception: `Panel::view` returns `Option<Element>` (`None` = closed), and
`Panel` additionally has `open(Kind)`, `close()` and `kind()`, used by
`RoomView` for `Effect::OpenPanel` / `Effect::ClosePanel`.

Subscriptions are mapped with non-capturing functions only (iced requirement);
`RoomView`/`App` add the server id and room id with `Subscription::with`.

## Context

`ViewCtx<'a>` (read-only, for `view`):
- `server_id`, `server: &ServerUrl`, `rid`
- `workspace: &Workspace` — rooms, subscriptions, timelines (`timeline(rid)`,
  `thread(tmid)`), presence, typing (`typing_label`), settings, permissions
  (`rustket_core::permissions::*`), emoji, drafts.
- `images: &Images` — `images.avatar(server_id, server, &Avatar, name)` returns an
  `AvatarSource` (initials until loaded); `images.get(server_id, url)` for any
  other image (attachments). Request what you will show from `update` /
  `on_changes` with `ctx.images.want(ImageRequest { .. })` so it is fetched
  right away; misses hit in `view` are fetched right after the frame. Downloads
  are size-limited; SVG is never decoded (SVG avatars show initials); raster
  images are decoded with limits and downscaled (avatars to 128 px, others to
  1600 px). Never block in `view`.
- `prefs: &UiPrefs` (density, 24h clock, real names override …)
- `connected` (realtime up), `panel: Option<&panel::Kind>`, `now: Instant`

Room messages are tagged with the session epoch by the app: results of a
previous session (after logout / re-login) never reach a component.

`UpdateCtx<'a>` (for `update` / `on_changes`):
- same identifiers, `workspace: &mut Workspace` (e.g. `add_pending`,
  `set_draft`, `open_thread`, `begin_load_thread`/`finish_load_thread`),
  `session: Option<&SessionHandle>`, `images`, `prefs`.
- `ctx.rest()` → `Option<RestClient>` (authenticated).
- `ctx.rest_task(|rest| async move { rest.send_message(&m).await }, Message::Sent)`
  runs a REST call; the result is `Res<T> = Result<T, Failure>` (cloneable,
  `Failure::user_text()` for a localised error, `failure.error()` for matching
  `rocketchat::Error` variants such as `TwoFactorRequired`).
- `ctx.session_task(|s| async move { s.session().set_user_activity(&rid, None, &["user-typing"]).await }, Message::Typed)`
  runs a DDP call (`Session::call`, `set_user_activity`, `watch_room`, …).
- Without a session both helpers resolve to an `Unauthorized` failure.

## Effects (component → parents)

Return them in `Action { task, effects }` (`Action::none()`,
`Action::task(t)`, `Action::effect(e)`, `.with(e)`):

| Effect | Handled by | Meaning |
|---|---|---|
| `Toast(BannerKind, title, body)` | App | transient notification (strings already translated) |
| `OpenRoom(rid)` | App | switch to another room of the same server |
| `LoadOlder` | RoomView | load the next older history page (`HistoryCursor`); the timeline sees the result through `on_changes` (`changes.timelines`) |
| `MarkRead` | RoomView | `subscriptions.read` (deduplicated) |
| `OpenPanel(Kind)` / `ClosePanel` | RoomView | right panel; opening the shown kind toggles it closed |
| `OpenUrl(url)` | App | external browser; only absolute `http`, `https`, `mailto` URLs (others are refused) |

What `RoomView` already does: `workspace.open_room` on open, keeps the
`watch_room` guard (dropped with the view when the room is closed or removed),
loads the first history page (and the next one automatically when a page held
only hidden thread replies), re-subscribes the watch and reloads a missing or
failed first page after a reconnect, reloads after `MessagesImported`, marks
the room read on open and when new messages arrive while the window is focused. The app applies every realtime event to the
`Workspace` before `on_changes` runs, so components never talk to the session
stream directly.

## i18n

`rust-i18n` with one file per component in `crates/rustket/locales/`, format
`_version: 2`, flat dotted keys with `en` and `ru` for every key:

```yaml
_version: 2
room_composer.placeholder:
  en: "Message"
  ru: "Сообщение"
```

Use `rust_i18n::t!("room_composer.placeholder")` (`%{name}` placeholders:
`t!("key", name = value)`). Every user-visible string goes through it. Keys
computed by `rustket-core` (`room_list` previews, typing labels, section names,
`PostBlock::i18n_key`) live in `preview.yml`, `typing.yml`, `sidebar.yml`;
add missing ones to your own file under your own prefix if you need different
wording. Locale changes are picked up by `build.rs` (`rerun-if-changed=locales`).

## Testing

- Unit-test component logic without iced (state + `update` with a
  `Workspace` built in the test; `UpdateCtx { session: None, .. }`).
- Visual check: `cargo run -p rustket --example snapshot -- <dir>` renders the
  real app headlessly against the dev server (see the example's doc comment);
  extend it with your component's state.
