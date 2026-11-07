# Apps engine and UIKit

Implementation spec for Rustket phase 10 (message `blocks`, block actions, modals,
contextual bars, app action buttons, app slash commands) and for the apps part of phase 12
(installed apps, enable/disable, settings, logs). Target server: Rocket.Chat 8.9.1.

Source prefixes used below:

- `RC:` = `.ref/rocketchat/` (8.9.1). `UK:` = `RC:packages/ui-kit/src/` (the JSON model),
  `FUK:` = `RC:packages/fuselage-ui-kit/src/` (the web renderer), `WEB:` =
  `RC:apps/meteor/client/` (web client), `AE:` = `RC:packages/apps-engine/src/definition/`
  (what apps can send), `SRV:` = `RC:apps/meteor/` server code.
- `RN:` = `.ref/rn/` (official React Native client).

## 0. What was verified live (dev server, 2026-11-08)

Verified with curl and a Python DDP probe (`websockets`) against `http://localhost:3000`, using a
throwaway channel `uikit-probe-<random>` (deleted afterwards):

| Claim | Result |
|---|---|
| `GET /api/apps/installed` (admin and alice) | `{"apps":[],"success":true}`; any logged-in user may call it |
| `GET /api/apps/actionButtons` (with and without auth) | bare JSON array `[]`, no envelope |
| `GET /api/apps/count` (with and without auth) | `{"totalMarketplaceEnabled":0,"totalPrivateEnabled":0,"maxMarketplaceApps":5,"maxPrivateApps":0,"success":true}`; public (`authRequired: false`, `SRV:ee/server/apps/communication/endpoints/appsCountHandler.ts`) |
| `GET /api/apps/languages`, `/api/apps/externalComponents` | `{"apps":[],"success":true}`, `{"externalComponents":[],"success":true}` |
| `GET /api/apps/logs?count=2` admin / alice | `{"offset":0,"logs":[],"count":0,"total":0,"success":true}` / 403 `error-unauthorized` |
| `GET /api/apps/<unknown>/{settings,status,icon,languages}` | 404 `{"success":false,"error":"No App found by the id of: <id>"}` (settings as alice: 403) |
| `POST /api/apps/ui.interaction/:id` without auth | 401 `{"status":"error","message":"You must be logged in to do this."}` |
| same, unknown app id, `blockAction` | 500 `{"error":"Cannot read properties of undefined (reading 'call')"}` |
| same, unknown app id, `type:"bogus"` | 400 `{"error":"Unknown action"}` |
| same, core app `videoconf-core`, `actionButton` | 500 `{"error":"Type not supported"}` |
| same, `videoconf-core` `info` with a bad call id | 500 `{"error":"invalid-call"}` |
| same, `mention-core` `dismiss` (real ephemeral message) | 200 `{}` and a replacement ephemeral message with the **same `_id`** on `{uid}/message` |
| `POST /api/v1/apps/ui.interaction/...` | 404 plain text `404 Not Found` (the path has no `v1`) |
| `POST /api/apps/ui.interaction/<id>/` (trailing slash, RN form) | routed like the form without the slash |
| request body > 100 KiB | 413 with an **HTML** body (`<pre>Payload Too Large</pre>`); a ~101 000-byte body passes, ~103 000 bytes fails (limit 102 400 bytes) |
| `GET /api/v1/banners?platform=web` / without `platform` | `{"banners":[],"success":true}` / 400 `error-invalid-params` "must have required property 'platform'" |
| `GET /api/apps/marketplace` (admin) | a **bare JSON array** (73 entries), no envelope |
| a regular user (alice) sends `chat.sendMessage` with `private:true`, `groupable:false`, `t:"videoconf"`, `u:{_id:"rocket.cat"}` and `mention-core` blocks | accepted and **persisted** with `private`, `groupable`, `t` and the blocks as sent; only `u` is replaced by the real sender (§3.2) |
| `API_Enable_Rate_Limiter` / `API_Enable_Rate_Limiter_Dev` (admin `GET /api/v1/settings/:id`) | `false` / `true` |
| DDP subs `stream-notify-user` `{uid}/uiInteraction`, `{uid}/message`; `stream-apps` `apps`, `actions/changed` | all `ready` |
| a regular user (alice) sends `blocks` with `chat.sendMessage` | accepted; blocks stored and relayed verbatim, unknown block types included (REST and `__my_messages__`); `md` is built from `msg` only |
| `GET /api/v1/commands.list` | `{commands:[{command, params?, description, permission?, clientOnly, providesPreview}], appsLoaded:true, offset, count, total}` |
| `users.autocomplete` / `rooms.autocomplete.channelAndPrivate` | `{items:[{_id,name,username,nickname,status,statusText,avatarETag,score}]}` / `{items:[{_id,fname,name,t}]}` |
| permissions | `manage-apps: [admin]`, `access-marketplace: [admin, user]` |

An independent re-check on 2026-11-08 re-ran the REST and `ui.interaction` rows (admin; alice for
`installed` and the 403 rows), the `{uid}/message`, `{uid}/uiInteraction` and `stream-apps` `apps`
subscriptions, the `mention-core` mention + `dismiss` flow over DDP (§3.3), `commands.list`, the
autocomplete endpoints and the permissions, and added rows for the trailing slash, the exact body
limit, banners, marketplace, forged message fields and the rate-limiter settings (throwaway
channel `uikit-check-<random>`, deleted afterwards). A third check (2026-11-08, throwaway
channel `uikit-verify-<random>` created and deleted by alice) re-ran the `stream-apps`
`actions/changed` + `apps` subscriptions (both `ready`) and the `__my_messages__` half of the alice
blocks row (blocks incl. an unknown type relayed verbatim, `md` built from `msg`), and added:

| Claim | Result |
|---|---|
| `GET /api/apps/categories` (admin) | a **bare JSON array** (like `marketplace`) |
| `ui.interaction/core` `blockAction` (the web's fallback app id) | 500 `{"error":"Cannot read properties of undefined (reading 'call')"}` |
| `videoconf-core` `viewClosed` | 200 `{}` (no handler) |
| `videoconf-core` `join` with a bad call id | 500 `{"error":"invalid-call"}` |
| `banner-core` `viewClosed` whose `view` has no `viewId` | 500 `{"error":"invalid banner"}` (it reads `payload.view.viewId`, not `viewId`/`id`) |
| `mention-core` `actionButton` | 500 `{"error":"Type not supported"}` |

**No app was installed.** The marketplace is reachable (`GET /api/apps/marketplace` lists 73 apps,
e.g. "Poll" `c33fa1a6-68a7-491e-bf49-9d7b99671c48` with `ui.interact`, `ui.registerButtons`,
`slashcommand`), but marketplace installs download
`v2/apps/{id}/download/{version}?token=<workspace token>`, and the token is `''` for a
workspace that is not registered with Rocket.Chat Cloud (`SRV:server/lib/cloud/getWorkspaceAccessToken.ts`).
The marketplace answered `400 {"code":268,"error":"purchased required"}` to a token-less
download (first probe only; not re-checked). A private (zip) app would install but could not be
enabled: `POST /api/apps` and `POST :id/status` call `canEnableApp`, which throws
`license-prevented` when `License.shouldPreventAction('privateApps')` (UNVERIFIED that
`maxPrivateApps: 0` makes it true; `@rocket.chat/license` is not in `.ref`).
Both ways need a global state change (cloud registration / license), so they were skipped.
Consequently **not verified live**: real `modal.open`/`contextual_bar.*` views, `errors`
responses, action buttons with content, app settings and logs with content, `stream-apps`
event payloads. Those parts come from source.

The probe also used the server's built-in UIKit "core apps", which are reachable through the
same endpoint: `mention-core`, `videoconf-core`, `banner-core`, `nps-core`,
`cloud-announcements-core`, `cloud-communication-core` (`SRV:server/modules/core-apps/*.module.ts`,
registered in `SRV:server/startup/coreApps.ts`). `media-call-core` is **not** a server core app: the web
handles its only action locally (§2.3).

## 1. Overview

```
 message.blocks ─┐             (timeline)          app action buttons ──┐ (room header, message
 ephemeral msg ──┤                                  GET /api/apps/actionButtons   menu, composer, user menu)
                 ▼                                                       ▼
            render blocks ── user acts ──► POST /api/apps/ui.interaction/{appId}  {type, triggerId, ...}
                                                    │                     ▲
             slash command ── POST /api/v1/commands.run {triggerId} ─────┘ │
                                                    │                       │
                 HTTP response: {} | {success} | ServerInteraction          │
                 DDP push:  stream-notify-user "{uid}/uiInteraction"  ServerInteraction
                                                    ▼
             accept only if triggerId was issued by this client and not yet used
                                                    ▼
                 modal.open/update/close · contextual_bar.open/update/close · errors · banner.*
                                                    ▼
                 view (modal dialog / room panel) ── viewSubmit / viewClosed / blockAction(container view)
```

## 2. The JSON model (`UK:`)

### 2.1 Text objects

`UK:blocks/text/PlainText.ts`, `UK:blocks/text/Markdown.ts`, `UK:blocks/WithTranslations.ts`:

```jsonc
{ "type": "plain_text", "text": "Hello", "emoji": true,
  "i18n": { "key": "greeting", "ns": "app-<appId>", "args": { "name": "Ana", "count": 2 } } }
{ "type": "mrkdwn", "text": "*bold* _it_ `code` :smile:", "verbatim": false, "i18n": {…} }
```

- Rendering (web): `plain_text` is the literal string; `emoji` is ignored
  (`FUK:elements/PlainTextElement.tsx`). `mrkdwn` is parsed **on the client** with
  `@rocket.chat/message-parser` `parse(text, { emoticons: false })` and rendered with gazzodown
  (`FUK:elements/MarkdownTextElement.tsx`); `verbatim` is ignored. The server never adds an `md`
  tree for block text, so Rustket needs a client-side markdown parser producing the
  `rocketchat::models::md` AST (the phase 4 "fallback parser"; grammar in
  `RC:packages/message-parser`). Until it exists: render `mrkdwn` as plain text with `*bold*`,
  `_italic_`, `~strike~`, `` `code` `` and `:shortcode:` handled by a small inline tokenizer.
- `i18n`: if present, the text is `t(key, {...args, ns, defaultValue: text})`. Default
  namespace is `app-{appId}`; apps whose id ends with `-core` use the core translations
  (`FUK:hooks/useAppTranslation.ts`). i18next rules apply: `{{name}}` interpolation, plural
  suffix chosen from `args.count` (`key_one`, `key_other`, …), context suffix from
  `args.context` (`key_female`) (`FUK:elements/TextElement.spec.tsx`). Missing key → `text`.
  Core-app keys refer to Rocket.Chat's own translation files, which Rustket does not have, so
  for `*-core` apps always fall back to `text`.
- Placeholders of selects/inputs, select option labels and overflow option labels go through
  `FUK:hooks/useStringFromTextObject.ts`, which calls `t(key, args)` **without** `ns` and
  `defaultValue`: a missing key shows the key itself, not `text`. Rustket uses the `text` fallback
  everywhere. `users_select`/`multi_users_select` use `placeholder.text` without i18n;
  `channels_select`/`multi_channels_select` render **no** placeholder at all
  (`FUK:elements/ChannelsSelectElement/*.tsx`).
- Exceptions to the text rules: a callout `title` is shown as the raw `title.text` (no i18n, no
  markdown, `FUK:blocks/CalloutBlock.tsx`); the banner surface replaces the `mrkdwn` renderer with
  `<MarkdownText variant='inline' content={text}>` (i18n ignored, `WEB:views/banners/UiKitBanner.tsx`).
- A banner `title` may be a bare string (`UK:surfaces/banner/BannerView.ts`).

### 2.2 Shared pieces

- **Actionable** (every interactive element, `UK:blocks/Actionable.ts`):
  `{ appId, blockId, actionId, confirm?, dispatchActionConfig?: InputElementDispatchAction[] }`
  (`appId`/`blockId` are typed as required). In practice `appId` and `blockId` are usually missing
  on elements and are inherited:
  - actions/input blocks: `element.appId ?? block.appId`, `element.blockId ?? block.blockId`
    (`FUK:blocks/ActionsBlock.tsx`, `FUK:blocks/InputBlock.tsx`);
  - section accessory: `{appId: block.appId, blockId: block.blockId, ...accessory}` (the
    accessory's own values win) (`FUK:blocks/SectionBlock.tsx`);
  - context items: always the block's values (`FUK:blocks/ContextBlock.tsx`);
  - **no** inheritance for a callout `accessory`, an `info_card` row `action` and the `tab`
    elements of a `tab_navigation` block: the web renders the raw element (`FUK:blocks/CalloutBlock.tsx`,
    `InfoCard.tsx`, `TabNavigationBlock.tsx`). Rustket inherits there too (safe improvement);
  - a `video_conf` block without `appId` acts as `appId: 'videoconf-core'`
    (`FUK:blocks/VideoConferenceBlock/VideoConferenceBlock.tsx`);
  - final fallback when emitting: `element.appId || UiKitContext.appId || 'core'`
    (`FUK:hooks/useUiKitState.ts`). `UiKitContext.appId` is the view's `appId` in modals, contextual
    bars and banners; the message context sets none, so message elements without an inherited
    `appId` emit to `'core'`. (`FUK:contexts/AppIdContext.tsx`, set from each block's `appId`, only
    picks the translation namespace, it is not used for emitting.) There is no core app named
    `core`; such a click always fails (500, verified). Rustket renders such elements disabled.
  - Apps built with the apps-engine `BlockBuilder` always set `blockId` and `actionId`
    (uuid v1) and stamp `appId` on every layout block (`AE:uikit/blocks/BlockBuilder.ts`).
- **Option** `{ text: TextObject, value: string, description?: PlainText, url?: string }`,
  **OptionGroup** `{ label: PlainText, options: Option[] }` (`UK:blocks/Option.ts`,
  `UK:blocks/OptionGroup.ts`). `optionGroups` and option `description` are ignored by the web and RN
  renderers; Rustket may render groups as headed sections in the dropdown. Option `url` is used
  only by `overflow` (§2.4).
- **ConfirmationDialog** `{ title: PlainText, text: TextObject, confirm: PlainText, deny: PlainText, style: 'primary'|'danger' }`
  (`UK:blocks/ConfirmationDialog.ts`). Neither the web nor RN implements it (no reference in
  `FUK:` or `RN:app/containers/UIKit`). Rustket should show a confirm dialog before emitting
  the action (safe improvement: an element without `confirm` behaves the same).
- **dispatchActionConfig**: `'on_character_entered' | 'on_item_selected'`
  (`UK:blocks/InputElementDispatchAction.ts`), see §5.2.

### 2.3 Layout blocks

All carry optional `appId` and `blockId` (`UK:blocks/LayoutBlockish.ts`). Type strings:
`UK:blocks/LayoutBlockType.ts`.

| `type` | Shape | Web rendering (`FUK:blocks/*`) |
|---|---|---|
| `section` | `text?: TextObject`, `fields?: TextObject[]`, `accessory?: button\|datepicker\|image\|multi_static_select\|overflow\|static_select` | Grid: left column = `text` (p2) then `fields` in a nested grid (`GridItem xs/sm/md=4, lg/xl=6`, i.e. two fields per row on wide layouts; UNVERIFIED: fuselage's column count per breakpoint is not in `.ref`); right column = accessory (no grow) |
| `divider` | – | horizontal rule, 24 px margin above/below |
| `image` | `imageUrl`, `altText`, `title?: PlainText` | optional caption (small, hint colour, one line truncated) above; image scaled so the longest side ≤ 360 px, aspect kept; skeleton 360×154 while loading; left-aligned in messages/banners, centred in modals/contextual bars |
| `actions` | `elements: [button, channels_select, conversations_select, datepicker, linear_scale, multi_channels_select, multi_conversations_select, multi_static_select, multi_users_select, overflow, static_select, users_select, toggle_switch, checkbox, radio_button, time_picker]` | wrapping row, 4 px margin around each item (8 px between neighbours); non-buttons grow (`flex-basis 45%`), buttons keep their size; if more than 5 elements (not in banners) only the first 5 plus a literal **"Show more..."** button that reveals the rest (`ActionsBlock.tsx`) |
| `context` | `elements: (TextObject \| image element)[]` | single row, items vertically centred, text small (c1) in hint colour, 20 px rounded images |
| `input` | `label: PlainText`, `element: …`, `hint?: PlainText`, `optional?: bool` | form field: label, element, error text (from view errors, keyed by the element's `actionId`), hint. `optional` is not shown |
| `conditional` | `when?: {engine?: ('rocket.chat'\|'livechat')[]}`, `render: LayoutBlock[]` | resolved before rendering (§2.6) |
| `preview` | `title: TextObject[]`, `description: TextObject[]`, `footer?: context block`, plus `thumb: {url, dimensions?}` **or** `preview: {url, dimensions?}` + `externalUrl?` + `oembedUrl?` | link-preview card: cover image only when the block has an `externalUrl` or `oembedUrl` key **and** `preview.dimensions` is set, thumbnail 368×192 when a `thumb` key is present, title (a link when `externalUrl`), description clamped, footer = context block (`PreviewBlock.tsx`, `UK:blocks/layout/PreviewBlock.ts` `isPreviewBlockWith*`) |
| `video_conf` | `callId` | video-conference card, data from `video-conference.info` (§3.4) |
| `callout` | `title?: TextObject`, `text: TextObject`, `variant?: info\|danger\|warning\|success`, `accessory?: button\|overflow` | fuselage `Callout` (coloured box with icon) with `title.text` as plain title, `text` as body and the accessory rendered in SECTION context as its action |
| `tab_navigation` | `tabs: tab[]` (experimental) | tab strip; contextual bar only |
| `info_card` | `rows: [{background: 'default'\|'secondary', elements: (icon\|plain_text\|mrkdwn)[], action?: icon_button}]` | bordered card, max width 345 px, each row 16 px padding; `background:'default'` = lighter surface; elements in a row, the row's `action` right-aligned (`InfoCard.tsx`). Used by media-call messages (`appId: 'media-call-core'`, `actionId: 'open-history'`, handled locally by the web, `WEB:uikit/hooks/useMessageBlockContextValue.ts`) |

Spacing between top-level blocks: 16 px after each block in messages, modals and contextual
bars, 8 px above and below in banners (`FUK:surfaces/*Surface.tsx`).

### 2.4 Elements

Type strings: `UK:blocks/BlockElementType.ts`. Value types sent in interactions are what the
web sends (`FUK:elements/*`, `FUK:hooks/useUiKitState.ts`), which apps are written against.

| `type` | Fields | Web widget | Value sent |
|---|---|---|---|
| `button` | `text: PlainText`, `url?`, `value?`, `style?: primary\|secondary\|danger\|warning\|success`, `secondary?: bool` | small button, min width 4ch; spinner and disabled while its action is in flight. With `url` it is a link opening a new tab **and** the action is still emitted | `value` (string); a button without `value` sends `""`, a `url` button sends no `value` key |
| `overflow` | `options: Option[]` | kebab icon button → popup menu | chosen option `value` (string). The web means to `window.open(option.url)` too, but builds the option tuple with the URL at index 5 and reads index 4 in the click handler (`FUK:elements/OverflowElement.tsx`), so it most likely never opens it (UNVERIFIED: fuselage's `OptionType` layout is not in `.ref`). Rustket opens it (§3.2 URL rules) and emits |
| `static_select` | `placeholder: TextObject`, `options`, `optionGroups?`, `initialOption?`, `initialValue?` | filterable single select | string |
| `multi_static_select` | `placeholder`, `options`, `optionGroups?`, `maxSelectItems?`, `initialValue?: string[]`, `initialOption?: Option[]` | filterable multi select with chips (`maxSelectItems` ignored; the array `initialOption` is ignored too, `getInitialValue` reads `initialOption.value`) | `string[]` (whole selection) |
| `users_select` / `multi_users_select` | `placeholder?: PlainText` | autocomplete over `GET /api/v1/users.autocomplete?selector={"term":"<q>"}` (debounce 300 ms single, 500 ms multi); label = `name \|\| username`; single: avatar + label, multi: avatar + label + `(username)`, selected users as chips | **username** (not the user id) / `string[]` of usernames (`FUK:elements/UsersSelectElement/hooks/useUsersData.ts`) |
| `channels_select` / `multi_channels_select` | `placeholder?: PlainText` (not rendered by the web) | autocomplete over `GET /api/v1/rooms.autocomplete.channelAndPrivate?selector={"name":"<q>"}` (300 ms, single and multi), room avatar + `name \|\| fname` | room **`_id`** / `string[]` (`FUK:elements/ChannelsSelectElement/hooks/useChannelsData.ts`) |
| `conversations_select` / `multi_conversations_select` | – (`@todo` in the typings) | **not rendered** by the web (no renderer method) | – |
| `datepicker` | `placeholder?: TextObject`, `initialDate?` | native `<input type=date>` | `"YYYY-MM-DD"` |
| `time_picker` | `placeholder?`, `initialTime?` | native `<input type=time>` | `"HH:MM"` (24 h) |
| `plain_text_input` | `placeholder?`, `initialValue?`, `multiline?`, `minLength?`, `maxLength?` | text input, or 6-row textarea when `multiline`; min/max length ignored | string |
| `linear_scale` | `minValue=0`, `maxValue=10`, `initialValue?`, `preLabel?`, `postLabel?` | row of small numbered buttons from min to max (at least one), selected one highlighted, labels at both ends | clicked point as a **string** (`"3"`); the initial value stays a number |
| `checkbox` | `options`, `initialOptions?` | vertical list: checkbox + label (label is a text object, mrkdwn allowed) | state: `string[]`; an emitted block action carries only the **toggled option's value** (string). Web quirk: without `initialOptions` the local value starts `undefined`, becomes the clicked option's string and `checked` is then a substring test |
| `toggle_switch` | `options`, `initialOptions?` | as checkbox, with switches | same as checkbox. Without `initialOptions` the web renderer throws (`value.includes` on `undefined`, `FUK:elements/ToggleSwitchElement.tsx`); Rustket treats a missing list as `[]` |
| `radio_button` | `options`, `initialOption?` | vertical radio list with labels | string |
| `image` | `imageUrl`, `altText` | 88 px square (cover crop, 4 px radius) as a section accessory, 20 px in a context block, nothing elsewhere (`FUK:elements/ImageElement.tsx`) | not interactive |
| `tab` | `title: TextObject`, `disabled?`, `selected?` | tab item, only inside `tab_navigation` (the surface renderer has no `tab` method); clicking a non-disabled tab selects it locally. The web emits **nothing**: tabs are rendered with the block's BLOCK context, so `useUiKitState` hands them the state-only handler (it emits only if the tab carries `dispatchActionConfig: ['on_item_selected']`) and the click just stores the event target's `value` under the tab's `actionId` in view state (`FUK:blocks/TabNavigationBlock.tsx`, `FUK:hooks/useUiKitState.ts`). Rustket emits a `blockAction` without `value` (a tab that tells the app nothing is useless; UNVERIFIED what experimental-tab apps expect) | no meaningful value |
| `icon` | `icon: phone\|phone-off\|phone-issue\|clock\|arrow-forward\|info\|phone-question-mark`, `variant: default\|danger\|secondary\|warning`, `framed?` | 20 px icon, colour by variant; framed = icon in a tinted square (danger/warning tint, else neutral) | – |
| `icon_button` | `icon: icon`, `label?`, `url?`, `value?` | small icon button (label = accessible label/tooltip); without `url` it is danger-coloured unless `icon.variant` is `default`; with `url` a link opening a new tab (action still emitted) | like `button` |

Initial values come from `initialValue || initialTime || initialDate || initialOption.value || initialOptions.map(value)`
(`FUK:utils/getInitialValue.ts`). The `||` drops `0` and `""`; Rustket treats them as values.

**Toast bar**: there is no toast/toast-bar element or block in the 8.9.1 ui-kit (`UK:` has none,
`AE:uikit` has none). The only toasts are the client's own messages (e.g. the timeout toast).

### 2.5 Which blocks each surface renders

The typed allow-lists in `UK:surfaces/*/UiKitParser*.ts` are **overridden** by the web renderers
(`FUK:surfaces/FuselageMessageSurfaceRenderer.tsx`, `ModalSurfaceRenderer.tsx`,
`ContextualBarSurfaceRenderer.tsx`, `BannerSurfaceRenderer.tsx`, which keeps the default list of
`FuselageSurfaceRenderer.tsx`).
Blocks not in the list are skipped silently (`UK:rendering/SurfaceRenderer.ts` `render`); so are
listed blocks the renderer has no method for (`UK:rendering/renderLayoutBlock.ts` returns `null`).

| Surface | ui-kit typings | Web 8.9.1 renders | Rustket |
|---|---|---|---|
| message | actions, context, divider, image, section, preview, video_conf, callout (+ info_card in the type) | actions, context, divider, image, **input**, section, preview, video_conf, info_card (**no callout**) | union: actions, context, divider, image, section, preview, video_conf, callout, info_card; `input` rendered read-only (in the web a message `input` element without `dispatchActionConfig` only keeps a local value that is never sent, §5.2) |
| modal | actions, context, divider, image, input, section, callout | + preview | actions, context, divider, image, input, section, preview, callout |
| contextual bar | actions, context, divider, image, input, section | + preview, callout, tab_navigation | same as web |
| banner | actions, context, divider, image, input, section (+callout in type) | actions, context, divider, image, input, section, preview, info_card | same as web |

RN does **not** render `callout` either: its `MessageParser` uses the ui-kit message list (which
contains `callout`) plus `info_card`, but has no `callout` method, so the block renders nothing
(`RN:app/containers/UIKit/index.tsx`; RN also has no `preview` renderer). Rendering `callout` in
messages is still allowed by the typings and harmless (read-only box plus a button/overflow), so
Rustket keeps it.

Element placement in the web renderer (`FUK:surfaces/FuselageSurfaceRenderer.tsx`,
`UK:rendering/SurfaceRenderer.ts`): the per-block element lists of the typings are **not
enforced**. `renderActionsBlockElement` / `renderInputBlockElement` /
`renderSectionAccessoryBlockElement` / `renderContextBlockElement` drop an element only when the
surface does not allow that block type at all **and** the element fails the matching
`is*BlockElement` check, so on every built-in surface any element type renders in any slot (e.g. a
`plain_text_input` inside an `actions` block renders in ACTION context and emits a block action,
with the loading state that disables it, on every keystroke). Whether a rendered element emits
depends on its context and `dispatchActionConfig` (§5.2): a button in a `context` block renders in
CONTEXT context, so a click merely updates view state and **emits nothing** (in a message, nothing
at all). Exceptions:
`users_select`, `channels_select` and their multi forms render **only inside `input` blocks**
(FORM context); `image` only as section accessory (88 px) or context item (20 px);
`conversations_select` / `multi_conversations_select` and `tab` (outside `tab_navigation`) never.
Unknown element types render nothing. Rustket restricts elements to the element unions typed on
each block (`UK:blocks/layout/*.ts`: `ActionsBlock.elements`, `InputBlock.element`,
`SectionBlock.accessory`, `ContextBlock.elements`, `CalloutBlock.accessory`, info-card rows), plus
the web's FORM-only rule for users/channels selects; out-of-list elements render nothing. Do not
use the `UK:blocks/is*BlockElement.ts` helpers for this: they are narrower than the types (e.g.
`isInputBlockElement` has no checkbox, radio, toggle, time picker or multi users/channels select).

### 2.6 Conditional blocks

`UK:rendering/resolveConditionalBlocks.ts`: before filtering, each `conditional` block is replaced
by its `render` list if `when.engine` is absent or contains the client's engine, else dropped.
The web passes `{engine: 'rocket.chat'}` (`FUK:surfaces/createSurfaceRenderer.tsx`); Rustket does
the same. Not recursive (blocks inside `render` are plain layout blocks).

## 3. Blocks in messages

### 3.1 Where blocks are shown

`WEB:components/message/variants/room/RoomMessageContent.tsx` (thread variant is the same):

1. E2E placeholder, 2. quote attachments,
3. the markdown body **only if `blocks` is empty or missing** (`!blocks?.length && md?.length`),
4. other attachments,
5. `blocks` rendered with the message surface (`WEB:components/message/uikit/UiKitMessageBlock.tsx`,
   fixed width `MessageBlock`),
6. URL previews, legacy `actionLinks`, reactions, thread/discussion metrics, …

So a message with blocks hides its `msg` text (it is only the fallback for notifications and
clients without UIKit). Rustket's timeline must follow the same rule.

### 3.2 Anyone can send blocks (verified)

`chat.sendMessage` accepts `message.blocks: object[]` from any user
(`RC:packages/rest-typings/src/v1/chat.ts` schema; `SRV:server/lib/messages/sendMessage.ts`
`validateMessage` checks `blocks: [Match.Any]`). Verified: alice posted the block set from §10.4
including `info_card`, `input`, two `conditional`s and `{"type":"unknown_future_block"}`; the stored
message (`chat.getMessage`) and the `__my_messages__` event carried the blocks unchanged, plus
`md` parsed from `msg: "fallback text"`, `urls: []`.

Consequences:
- Parse tolerantly: unknown block/element types become `Unknown(Value)` and render nothing
  (or a debug placeholder), never fail the message.
- Blocks are untrusted input. A member can forge buttons that carry the `appId` of a real app
  or a core app. Example: a fake `mention-core` "Add them" button whose `value` lists
  arbitrary users and whose `blockId` is any existing message id — when a room owner clicks it,
  the server adds those users with the clicker's permissions
  (`SRV:server/modules/core-apps/mention.module.ts` `add-users`; the referenced message is only
  looked up by id, its room is not compared with `rid`); "Let them know" (`share-message`) makes
  the clicker post a link message to every `_id` in the forged list through
  `processWebhookMessage`: a leading `#`/`@` selects a channel name / username, else an `_id`
  that is a room id or name is joined (public rooms) and posted to, otherwise it is treated as a
  user id and DMed (`SRV:server/lib/messages/processWebhookMessage.ts`).
- Ephemeral messages and pushed interactions cannot be forged by other clients:
  `stream-notify-user` `allowWrite` accepts only the `video-conference` and `media-calls` events
  (`RC:packages/streamer/src/notifications.module.ts`), so `{uid}/message` and
  `{uid}/uiInteraction` only carry server broadcasts.
- **Message fields cannot prove origin** (verified, §0): `chat.sendMessage` stores
  client-supplied `private`, `groupable` and `t` (e.g. `t: 'videoconf'`), and `_id` is
  client-chosen; only `u` is set by the server. Rustket rule: block actions whose resolved
  `appId` ends with `-core` are enabled only in messages that arrived as
  `SessionEvent::EphemeralMessage` (`stream-notify-user` `{uid}/message`, never persisted) with
  `u._id == 'rocket.cat'`. The store must keep that "ephemeral" flag next to the message (today
  `crates/rustket/src/session.rs` merges ephemeral messages with `apply_message`, losing the
  origin) and must drop it whenever a message with the same `_id` arrives from the room stream,
  history or `chat.getMessage`. Exception: the `video_conf` block's own actions (`join`,
  `callBack`, `info`, §3.4) are allowed in any message: `join` and `callBack` are handled by the
  client (`callBack` uses the message's own `rid`; joining goes through REST
  `video-conference.join`, which checks access to the conference), and `info` only reads call
  info. Note that the core app does **not** check access: `videoconf-core` `info` calls
  `VideoConf.getInfo(callId, uid)` and `join` calls `VideoConf.join`, neither of which checks room
  membership (`SRV:server/services/video-conference/service.ts`), so Rustket must never send
  `join` to `videoconf-core`; `info` returns `[]` for calls not managed by an app, otherwise the
  provider's info blocks or a fallback section with the call URL (anyone can send that request
  directly anyway). Everything else with a `-core` `appId` in a persisted message is rendered
  disabled, with one more exception: a `url` button still opens its URL (local action, §3.2 URL
  rules) but emits nothing. This matters for omnichannel video calls: `createLivechatMessage`
  persists a section plus an actions block whose only element is a `url` button
  (`appId: 'videoconf-core'`, `actionId: 'joinLivechat'`, `blockId: callId`, text `Join_call`);
  `videoconf-core` ignores `joinLivechat` anyway (no handler for it, answers `{}`).
- `url` buttons show the target host in a tooltip and go through `Effect::OpenUrl`
  (http/https/mailto only); the web opens any URL (`UK:blocks/elements/ButtonElement.ts` carries
  a TODO about the missing validation).
- Respect the existing md limits (`protocol.md`): apply the same depth/size guard to `blocks`
  before decoding (e.g. ≤ 32 levels, ≤ 20 000 values, ≤ 1 MiB) and cap rendered blocks
  (e.g. 50 top-level blocks, 25 elements per actions block).

### 3.3 Ephemeral messages with blocks (verified)

Ephemeral messages arrive on `stream-notify-user` `{uid}/message`
(`RC:packages/streamer/src/listeners.module.ts` `notify.ephemeralMessage`; Rustket already
subscribes it and emits `SessionEvent::EphemeralMessage`), are never persisted and default to
`private: true`, `groupable: false`, `u: {_id:'rocket.cat', username:'rocket.cat'}`, `_id:
String(Date.now())`, `ts`/`_updatedAt: now`. These are only defaults: the message's own fields
are spread over them, so app ephemerals (`IModify.getNotifier().notifyUser/notifyRoom`,
`SRV:app/apps/server/bridges/messages.ts`) carry the sender the app chose, a random 17-char `_id`
when the app set none (`convertAppMessage`, `SRV:app/apps/server/converters/messages.ts`) and keep
`private: true`. Because the spread copies keys whose value is `undefined`, an explicit
`undefined` in the message wins over the default: app ephemerals carry `_updatedAt:
message.updatedAt`, which is usually `undefined`, so they most likely arrive **without**
`_updatedAt` (from source; UNVERIFIED live, needs an installed app). Decode ephemerals with
`_updatedAt` optional. The server adds `md` when `msg` is non-empty. Ephemerals may carry `tmid` (e.g. a mention inside a thread,
`SRV:server/hooks/messages/mentionUserNotInChannel.ts`) and then belong in that thread's panel.
Captured after admin sent `hello @bob` in a channel bob is not in (`blockId` is the `_id` of
admin's `hello @bob` message):

```json
{"groupable":false,"u":{"_id":"rocket.cat","username":"rocket.cat"},"private":true,
 "_id":"1794088390781","rid":"6aef9dc6ff2338a3f2a2ea1a","ts":{"$date":1794088390782},
 "_updatedAt":{"$date":1794088390782},"msg":"",
 "mentions":[{"_id":"SLiu9igvh94B9T9Xu","username":"bob","name":"Bob","type":"user"}],
 "blocks":[
  {"appId":"mention-core","type":"section","text":{"type":"mrkdwn","text":"You mentioned *bob*, but they're not in this room."}},
  {"type":"actions","appId":"mention-core","elements":[
   {"type":"button","appId":"mention-core","blockId":"vSxru3XTbnFh8HJrc","actionId":"add-users",
    "value":"[{\"_id\":\"SLiu9igvh94B9T9Xu\",\"username\":\"bob\",\"name\":\"Bob\",\"type\":\"user\"}]",
    "text":{"type":"plain_text","text":"Add them"}},
   {"type":"button","appId":"mention-core","blockId":"vSxru3XTbnFh8HJrc","actionId":"dismiss","value":"…","text":{"type":"plain_text","text":"Do nothing"}},
   {"type":"button","appId":"mention-core","blockId":"vSxru3XTbnFh8HJrc","actionId":"share-message","value":"…","text":{"type":"plain_text","text":"Let them know"}}]}]}
```

Clicking "Do nothing" with

```json
{"type":"blockAction","actionId":"dismiss",
 "payload":{"blockId":"vSxru3XTbnFh8HJrc","value":"[{…}]"},
 "container":{"type":"message","id":"1794088390781"},
 "mid":"1794088390781","rid":"6aef9dc6ff2338a3f2a2ea1a","triggerId":"<17 chars>"}
```

answered `200 {}` and pushed a new ephemeral message with the **same `_id`** (`mention-core`
reuses the request's `mid`), new `ts`, no `blocks`, `msg: "You mentioned @bob, but they're not in
this room."` and an `md` tree (re-verified 2026-11-08). The
timeline must therefore upsert ephemeral messages by `_id` (replace, keep position by the
new `ts`). The buttons shown depend on the sender's permissions `add-user-to-joined-room` /
`add-user-to-any-c-room` / `add-user-to-any-p-room` and `create-d`
(`SRV:server/hooks/messages/mentionUserNotInChannel.ts`). Rustket should not send
`share-message` / `add-users` in tests (they create DMs / add users).

### 3.4 `video_conf` blocks

Messages with `t: 'videoconf'` carry `[{type:'video_conf', callId, appId:'videoconf-core', blockId: callId}]`
(`SRV:server/services/video-conference/service.ts` `createMessage`; the caller may pass
`customBlocks` instead, and any user can post `t: 'videoconf'` too, §3.2). Rendering
(`FUK:blocks/VideoConferenceBlock/VideoConferenceBlock.tsx`): data from
`GET /api/v1/video-conference.info?callId=` refreshed when `stream-notify-room` `{rid}/videoconf`
sends the call id; states: ended ("Call ended", direct calls get "Call again"/"Call back",
"Call was not answered" for expired/declined), direct calling ("Calling… Waiting for answer"),
ongoing ("Call ongoing", primary "Join", stack of up to 3 joined avatars + "+N joined"); ended
group calls show the joined-users stack, or "Call was not answered" when nobody joined and the
call expired/was declined. Only users who actually joined count (`users` is the membership
list). An info icon and, if `discussionRid`, a discussion icon (opens the discussion room
locally). Actions:
- `join` (`blockId` = `value` = the block's `blockId`, which the server sets to the call id) and
  `callBack` (`blockId` = `value` = rid) are handled **locally** for `appId === 'videoconf-core'`
  (`WEB:uikit/hooks/useMessageBlockContextValue.ts`) → Rustket's phase 9 video-conference module
  (external browser). If sent to the server anyway, `videoconf-core` handles `join` by calling
  `VideoConf.join` and answers `{}`.
- `info` (`blockId` = callId, `value` = rid) is sent to the server; `videoconf-core` answers in
  the HTTP body with `modal.open` (no `success` key; view id `{callId}-info`, title
  `i18n.t('Video_Conference_Info')`, close button `actionId: 'cancel'`, no submit)
  (`SRV:server/modules/core-apps/videoconf.module.ts`). Closing it sends `viewClosed` to
  `videoconf-core`, which has no handler and answers `{}`.

## 4. Interaction protocol

### 4.1 Endpoint

`POST {server}/api/apps/ui.interaction/{appId}` — **not** under `/api/v1`
(`SRV:ee/server/apps/communication/uikit.ts`; typed as `'/apps/ui.interaction/:id'` in
`RC:packages/rest-typings/src/apps/index.ts`; RN posts to `/api/apps/ui.interaction/{appId}/`
with a trailing slash, `RN:app/lib/services/appsApiFetch.ts`).

- Headers: `X-User-Id`, `X-Auth-Token`, `Content-Type: application/json` (the express router
  uses `authenticationMiddleware`; omnichannel visitors use `x-visitor-token`, not relevant).
- Body limit: express `bodyParser.json()` default **100 KiB** (102 400 bytes); larger → 413 with an
  HTML body (verified). See §5.3 for how Rustket keeps `viewSubmit` under the limit.
- Rate limit: when `API_Enable_Rate_Limiter` is on, 60 × `API_Enable_Rate_Limiter_Limit_Calls_Default`
  per `API_Enable_Rate_Limiter_Limit_Time_Default` window (`express-rate-limit`, `uikit.ts`); also
  skipped when `NODE_ENV=development` unless `API_Enable_Rate_Limiter_Dev`. Off on the dev server
  (`API_Enable_Rate_Limiter: false`, verified). The limiter is added with `router.use` inside
  `Meteor.startup`, i.e. after the core-app `router.post('/:id')` handler was registered at module
  load; express runs handlers in registration order and the core-app handler answers without
  calling `next()`, so core-app interactions most likely never reach the limiter. Whether
  installed-app interactions do depends on whether `AppUIKitInteractionApi` (constructed in
  `Apps.initialize()`, `SRV:ee/server/apps/startup.ts`) registers its route before or after that
  startup callback. UNVERIFIED: both orderings, and the exact 429 body/headers of
  `express-rate-limit` (they differ from the REST API's `x-ratelimit-*` mapping in `protocol.md`).
- Routing: core apps (`UiKitCoreApp.isRegistered`) handle `blockAction`, `viewSubmit`,
  `viewClosed` (others → 500 "Type not supported") and answer `result ?? {}`. Installed apps
  go through `AppUIKitInteractionApi.routeHandler`, which converts `rid`/`mid` to app objects
  and calls the app's `executeBlockActionHandler` / `executeViewSubmitHandler` /
  `executeViewClosedHandler` / `executeActionButtonHandler`
  (`RC:packages/apps/src/server/managers/AppListenerManager.ts` `executeUIKitInteraction`).
  `mid` is converted with `convertById`, so a block action on an app's **ephemeral** message
  (never stored) reaches the app with `message: undefined`; the core-app path passes `mid`/`rid`
  through as plain strings.
  A missing `executeViewClosedHandler` falls back to `{success:true}`; other missing handlers
  return nothing (empty 200 body), and so does a handler that returns nothing (`res.send(undefined)`).
  `viewClosed` for installed apps is forwarded **without** `triggerId` (neither the router nor
  `executeUIKitInteraction` passes it), so any interaction an app returns from
  `executeViewClosedHandler` has no valid trigger and is ignored by clients; core apps do get it.
- Unknown/uninstalled app → 500 `{"error":"Cannot read properties of undefined (reading 'call')"}`;
  unknown `type` → 400 `{"error":"Unknown action"}`; app exceptions → 500 `{"error": "<message>"}`.
  Error bodies have neither `success` nor `errorType`.

The existing (private) `RestClient::send_value` treats `success:false` as an error, maps an empty
body to `{}`, and the typed endpoint methods build URLs with `ServerUrl::api` (`/api/v1`). UIKit
needs its own call (`ServerUrl::api_root("apps/ui.interaction/{appId}")`) that
returns the raw JSON for any 2xx (an `errors` response is `success:false` with HTTP 200) and maps
non-2xx bodies `{error}` / HTML to `Error::Status`.

### 4.2 `triggerId`

- Generated by the client for every interaction: 17-char Meteor random id
  (`Random.id()`, `WEB:lib/ActionManager.ts` `generateTriggerId`; RN `random(17)`).
  Use `rocketchat::ids::random_id()` (17 chars, same as `random_id_len(17)`).
- Registered as `triggerId → appId` for **5 s** (`TRIGGER_TIMEOUT = 5000`). RN keeps it until
  5 s after the HTTP request finished (`RN:app/lib/apps/actions.ts` `withTriggerId`), which is
  more forgiving for slow apps; Rustket uses the RN rule.
- A server interaction (HTTP body or `{uid}/uiInteraction` push) is **accepted only if its
  `triggerId` is registered and unused**; acceptance consumes it
  (`ActionManager.handleServerInteraction` → `invalidateTriggerId`; RN `consumed`). Pushes are
  delivered to every connection of the user (all devices and tabs), so this check is what
  keeps a modal from opening on every client. Responses without `triggerId` (`{}`,
  `{success:true}`) consume nothing. Neither the web nor RN compares the interaction's `appId`
  with the trigger's app; Rustket does not either (core apps answer with their own `appId`).
- The web expires the trigger 5 s after issuing it, also while the HTTP request is still
  running: a late HTTP answer is then ignored (and the timeout path below already ran).
- Slash commands register a trigger too and pass it to `commands.run` (§7). The web registers it
  with the command's `appId`, which is `undefined` for built-in commands, so pushes carrying a
  built-in command's trigger are ignored (`handleServerInteraction` returns when the stored appId is
  falsy).
- Apps that open a view later (scheduler, webhook) with an old trigger are ignored by design.

### 4.3 User interactions (client → server)

`UK:interactions/UserInteraction.ts`. All carry `triggerId`.

**blockAction from a message** (`WEB:uikit/hooks/useMessageBlockContextValue.ts`):

```json
{"type":"blockAction","actionId":"<actionId>",
 "payload":{"blockId":"<blockId>","value":<value or key absent>},
 "container":{"type":"message","id":"<mid>"},
 "mid":"<mid>","rid":"<rid>","triggerId":"…"}
```

(`tmid` is allowed by the type but the web never sends it.)

**blockAction from a view** (modal: no `rid`; contextual bar: with `rid`)
(`WEB:uikit/hooks/useModalContextValue.ts`, `useContextualBarContextValue.ts`):

```json
{"type":"blockAction","actionId":"…","payload":{"blockId":"…","value":…},
 "container":{"type":"view","id":"<viewId>"},"rid":"<rid, contextual bar only>","triggerId":"…"}
```

Banner block actions are the same with the banner `viewId`, and the banner is closed right
after (`WEB:uikit/hooks/useBannerContextValue.ts`).

**viewSubmit** (`WEB:views/modal/uikit/UiKitModal.tsx`, `WEB:views/room/contextualBar/uikit/UiKitContextualBar.tsx`):

```json
{"type":"viewSubmit","viewId":"<viewId>",
 "payload":{"view":{ …the whole current view object…, "state":{"<blockId>":{"<actionId>":<value>}}}},
 "rid":"<contextual bar only>","triggerId":"…"}
```

**viewClosed**:

```json
{"type":"viewClosed",
 "payload":{"viewId":"<viewId>","view":{ …view…, "state":{…}},"isCleared":false},
 "rid":"<contextual bar only>","triggerId":"…"}
```

`isCleared: false` for the footer close button, `true` for the X button and backdrop click (and
Escape in Rustket, §5.3). RN always sends `true`. Banners send `{…view, id: view.viewId, state}` with `isCleared:true`
to `banner-core`, which dismisses the banner and answers `banner.close`
(`WEB:views/banners/UiKitBanner.tsx`, `SRV:server/modules/core-apps/banner.module.ts`).

**actionButton** (§6): `payload.context` is the button context.

```jsonc
{"type":"actionButton","actionId":"…","rid":"<rid>","payload":{"context":"roomAction"},"triggerId":"…"}
{"type":"actionButton","actionId":"…","rid":"<rid>","tmid":"<tmid?>","mid":"<mid>","payload":{"context":"messageAction"},"triggerId":"…"}
{"type":"actionButton","actionId":"…","rid":"<rid>","tmid":"<tmid?>","payload":{"context":"messageBoxAction","message":"<composer text>"},"triggerId":"…"}
{"type":"actionButton","actionId":"…","payload":{"context":"userDropdownAction"},"triggerId":"…"}
```

### 4.4 Responses and server interactions

HTTP 200 bodies (`AE:uikit/UIKitInteractionResponder.ts`, `AE:uikit/UIKitInteractionPayloadFormatter.ts`):

| Body | Meaning |
|---|---|
| empty, `{}`, `{"success":true}`, `{"success":false}` | acknowledged, nothing to show (an empty body counts as "no answer" in the §4.6 `viewSubmit` close decision) |
| `{"success":true,"type":"modal.open"\|"modal.update","triggerId","appId","view":{appId, type:"modal", id, …, showIcon:true}}` | open / replace modal |
| `{"success":true,"type":"contextual_bar.open"\|"contextual_bar.update", …, "view":{…, type:"contextualBar"}}` | open / replace contextual bar |
| `{"success":false,"type":"errors","triggerId","appId","viewId","errors":{"<actionId>":"message"}}` | validation errors |
| core apps: a ServerInteraction without `success` | e.g. `modal.open` from `videoconf-core`, `banner.close` from `banner-core`, `banner.close` or `modal.close` **with `viewId`** from `cloud-announcements-core` `viewClosed` |

Server interactions (`UK:interactions/ServerInteraction.ts`), same shape on HTTP and on the stream:

```ts
{ type:'modal.open'|'modal.update', triggerId, appId, view: ModalView }
{ type:'modal.close', triggerId, appId }                 // apps-engine formatter also allows `view`;
                                                         // cloud-announcements-core sends `viewId`
{ type:'contextual_bar.open'|'contextual_bar.update'|'contextual_bar.close', triggerId, appId, view: ContextualBarView }
{ type:'banner.open', triggerId, appId, ...BannerView }  // view fields at top level
{ type:'banner.update', triggerId, appId, view: BannerView }
{ type:'banner.close', triggerId, appId, viewId }
{ type:'errors', triggerId, appId, viewId, errors: {[actionId]: string}[] | {[actionId]: string} }
```

- View ids: apps get a uuid v1 when they pass none; `formatModalInteraction` /
  `formatContextualBarInteraction` always set `showIcon: true` and `type` (`modal` /
  `contextualBar`).
- `errors`: the typings say an array of objects, apps-engine sends one object; the web accepts
  both (`useUiKitState`: `Array.isArray(errors) ? errors.find(e => actionId in e)[actionId] : errors[actionId]`).
  Normalise into `HashMap<actionId, String>`.

### 4.5 Server push: `stream-notify-user` `{uid}/uiInteraction`

Args `[ServerInteraction]` (`RC:packages/ddp-client/src/types/streams.ts`). Sent when an app
uses `IUIController.openSurfaceView/updateSurfaceView` (current API; `UIHelper.assignIds` stamps
each top-level block's missing `appId` with the app's id and missing `blockId` with a random UUID,
and missing `actionId` with a random UUID only on `block.elements[]` items, not on an input block's
`element` or a section `accessory`) or the deprecated
`openModalView/updateModalView/openContextualBarView/updateContextualBarView`, or `setViewError`
(`AE:accessors/IUIController.ts`, `RC:packages/apps/base-runtime/src/lib/accessors/modify/UIController.ts`).
So pushes are only `modal.open|update`, `contextual_bar.open|update` and `errors`; there is no
close method. The app needs the `ui.interact` permission, else nothing is
sent (`RC:packages/apps/src/server/bridges/UiInteractionBridge.ts`) — via
`api.broadcast('notify.uiInteraction', uid, interaction)` (`SRV:app/apps/server/bridges/uiInteraction.ts`,
`RC:packages/streamer/src/listeners.module.ts`). Rustket already subscribes it and emits
`SessionEvent::UiInteraction(Value)` (`crates/rocketchat/src/session/events.rs`).

### 4.6 Client algorithm

```
emit(appId, interaction_without_trigger, origin):
    trigger = registry.issue(appId, origin)          // origin: rid/tmid/mid, view id, surface
    busy += 1                                        // global "Loading" pill
    mark element/view loading
    resp = POST /api/apps/ui.interaction/{appId} with triggerId, timeout 5 s
    busy -= 1; registry.settle(trigger)
    match resp:
      timeout      -> toast "App has failed to respond. Please try again or contact your admin"
                      (UIKit_Interaction_Timeout); for viewSubmit close that view (the web
                      disposes only when the interaction has a top-level `viewId`, i.e. viewSubmit;
                      view block actions and viewClosed keep the view)
      HTTP error   -> log; toast (generic app error); views stay open
      empty body   -> no answer (distinct from `{}`; RN treats it so, see close decision)
      body with type -> handle_server(body)
    close decision for viewSubmit / viewClosed (below)

handle_server(si):                                   // HTTP body or stream push
    Some(t) = registry.accept(si.triggerId) else ignore   // unknown, expired or used
    match si.type:
      modal.open             -> replace the current modal (one modal at a time) with fresh state
                                (web: `imperativeModal.open`, which has a separate `push`, so
                                `open` presumably replaces, `RC:packages/ui-client/src/helpers/
                                imperativeModal.tsx`; UNVERIFIED, the `ModalStore` it delegates to
                                is not in `.ref`. RN also replaces: `Navigation.navigate` to the
                                open `ModalBlockView` changes its params and the screen then
                                `navigation.replace`s itself on a new `triggerId`,
                                `RN:app/views/ModalBlockView.tsx`; from source, UNVERIFIED on a
                                device since it relies on react-navigation's `navigate` semantics)
      modal.update           -> if open modal.id == view.id: replace view, keep values (§5.2)
                                (web shallow-merges `{...oldView, ...newView}` and keeps errors)
      modal.close            -> close the open modal with id == (si.view?.id ?? si.viewId), else
                                the open modal if it belongs to si.appId. The web's handler
                                does nothing for modal.close (it only closes as a viewSubmit
                                answer, like any non-keep type); RN closes on an HTTP-body
                                modal.close and records a pushed one as `handled` (it closes the
                                view only when the push names a view id)
      contextual_bar.open    -> open panel Kind::App(view.id) in the origin room
                                (t.origin.rid, else the active room, else show as a dialog)
      contextual_bar.update  -> replace view if that bar is open
      contextual_bar.close   -> close bar view.id
      errors                 -> set errors on the open view with id == viewId
      banner.*               -> banner store (§5.7)
      unknown                -> log, toast "unsupported app action" (RN: App_action_unsupported)
    record t.handled = si.type

close decision:
    viewSubmit: keep open if the accepted interaction for this trigger (HTTP or push) was
                errors / modal.update / contextual_bar.update; close otherwise, but only if
                the HTTP call succeeded (2xx) with a non-empty body or a push already decided.
                Empty body and nothing pushed -> keep open (RN: `postUserInteraction` returns
                `handled`, i.e. undefined, so `triggerSubmitView` is false). HTTP error -> keep
                open, show error. The web keeps it open too unless its REST client maps an empty
                body to an object (UNVERIFIED: `@rocket.chat/api-client` is not in `.ref`; a thrown
                parse error or a falsy result keeps it open because of `!!interaction`).
                If the accepted interaction was `modal.open`, the submitted modal has already
                been replaced: close nothing, never the new modal, even when it reuses the
                submitted view id. (The web runs `disposeView(submittedViewId)` after `openModal`
                has registered the new instance in `viewInstances`: with a reused id that closes
                the new modal; with a new id it calls the replaced instance's `close()`, whose
                effect depends on `ModalStore`, UNVERIFIED. RN's `triggerSubmitView` is false for
                `modal.open`, so it keeps the screen, which its navigation has replaced.)
    viewClosed: the view is closed locally at once (Rustket: do not wait). The web waits and
                disposes only if the HTTP call returned a body that is not `errors`: a failed
                call (e.g. 500 from an uninstalled app) or a timeout (viewClosed has no
                top-level `viewId`) leaves the web's modal open.
```

Web reference: `ActionManager.emitInteraction`/`runWithTimeout`/`handleServerInteraction`
(`WEB:lib/ActionManager.ts`). The web decides closing from the HTTP body only, so a submit
handled by pushing `modal.update` and returning `{success:true}` closes the just-updated modal;
RN tracks the pushed type (`RN:app/lib/apps/actions.ts` `KEEPS_VIEW_OPEN`, `handled`) — Rustket
follows RN. The web swallows non-timeout HTTP errors silently; Rustket shows a toast.

Busy indicator (web `WEB:components/ActionManagerBusyState.tsx`): a small "Loading" pill at the
top centre of the window while an interaction is in flight (the web keeps a boolean, so the first
of two overlapping requests to finish hides it; Rustket counts). Element loading: the clicked
element shows a spinner and is disabled until the request finishes
(`useUiKitState` `loading`); `on_character_entered` inputs are not disabled.

## 5. Views

### 5.1 Shapes

```ts
ModalView         = { appId, id, title: TextObject, showIcon?, close?: ButtonElement, submit?: ButtonElement,
                      blocks: LayoutBlock[], type?: 'modal', clearOnClose?, notifyOnClose?, state? }
ContextualBarView = { appId, id, title: TextObject, close?, submit?, blocks, type?: 'contextualBar' }
BannerView        = { appId, viewId, inline?, variant?: neutral|info|success|warning|danger, icon?,
                      title?: string|TextObject, blocks }
```

(`UK:surfaces/modal/ModalView.ts`, `contextualBar/ContextualBarView.ts`, `banner/BannerView.ts`;
`type`, `clearOnClose`, `notifyOnClose` and `state` exist only in `AE:uikit/IUIKitSurface.ts`, the
web ignores them; apps-engine views always carry `type` and `showIcon: true`, contextual bars
included.) Keep the **raw JSON** of every open view next to the parsed form: submit/close echo the
view back, and re-serialising parsed types would drop unknown fields.

### 5.2 State (values) collection

Web: `WEB:uikit/hooks/useUiKitView.ts`, `FUK:utils/extractInitialStateFromLayout.ts`,
`FUK:hooks/useUiKitState.ts`.

- `values: Map<actionId, {blockId, value}>`. Initialised once per opened view from the
  top-level `input.element` and `actions|context.elements` of the view (initial value per
  §2.4; section accessories are **not** included). Details of
  `extractInitialStateFromLayout`: the entry's `blockId` is always the **block's** `blockId` (an
  element's own `blockId` is ignored here, while later changes use `element.blockId ??
  block.blockId`), and `conditional` blocks are not descended into, so elements inside a matching
  `conditional` get no initial value. Rustket resolves conditionals first (§2.6) and uses the
  inherited `blockId` for both.
- `state` for submit/close = values grouped `{[blockId]: {[actionId]: value}}`; entries without
  a `blockId` are skipped; user changes without a `blockId` are stored under `'default'`.
  Rustket uses `'default'` for both cases.
- Duplicate `actionId`s share one value (keyed by actionId) — keep, it is what apps expect.
- On `modal.update` / `contextual_bar.update` the view is shallow-merged over the old one
  (`{...view, ...newView}`, `WEB:uikit/hooks/useUiKitView.ts`, so a top-level key missing from the
  update, e.g. `submit`, survives) and `values` are kept; the web does not add initial values of
  new elements, so an untouched new field is missing from `state`. Rustket replaces the raw view
  as received, keeps existing values and adds initial values for action ids it does not have
  yet; it drops nothing.
- When an element changes (by context, `useUiKitState`):
  - `plain_text_input` with `dispatchActionConfig` ∋ `on_character_entered`: store the value and
    emit `blockAction`, debounced **700 ms** in modals and contextual bars; no loading state.
  - element in a section accessory, a callout accessory (SECTION context), an actions block or
    an `info_card` row action (ACTION context): store and emit `blockAction` immediately (with
    loading state).
  - element with `dispatchActionConfig` ∋ `on_item_selected` in any other context (input
    block, context block, tab): store and emit (with loading state).
  - anything else (input-block elements, context-block items, tabs): only store (no request).
    Rustket deviates only for tabs, which it emits (§2.4).
  - message blocks have no view state (`values: {}`, no `updateState` in
    `WEB:uikit/hooks/useMessageBlockContextValue.ts`): section-accessory and actions elements emit
    on change, `on_character_entered` inputs emit on every change **without** debounce, and an
    input-block element without `dispatchActionConfig` only changes its local widget value and
    sends nothing.
  - banners: `updateState` is a no-op (`WEB:uikit/hooks/useBannerContextValue.ts`), so banner input
    values are never stored; `state` sent on close holds only the initial values.
- Values per element type as in §2.4. Checkbox/toggle state is the array of selected values;
  the emitted block action carries the toggled value (web compatibility). The web's view `values`
  hold only the **last toggled option** for checkboxes/toggles in every context: `actionFunction`
  (actions/section) passes the toggled value to `updateState`, `stateFunction` (input blocks) does
  the same, so an edited checkbox is submitted as a string, an untouched one as the initial array.
  Rustket keeps and submits the full array.

### 5.3 Submit, close and payload size

- Submit button (`view.submit`, primary unless `style:'danger'`) → `viewSubmit`. Rustket rule:
  Enter in a single-line field submits when the view has a submit button; Enter in a multiline
  field inserts a newline. (`WEB:views/modal/uikit/ModalBlock.tsx` has an Enter/Escape/Tab
  keydown handler meant to do this, but it only runs when `e.target` is `#modal-root` **and**
  inside `.rcx-modal__content`, which cannot both hold, so in practice the web relies on native
  implicit form submission, which needs a single text input in the form; focus trapping comes
  from `FocusScope contain`. UNVERIFIED: this assumes fuselage `Button` renders
  `type="button"`; if UIKit buttons inside the form were submit buttons, Enter would instead
  "click" the first one. fuselage is not in `.ref`.) Double submit is blocked while a submit is in flight (RN
  `submitting`; the web has no such guard).
- Footer close button (`view.close`, danger when `style:'danger'`) → `viewClosed`
  `isCleared:false`; X and backdrop click (click whose target is `#modal-root`) →
  `isCleared:true`. Rustket also maps Escape to `isCleared:true`; UNVERIFIED whether the web closes
  on Escape at all (its own handler never runs, see above).
- Payload: `{...rawView, state}` like the web. If the serialised body exceeds 90 KB, send the
  RN form `{id, appId, state}` instead (RN submits only `{id, state}`,
  `RN:app/views/ModalBlockView.tsx`), so a large view never hits the 413.
- Contextual bar: the web closes the panel **before** sending (so errors/updates cannot be
  shown). Rustket keeps it open until the answer and applies §4.6, like modals.

### 5.4 Validation errors

`errors` maps `actionId → message`. Show the message under the input block whose element has
that action id (web `FieldError`, `FUK:blocks/InputBlock.tsx`) and give the element an error
style (selects, inputs, linear scale buttons turn danger). On new errors move focus to the
first field with an error, otherwise to the first focusable field (`ModalBlock.tsx`; the web
looks for the first `:invalid` element, i.e. native constraint validation, not the UIKit error).
The web keeps errors across edits and across `modal.update` (only a new `errors` interaction or
a new modal instance resets them); Rustket clears them on `modal.update` /
`contextual_bar.update` (an updated view is usually the app's answer to the fix). The web ignores
errors in contextual bars (`useContextualBarContextValue` passes none); Rustket shows them.

### 5.5 Modal UI (web, worth copying)

`WEB:views/modal/uikit/ModalBlock.tsx`: header = app icon `GET /api/apps/{appId}/icon` (public;
image bytes with the content type from the app's data URL, may be SVG → show initials of the app
name instead, as Rustket never decodes SVG) when `showIcon`, title (text object), X button;
scrollable content; footer with close and submit buttons. Focus trapped inside, restored on
close. Busy pill while requests run.

### 5.6 Contextual bar UI

`WEB:views/room/contextualBar/uikit/UiKitContextualBar.tsx`, `WEB:views/room/hooks/useAppsContextualBar.ts`:
opens as the room's right panel (route `tab=app`, `context=<viewId>`, i.e. in whatever room is
currently routed), header = app avatar (`/api/apps/{appId}/icon`, always, no `showIcon` check),
title, close X; scrollable blocks; footer with close/submit buttons stretched. `tab_navigation`
only here. Interactions from it include `rid`. Opening another room keeps the view registered; Rustket closes app panels when their room view is
closed and sends nothing (the web sends nothing either).

### 5.7 Banners

UIKit banners (`IBanner`, `RC:packages/core-typings/src/IBanner.ts`) come from
`GET /api/v1/banners?platform=web` (`platform` is required; verified `{"banners":[]}`) →
`{banners: IBanner[]}`, each with `surface: 'banner' | 'modal'`, `view` (a `BannerView`, or a modal
view when `surface: 'modal'`) and an optional `dictionary` `{lang: {key: text}}`
(`WEB:views/cloud/CloudAnnouncementsRegion.tsx`, `CloudAnnouncementHandler.tsx`). The web refetches
on `stream-notify-logged` `banner-changed` (`[{bannerId}]`), on `stream-notify-user`
`{uid}/banners` and every 24 h; Rustket already subscribes both events. A view without `appId`
gets `appId: 'cloud-announcements-core'`; the web adds `dictionary` to the
`app-cloud-announcements-core` i18n namespace (note that `-core` apps resolve keys in the core
namespace unless a text object names `ns` explicitly, §2.1). `banner.open` / `banner.update` /
`banner.close` interactions also drive banners.

Render with the banner surface. The web shows **one banner at a time** (the first in its queue,
`WEB:lib/banners.ts`, `WEB:views/banners/BannerRegion.tsx`); block actions emit and then close the
banner; the close X sends `viewClosed` (`{...view, id: view.viewId, state}`, `isCleared: true`) to
the view's `appId`: `banner-core` dismisses `view.viewId` for the user and answers `banner.close`;
`cloud-announcements-core` dismisses the banner, forwards the interaction to Rocket.Chat Cloud and
answers `banner.close` or (for `surface: 'modal'`) `modal.close`, both with `viewId`
(`SRV:server/modules/core-apps/banner.module.ts`, `cloudAnnouncements.module.ts`). If `title` is a
text object the web renders **nothing for the whole banner** (`UiKitBanner.tsx` returns `null`);
Rustket renders it. Legacy per-user banners (`user.banners`, `POST /api/v1/banners.dismiss`,
`WEB:views/banners/hooks/useUserBanners.ts`) are plain title/text banners, not UIKit.

## 6. App action buttons

### 6.1 Endpoint

`GET /api/apps/actionButtons` (no auth required) → bare JSON array (verified `[]`;
`API.v1.success(array)` assigns `success` as a non-index property of the array, which JSON
serialisation drops, `SRV:server/api/ApiClass.ts`):

```ts
IUIActionButton = { appId, actionId, context: 'messageAction'|'roomAction'|'messageBoxAction'|'userDropdownAction'|'roomSideBarAction',
                    labelI18n: string, variant?: 'danger', category?: 'default'|'ai',
                    when?: { roomTypes?: RoomTypeFilter[], messageActionContext?: ('message'|'message-mobile'|'threads'|'starred')[],
                             hasOnePermission?: string[], hasAllPermissions?: string[], hasOneRole?: string[], hasAllRoles?: string[] } }
```

(`AE:ui/IUIActionButtonDescriptor.ts`, `AE:ui/UIActionButtonContext.ts`.) Only buttons of
enabled apps with the `ui.registerButtons` permission are listed
(`RC:packages/apps/src/server/managers/UIActionButtonManager.ts`). `roomSideBarAction` is not used
by the 8.9 web client. Fetch after login, after every reconnect (the web's query key includes the
connection status) and whenever `stream-apps` reports `actions/changed` (debounce 100 ms,
`WEB:hooks/useAppActionButtons.ts`).

### 6.2 Filters

`WEB:hooks/useApplyButtonFilters.ts` (same logic in `RN:app/lib/apps/filters.ts`):

- `roomTypes` (any match): `public_channel` t=c, `private_channel` t=p, `public_team`/`private_team`
  = `teamMain` and t=c/p, `public_discussion`/`private_discussion` = `prid` and t=c/p, `direct` t=d,
  `direct_multiple` t=d and `uids.length > 2`, `livechat` t=l.
- `hasAllPermissions` / `hasOnePermission`: global permission checks.
- `hasAllRoles` / `hasOneRole`: roles of the user, scoped to the room (subscription roles count)
  where there is a room; names may be role names or ids.
- category: the room header kebab, message menu and composer menu show `category` absent or
  `'default'`; `'ai'` room/message buttons go to a separate "AI actions" menu with a stars icon
  (room header featured item `WEB:hooks/roomActions/useAppsRoomStarActions.tsx`; message toolbar
  `WEB:components/message/toolbar/MessageToolbarStarsActionMenu.tsx`); `'ai'` composer buttons are
  not shown anywhere. The user menu applies only the permission/role filter
  (`useApplyButtonAuthFilter`, no room, no room-type and no category filter), so it lists `'ai'`
  buttons too (`WEB:hooks/useUserDropdownAppsActionButtons.ts`).
- `messageActionContext` (message buttons only): default = all four; Rustket's main timeline is
  `message`, thread panel `threads`, starred list `starred`. The web's message lists use more
  contexts (`pinned`, `mentions`, `search`, `direct`, `videoconf`, …,
  `WEB:lib/MessageAction.ts`); none of them is in the apps-engine list, so app message buttons never
  show there (`WEB:components/message/toolbar/useMessageActionAppsActionButtons.ts`). Rustket
  shows none in pinned/mentions/search lists either.

### 6.3 Placement (web) and interaction

| Context | Web placement | Interaction |
|---|---|---|
| `roomAction` | room header toolbox, kebab menu section "Apps" (`WEB:views/room/providers/hooks/useAppsRoomActions.ts`) | `{rid, payload:{context}}` |
| `messageAction` | message toolbar kebab menu section "Apps"; for E2E messages the section shows a disabled "Unavailable" item (`MessageToolbarActionMenu.tsx`) | `{rid, tmid, mid, payload:{context}}` |
| `messageBoxAction` | composer "+" actions menu, "Apps" group (`WEB:views/room/composer/messageBox/MessageBoxActionsToolbar/MessageBoxActionsToolbar.tsx`, `WEB:hooks/useMessageboxAppsActionButtons.ts`) | `{rid, tmid, payload:{context, message: <composer text or "">}}` (the server drops an empty `message` before calling the app) |
| `userDropdownAction` | user avatar menu (`WEB:navbar/NavBarSettingsToolbar/UserMenu/hooks/useUserMenu.tsx`), role filter without room | `{payload:{context}}` |

`variant:'danger'` renders the item in danger colour in the room header and message menus; the
composer and user menus drop it (`useMessageboxAppsActionButtons`,
`useUserDropdownAppsActionButtons` do not copy `variant`). Rustket applies it everywhere. No icons
(apps cannot set one).
Label: `t("app-{appId}.{labelI18n}")` with i18next `nsSeparator: '.'`
(`RC:apps/meteor/ee/lib/misc/Utilities.ts` `getI18nKeyForApp`, `WEB:providers/TranslationProvider.tsx`)
= key `labelI18n` in the app's language bundle (§6.4). Rustket falls back to the raw `labelI18n`.
UNVERIFIED what the web shows for a missing key (`app-{appId}.{labelI18n}` or `labelI18n`):
`keySeparator` is also the default `.`, and i18next then splits off the namespace only when it is
a known namespace.

### 6.4 App translations

`GET /api/apps/languages` (no auth) → `{apps:[{id, languages:{"en":{"key":"text"}, "pt-BR":{…}}}]}`
(verified empty); per app `GET /api/apps/{id}/languages` → `{languages:{…}}`. Refetch on
`app/added` (`WEB:hooks/useTranslationsForApps.ts`). Resolution: user language (normalised,
e.g. `pt-br` → `pt-BR`), then its base (`pt`), then `en`, then the fallback text/key.

### 6.5 `stream-apps`

Subscribe `stream-apps` with event `apps` (server-only stream, readable by all,
`RC:packages/streamer/src/notifications.module.ts`). Each change is emitted twice: under its own
event name and wrapped under `apps` (`RC:packages/streamer/src/listeners.module.ts`):

```json
{"msg":"changed","collection":"stream-apps","id":"id",
 "fields":{"eventName":"apps","args":[["app/statusUpdate",[{"appId":"…","status":"manually_enabled"}]]]}}
```

Keys: `app/added [appId]`, `app/removed [appId]`, `app/updated [appId]`,
`app/statusUpdate [{appId,status}]`, `app/settingUpdated [{appId,setting}]`,
`command/added|disabled|updated|removed [command]`, `actions/changed []`. Reactions:
`actions/changed` → refetch action buttons; `command/*` → refetch `commands.list`;
`app/added` → refetch languages; any app change → refresh the admin list if open.

## 7. App slash commands

App commands live in `commands.list` with `appId`, and their `description`/`params` are app i18n
keys `app-{appId}.{key}` (`SRV:app/apps/server/bridges/commands.ts` `registerCommand`).
`commands.list` answers `202 {appsLoaded:false, commands:[]}` while apps load; the web retries
with backoff until `appsLoaded`, pages with `count=50`, and on `command/removed|disabled|updated`
drops the command locally before refetching (`WEB:hooks/useAppSlashCommands.ts`). Execution
(`WEB:lib/chats/flows/processSlashCommand.ts`):

1. `triggerId = registry.issue(appId)` (for built-in commands too; harmless, but register them so
   that pushes are ignored, like the web, §4.2).
2. Busy pill on for app commands; clear the composer.
3. `POST /api/v1/commands.run {command, params, roomId, tmid?, triggerId}`
   (`RC:packages/rest-typings/src/v1/commands.ts`, `SRV:server/api/v1/commands.ts`).
4. The app may push `modal.open`/`contextual_bar.open` with that trigger over
   `{uid}/uiInteraction`; §4.6 accepts it.
5. On error: local ephemeral from `rocket.cat`, i18n key
   `Something_went_wrong_while_executing_command` ("Something went wrong while executing command:
   `/{{command}}`"); the web shows the same kind of local ephemeral for an unknown command
   (`No_such_command`, unless `Message_AllowUnrecognizedSlashCommand`) and for a missing
   `permission`.

Previews: `GET /api/v1/commands.preview?command&params&roomId` → `{preview:{i18nTitle, items:[{id,type,value}]}}`
or `{preview: null}`; an unknown command answers 400 "The command provided does not exist (or is
disabled).", a command without previews 400 `error-invalid-command` "Command Does Not Provide
Previews" (both verified) — call it only for commands with `providesPreview: true`;
selecting an item `POST /api/v1/commands.preview {command, params, roomId, previewItem:{id,type,value}, triggerId, tmid?}`.
The composer side (autocomplete, permission check `permission`, `Message_AllowUnrecognizedSlashCommand`)
belongs to the composer spec.

## 8. Apps administration

All under `/api/apps/` (`SRV:ee/server/apps/communication/rest.ts`, `…/endpoints/*.ts`):

| Method / path | Auth | Request | Response |
|---|---|---|---|
| `GET installed?includeClusterStatus=true` | logged in | – | `{apps: IAppInfoRest[]}` (verified empty) |
| `GET :id` | `manage-apps` | – | `{app}` (404 unknown) |
| `POST :id/status` | `manage-apps` | `{status: "manually_enabled" \| "manually_disabled"}` | `{status}`; `failure` with the license message if enabling exceeds the limit (`canEnableApp`) |
| `GET :id/status` | `manage-apps` | – | `{status, clusterStatus?}` |
| `GET :id/settings` | `manage-apps` | – | `{settings: {<id>: ISetting}}` — an **object keyed by id** (typings say array); hidden settings removed |
| `POST :id/settings` | `manage-apps` | `{settings: ISetting[]}` (array; only entries whose `value` changed are applied) | `{updated: ISetting[]}` |
| `GET/POST :id/settings/:settingId` | `manage-apps` | `{setting}` | `{setting}` / `{success}` |
| `GET :id/logs?count&offset&logLevel&method&instanceId&startDate&endDate&sort` | `manage-apps` | `logLevel` `'0'` errors, `'1'` + warn/info/log, `'2'` + debug/success; dates ISO | `{offset, logs: ILogItem[], count, total}` |
| `GET logs?appId&…` | `manage-apps` | same | same (verified empty) |
| `GET :id/logs/distinctValues` | `manage-apps` | – | `{instanceIds, methods}` |
| `GET :id/export-logs?type=json\|csv&…` | `manage-apps` (auth headers, or `rc_uid`/`rc_token` cookies) | same filters; `count` default 100, capped at 2000 | file download (`app-logs-<id>-<timestamp>.json\|csv`) |
| `DELETE :id` | `manage-apps` | – | `{app}` with `status: 'disabled'` (uninstall); on failure `success:false` with `{app}` |
| `GET :id/icon` | public | – | image bytes; 404 when the app has no icon |
| `GET :id/languages`, `GET languages` | public | – | §6.4 |
| `GET :id/apis` | `manage-apps` | – | `{apis}` (app HTTP endpoints) |
| `GET count` | public (verified without auth) | – | verified above |
| `GET marketplace`, `GET categories` | logged in | – | marketplace data (needs internet); both are bare JSON arrays (verified) |
| `GET externalComponents` | public | – | `{externalComponents: []}` (verified) |
| `POST /api/apps` | `manage-apps` | `{appId, marketplace:true, version, permissionsGranted?}` or multipart `app` (+`permissions`) or `{url}` | `{app, implemented, licenseValidation}`; requires cloud registration / private-app license |

`IAppInfoRest` = apps-engine `IAppInfo` (`id, name, nameSlug, version, description, requiredApiVersion,
author{name,homepage,support}, classFile, iconFile, iconFileContent?, implements, essentials?,
permissions?, addon?`) + `status`, `languages`, `private`, `migrated`, `licenseValidation?` (only
when it has errors or warnings), `clusterStatus?`
(`RC:apps/meteor/ee/lib/misc/formatAppInstanceForRest.ts`, `AE:metadata/IAppInfo.ts`).
`AppStatus` enabled = `auto_enabled | manually_enabled`; disabled = `compiler_error_disabled,
error_disabled, manually_disabled, invalid_settings_disabled, invalid_license_disabled,
invalid_installation_disabled, disabled`; also `unknown, constructed, initialized` (`AE:AppStatus.ts`).
`ISetting` = `{id, type: boolean|code|color|font|int|select|string|multiSelect|password|roomPick,
packageValue, value?, required, public, hidden?, values?: [{key, i18nLabel}], multiline?, section?,
i18nLabel, i18nDescription?, i18nAlert?, i18nPlaceholder?}` (`AE:settings/*`); labels resolve in
the app's language bundle. `ILogItem` = `{_id, appId, method, instanceId, startTime, endTime,
totalTime, _createdAt, _updatedAt, entries:[{severity, timestamp, caller, args: string[], method?}]}`
(the `startDate`/`endDate` filters apply to `_updatedAt`, `…/endpoints/lib/makeAppLogsQuery.ts`)
(`RC:packages/core-typings/src/ILogItem.ts`). The web saves settings by posting every setting with
its new value (`WEB:views/marketplace/AppDetailsPage/AppDetailsPage.tsx`) and toggles status with
`manually_enabled`/`manually_disabled` (`WEB:views/marketplace/hooks/useAppMenu.tsx`).

Rustket phase 12 scope: list installed apps (name, version, author, status badge), enable/disable,
settings form generated from `ISetting.type`, logs viewer with level filter and paging, uninstall
with confirmation. Marketplace browsing/installing is out of scope (link to the web UI).

## 9. Streams summary

| Stream / event | Payload | Use |
|---|---|---|
| `stream-notify-user` `{uid}/uiInteraction` | `[ServerInteraction]` | §4.5 (already subscribed) |
| `stream-notify-user` `{uid}/message` | `[ephemeral IMessage]` | ephemeral messages with blocks, upsert by `_id`, keep the ephemeral flag for §3.2 (already subscribed, `SessionEvent::EphemeralMessage`) |
| `stream-apps` `apps` | `[[key, args]]` | §6.5 (new subscription; per-stream sub rate limits in `protocol.md` apply) |
| `stream-notify-room` `{rid}/videoconf` | `[callId]` | refresh `video_conf` blocks (already subscribed per watched room) |
| `stream-notify-user` `{uid}/banners`, `stream-notify-logged` `banner-changed` | `[IBanner]` / `[{bannerId}]` | refetch `GET /api/v1/banners?platform=web` (§5.7; both already subscribed) |

## 10. Rust design

### 10.1 `crates/rocketchat` (protocol)

```
src/uikit/mod.rs          re-exports, `decode_blocks(&Value) -> Vec<LayoutBlock>` with size guard
src/uikit/text.rs         TextObject, TextI18n, StrOrText
src/uikit/blocks.rs       LayoutBlock, Element, UiOption, OptionGroup, Confirm, Actionable, BlockMeta
src/uikit/view.rs         ModalView, ContextualBarView, BannerView, RawView<T>
src/uikit/interaction.rs  UserInteraction (Serialize), ServerInteraction (Deserialize), Container,
                          InteractionResponse, ViewErrors
src/models/apps.rs        ActionButton, ActionButtonWhen, AppInfo, AppStatus, AppSetting, AppLogItem,
                          AppLanguages, AppsStreamEvent
src/rest/apps.rs          impl RestClient { ui_interaction, action_buttons, apps_languages,
                          installed_apps, app_status, set_app_status, app_settings,
                          set_app_settings, app_logs, app_logs_distinct, uninstall_app, app_icon_url }
```

Tolerant decoding: deserialize each block/element to `Value`, read `type`, `from_value` into the
variant, and fall back to `Unknown(Value)` on an unknown type **or** a decode error (same idea as
`models/md.rs`). Missing `text` → `""`, missing arrays → empty.

```rust
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TextObject {
    #[serde(rename = "type", default)]
    pub kind: TextKind,                       // Plain | Mrkdwn | Unknown (serde(other))
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbatim: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub i18n: Option<TextI18n>,               // { key, ns?, args: Map<String, Value> }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockMeta { pub app_id: Option<String>, pub block_id: Option<String> }

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutBlock {
    Section { meta: BlockMeta, text: Option<TextObject>, fields: Vec<TextObject>, accessory: Option<Box<Element>> },
    Divider { meta: BlockMeta },
    Image { meta: BlockMeta, image_url: String, alt_text: String, title: Option<TextObject> },
    Actions { meta: BlockMeta, elements: Vec<Element> },
    Context { meta: BlockMeta, elements: Vec<Element> },          // Element::Text / Element::Image
    Input { meta: BlockMeta, label: TextObject, element: Box<Element>, hint: Option<TextObject>, optional: bool },
    Conditional { meta: BlockMeta, engines: Option<Vec<String>>, render: Vec<LayoutBlock> },
    Preview(Box<PreviewBlock>),
    VideoConf { meta: BlockMeta, call_id: String },
    Callout { meta: BlockMeta, title: Option<TextObject>, text: TextObject, variant: CalloutVariant, accessory: Option<Box<Element>> },
    TabNavigation { meta: BlockMeta, tabs: Vec<Element> },
    InfoCard { meta: BlockMeta, rows: Vec<InfoCardRow> },
    Unknown(serde_json::Value),
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Actionable {
    pub app_id: Option<String>,
    pub block_id: Option<String>,
    #[serde(default)]
    pub action_id: String,
    pub confirm: Option<Confirm>,
    #[serde(default)]
    pub dispatch_action_config: Vec<String>,  // "on_character_entered" | "on_item_selected"
}

#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    Button { a: Actionable, text: TextObject, url: Option<String>, value: Option<String>, style: Option<String>, secondary: bool },
    Overflow { a: Actionable, options: Vec<UiOption> },
    StaticSelect { a: Actionable, placeholder: Option<TextObject>, options: Vec<UiOption>, groups: Vec<OptionGroup>, initial: Option<String> },
    MultiStaticSelect { a: Actionable, placeholder: Option<TextObject>, options: Vec<UiOption>, groups: Vec<OptionGroup>, max: Option<u32>, initial: Vec<String> },
    UsersSelect { a: Actionable, placeholder: Option<TextObject>, multi: bool },
    ChannelsSelect { a: Actionable, placeholder: Option<TextObject>, multi: bool },
    ConversationsSelect { a: Actionable, multi: bool },
    DatePicker { a: Actionable, placeholder: Option<TextObject>, initial: Option<String> },
    TimePicker { a: Actionable, placeholder: Option<TextObject>, initial: Option<String> },
    PlainTextInput { a: Actionable, placeholder: Option<TextObject>, initial: Option<String>, multiline: bool, min_len: Option<u32>, max_len: Option<u32> },
    LinearScale { a: Actionable, min: i64, max: i64, initial: Option<serde_json::Value>, pre: Option<TextObject>, post: Option<TextObject> },
    Checkbox { a: Actionable, options: Vec<UiOption>, initial: Vec<String> },
    ToggleSwitch { a: Actionable, options: Vec<UiOption>, initial: Vec<String> },
    RadioButton { a: Actionable, options: Vec<UiOption>, initial: Option<String> },
    Tab { a: Actionable, title: TextObject, disabled: bool, selected: bool },
    IconButton { a: Actionable, icon: Icon, label: Option<String>, url: Option<String>, value: Option<String> },
    Icon(Icon),                               // { icon, variant, framed }
    Image { image_url: String, alt_text: String },
    Text(TextObject),                         // context / info_card items
    Unknown(serde_json::Value),
}

/// The JSON as received plus the parsed form; submit/close echo `raw`.
#[derive(Debug, Clone)]
pub struct RawView<T> { pub raw: serde_json::Value, pub view: T }

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum UserInteraction {
    #[serde(rename = "blockAction", rename_all = "camelCase")]
    BlockAction { action_id: String, payload: BlockActionPayload, container: Container,
                  #[serde(skip_serializing_if = "Option::is_none")] mid: Option<String>,
                  #[serde(skip_serializing_if = "Option::is_none")] tmid: Option<String>,
                  #[serde(skip_serializing_if = "Option::is_none")] rid: Option<String>,
                  trigger_id: String },
    #[serde(rename = "viewSubmit", rename_all = "camelCase")]
    ViewSubmit { payload: ViewPayload, view_id: String,
                 #[serde(skip_serializing_if = "Option::is_none")] rid: Option<String>, trigger_id: String },
    #[serde(rename = "viewClosed", rename_all = "camelCase")]
    ViewClosed { payload: ViewClosedPayload,
                 #[serde(skip_serializing_if = "Option::is_none")] rid: Option<String>, trigger_id: String },
    #[serde(rename = "actionButton", rename_all = "camelCase")]
    ActionButton { action_id: String, payload: ActionButtonPayload,   // { context, message? }
                   #[serde(skip_serializing_if = "Option::is_none")] mid: Option<String>,
                   #[serde(skip_serializing_if = "Option::is_none")] tmid: Option<String>,
                   #[serde(skip_serializing_if = "Option::is_none")] rid: Option<String>,
                   trigger_id: String },
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockActionPayload { pub block_id: String, #[serde(skip_serializing_if = "Option::is_none")] pub value: Option<serde_json::Value> }
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "lowercase")]  // {"type":"message","id":…}
pub enum Container { Message(String), View(String) }

#[derive(Debug, Clone)]
pub enum ServerInteraction {
    ModalOpen { trigger_id: String, app_id: String, view: RawView<ModalView> },
    ModalUpdate { trigger_id: String, app_id: String, view: RawView<ModalView> },
    ModalClose { trigger_id: String, app_id: String, view_id: Option<String> },
    ContextualBarOpen { trigger_id: String, app_id: String, view: RawView<ContextualBarView> },
    ContextualBarUpdate { trigger_id: String, app_id: String, view: RawView<ContextualBarView> },
    ContextualBarClose { trigger_id: String, app_id: String, view_id: String },
    BannerOpen { trigger_id: String, app_id: String, view: RawView<BannerView> },
    BannerUpdate { trigger_id: String, app_id: String, view: RawView<BannerView> },
    BannerClose { trigger_id: String, app_id: String, view_id: String },
    Errors { trigger_id: String, app_id: String, view_id: String, errors: HashMap<String, String> },
    Unknown(serde_json::Value),
}
pub enum InteractionResponse { Empty, Ack, Interaction(ServerInteraction) }

impl RestClient {
    /// `POST /api/apps/ui.interaction/{app_id}`: any 2xx body is returned (an `errors` answer is
    /// `success:false` with HTTP 200); an empty/whitespace body is `Empty` (§4.6 close decision),
    /// `{}` and bodies without `type` are `Ack`.
    pub async fn ui_interaction(&self, app_id: &str, i: &UserInteraction) -> Result<InteractionResponse>;
    pub async fn action_buttons(&self) -> Result<Vec<ActionButton>>;   // bare array, no envelope
}
```

`app_id` goes into the URL path: percent-encode it and reject `/`, `?`, `#` (it comes from
untrusted blocks). Session: `{uid}/message` already yields `SessionEvent::EphemeralMessage`;
decode `uiInteraction` into `SessionEvent::UiInteraction(Box<ServerInteraction>)` and add
`SessionEvent::Apps(AppsStreamEvent)` for `stream-apps` `apps` (key + args; unknown keys kept).
Never log interaction bodies at info level (they can contain user input).

### 10.2 `crates/rustket-core` (state)

```
src/uikit/mod.rs        UiKitStore (per workspace): triggers, modal, bars, banners, buttons, languages, busy
src/uikit/trigger.rs    TriggerRegistry
src/uikit/view_state.rs ViewState reducer, initial values, state JSON, errors
src/uikit/buttons.rs    action button filtering (room types, permissions, roles, category, contexts)
src/uikit/i18n.rs       app translation lookup + i18next-style interpolation/plurals
src/uikit/trust.rs      `actions_allowed(msg, is_ephemeral, in_video_conf_block, app_id) -> bool` (§3.2;
                        `is_ephemeral` comes from the store, never from message fields)
```

```rust
pub struct TriggerRegistry { pending: HashMap<String, Trigger> }
pub struct Trigger { pub app_id: Option<String> /* None: built-in slash command */, pub origin: Origin, issued: Instant, settled: Option<Instant>, consumed: bool, pub handled: Option<&'static str> }
pub struct Origin { pub rid: Option<String>, pub tmid: Option<String>, pub mid: Option<String>, pub view_id: Option<String>, pub kind: OriginKind }
pub enum OriginKind { MessageBlock, ViewBlock, ViewSubmit, ViewClosed, ActionButton(ButtonContext), SlashCommand }

impl TriggerRegistry {
    pub fn issue(&mut self, app_id: Option<&str>, origin: Origin) -> String;  // random_id()
    pub fn settle(&mut self, trigger_id: &str, now: Instant);                  // HTTP finished
    pub fn accept(&mut self, trigger_id: &str, now: Instant) -> Option<&mut Trigger>; // consumes; None if app_id is None
    pub fn prune(&mut self, now: Instant);    // settled + 5 s, or issued + 5 s + request timeout
}

pub struct OpenView<V> {
    pub app_id: String, pub view: RawView<V>, pub state: ViewState,
    pub errors: HashMap<String, String>, pub submitting: bool, pub rid: Option<String>,
}
pub struct ViewState { values: IndexMap<String /*actionId*/, (String /*blockId*/, serde_json::Value)> }
impl ViewState {
    pub fn from_blocks(blocks: &[LayoutBlock]) -> Self;   // input.element + actions/context elements
    pub fn merge_initials(&mut self, blocks: &[LayoutBlock]); // on update: add missing action ids
    pub fn set(&mut self, action_id: &str, block_id: Option<&str>, value: serde_json::Value);
    pub fn toggle(&mut self, action_id: &str, block_id: Option<&str>, option: &str); // checkbox/toggle
    pub fn get(&self, action_id: &str) -> Option<&serde_json::Value>;
    pub fn to_state_json(&self) -> serde_json::Value;     // {blockId: {actionId: value}}
}
```

`UiKitStore::apply(ServerInteraction, now) -> Vec<UiKitChange>` implements §4.6 without iced;
`UiKitStore::close_decision(trigger, http_ok)` implements the RN rule. Everything is plain data
and unit-testable from JSON fixtures.

### 10.3 `crates/rustket` (iced)

Ownership: a new `src/uikit/` module (a "uikit agent"), plus small additive hooks in components
owned by others:

```
src/uikit/mod.rs         UiKitController: owns nothing persistent (state is in the workspace);
                         turns UiKitMsg into REST tasks, applies responses, emits toasts
src/uikit/render.rs      Renderer { surface, ctx, state: Option<&ViewState>, ui: &UiState } -> Element<UiKitMsg>
src/uikit/elements/*.rs  select (filter popover, chips), users/channels autocomplete, date, time,
                         linear scale, overflow menu, checkbox/radio/toggle lists, preview card, info card
src/uikit/modal.rs       modal dialog (rustket_ui::widget::modal_keyed keyed by view id)
src/uikit/bar.rs         contextual bar body, used by room/panel.rs Kind::App(view_id)
src/uikit/banner.rs      banner strip
locales/uikit.yml        "Show more...", timeout toast, "Unavailable", "Loading", confirm defaults, …
```

Hooks: `room/render/` calls `uikit::render::message_blocks(msg, ctx)` and maps `UiKitMsg` into
`RenderMsg::UiKit(..)`; `room/panel.rs` gains `Kind::App(String)`; the room header kebab, message
menu, composer "+" menu and status menu add app button entries from `UiKitStore::buttons_for(..)`;
`component::Effect` / room `Effect` gain `UiKit(uikit::Request)` handled by the app, which owns
the controller per server. Slash commands from the composer register a trigger through the same
request type.

```rust
#[derive(Debug, Clone)]
pub enum UiKitMsg {
    /// Emit a blockAction (after a confirm dialog when the element has `confirm`).
    Action { surface: SurfaceKey, app_id: String, block_id: String, action_id: String, value: Option<serde_json::Value>, open_url: Option<String> },
    /// Store a value only (input block without dispatch config).
    SetValue { surface: SurfaceKey, action_id: String, block_id: Option<String>, value: serde_json::Value },
    Toggle { surface: SurfaceKey, action_id: String, block_id: Option<String>, option: String, emit: bool },
    TextEdited { surface: SurfaceKey, action_id: String, block_id: Option<String>, value: String, dispatch: bool }, // 700 ms debounce
    Ui(UiEvent),               // open/close dropdown, filter text, autocomplete query, show more, tab select
    Submit(SurfaceKey), Close { surface: SurfaceKey, cleared: bool },
    Confirmed(Box<UiKitMsg>), Cancelled,
}
pub enum SurfaceKey { Message { rid: String, mid: String, tmid: Option<String> }, Modal(String), Bar { rid: String, view_id: String }, Banner(String) }
```

Rendering approach:

- One recursive `Renderer` with the block context (`Block`, `Section`, `Action`, `Form`,
  `Context`) like `UK:rendering/BlockContext.ts`, applying the surface allow-list (§2.5) and
  the element-in-context rules. Elements are pure functions of (`Element`, current value,
  loading flag, error, ui state) → `Element<UiKitMsg>`.
- iced 0.14 widgets: `column`/`row` (`row(..).wrap()` for actions), `grid(..).columns(2)` for
  section fields, `rustket_ui::widget::{primary_button, secondary_button, danger_button,
  icon_button, divider, chip, popover, popover_menu, text_field, tooltip, spinner, modal_keyed}`,
  `checkbox(is_checked)` / `toggler(is_checked)` without built-in labels next to a rich-text
  label (option text can be mrkdwn), radio rows built as buttons (iced `radio` needs `Copy`
  values; use the option index), `text_editor` for multiline inputs (content in `UiState`),
  `image` through `ctx.images` (resolve relative URLs with `ServerUrl::absolute`; auth headers only
  to the own server; SVG → alt text placeholder), `float`/overlay popovers for selects.
- Selects: filterable popover list (web `SelectFiltered`): text field + options + groups;
  multi selects show chips with remove buttons. Users/channels: debounced autocomplete tasks
  (`users.autocomplete` 300/500 ms, `rooms.autocomplete.channelAndPrivate` 300 ms) with avatars.
- Datepicker: text field validated as `YYYY-MM-DD` plus a month-grid calendar popover;
  time picker: `HH:MM` field with hour/minute steppers. Values exactly as §2.4.
- Linear scale: wrap a row of small toggle buttons, cap the number of points at 101.
- Ephemeral UI state (open popover, filter text, autocomplete results, "show more" expanded,
  selected tab, per-action loading) lives in the owning component keyed by
  `(SurfaceKey, actionId)`; durable values live in `ViewState` (views) — messages have none.
- Text: `plain_text` → `text`; `mrkdwn` → md AST through the shared message renderer
  (mentions/channels clickable, emoji); i18n via `uikit::i18n`.
- Modal: header (app icon / initials, title, X), scrollable body, footer buttons, Escape and
  backdrop = close (`isCleared: true`), Enter = submit in single-line fields, focus first field
  or first field with an error (`iced::widget::operation::focus`), busy pill while requests run.

### 10.4 Tests and fixtures

- Fixtures (capture into `crates/rocketchat/tests/fixtures/uikit/`): the ephemeral mention
  message and its dismiss replacement (§3.3), the alice blocks message (block list below), the
  error bodies of §0, a synthetic `modal.open` / `errors` / `contextual_bar.open` built from
  `AE:uikit/UIKitInteractionPayloadFormatter.ts`, the existing
  `crates/rocketchat/tests/fixtures/ddp/synthetic_ui_interaction.json`.
- Unit tests: tolerant decoding (unknown types, missing fields, wrong types), conditional
  resolution, appId/blockId inheritance, initial state extraction, state JSON grouping,
  `merge_initials`, errors normalisation (object and array), trigger accept/consume/expiry,
  close decision table (incl. empty body and a `modal.open` answer that reuses the submitted view
  id), button filters, i18n plural/interpolation, payload size
  fallback, trust rule (forged `private`/`t:'videoconf'` persisted message stays disabled; a
  persisted message reusing an ephemeral `_id` drops the ephemeral flag).
- Live test (`crates/rocketchat/tests/live.rs`): create a channel, mention a non-member with
  admin, wait for the ephemeral blocks on `{uid}/message`, post `dismiss` to `mention-core`,
  expect `{}` and a replacement with the same `_id`; error mapping for an unknown app id;
  `action_buttons()` decodes `[]`; delete the channel.

Block set used for the alice message (one of each renderable kind):

```json
[{"type":"section","appId":"fake-app-id","blockId":"blk_section","text":{"type":"mrkdwn","text":"*Bold* section with `code` and :smile:"},
  "fields":[{"type":"plain_text","text":"Field A"},{"type":"mrkdwn","text":"_Field B_"}],
  "accessory":{"type":"overflow","actionId":"ovf","options":[{"text":{"type":"plain_text","text":"One"},"value":"1"},{"text":{"type":"plain_text","text":"Two"},"value":"2","url":"https://example.com"}]}},
 {"type":"divider","blockId":"blk_div"},
 {"type":"image","blockId":"blk_img","imageUrl":"http://localhost:3000/images/logo/logo.svg","altText":"logo","title":{"type":"plain_text","text":"Image title"}},
 {"type":"context","blockId":"blk_ctx","elements":[{"type":"image","imageUrl":"http://localhost:3000/avatar/admin","altText":"a"},{"type":"mrkdwn","text":"context *md*"},{"type":"plain_text","text":"ctx plain"}]},
 {"type":"actions","appId":"fake-app-id","blockId":"blk_actions","elements":[
   {"type":"button","actionId":"btn_primary","text":{"type":"plain_text","text":"Primary"},"style":"primary","value":"v1",
    "confirm":{"title":{"type":"plain_text","text":"Sure?"},"text":{"type":"plain_text","text":"Really"},"confirm":{"type":"plain_text","text":"Yes"},"deny":{"type":"plain_text","text":"No"},"style":"danger"}},
   {"type":"button","actionId":"btn_url","text":{"type":"plain_text","text":"Link"},"url":"https://rocket.chat"},
   {"type":"static_select","actionId":"sel","placeholder":{"type":"plain_text","text":"Pick"},"options":[{"text":{"type":"plain_text","text":"A"},"value":"a"},{"text":{"type":"plain_text","text":"B"},"value":"b"}],"initialValue":"b"},
   {"type":"multi_static_select","actionId":"msel","placeholder":{"type":"plain_text","text":"Pick many"},"options":[{"text":{"type":"plain_text","text":"A"},"value":"a"},{"text":{"type":"plain_text","text":"B"},"value":"b"}]},
   {"type":"datepicker","actionId":"date","initialDate":"2026-11-08"},
   {"type":"time_picker","actionId":"time","initialTime":"13:45"},
   {"type":"linear_scale","actionId":"scale","minValue":1,"maxValue":5,"initialValue":3,"preLabel":{"type":"plain_text","text":"Low"},"postLabel":{"type":"plain_text","text":"High"}},
   {"type":"checkbox","actionId":"chk","options":[{"text":{"type":"plain_text","text":"C1"},"value":"c1"},{"text":{"type":"mrkdwn","text":"*C2*"},"value":"c2"}],"initialOptions":[{"text":{"type":"plain_text","text":"C1"},"value":"c1"}]},
   {"type":"radio_button","actionId":"rad","options":[{"text":{"type":"plain_text","text":"R1"},"value":"r1"},{"text":{"type":"plain_text","text":"R2"},"value":"r2"}]},
   {"type":"toggle_switch","actionId":"tgl","options":[{"text":{"type":"plain_text","text":"T1"},"value":"t1"}]}]},
 {"type":"callout","blockId":"blk_callout","variant":"warning","title":{"type":"plain_text","text":"Heads up"},"text":{"type":"mrkdwn","text":"callout *text*"}},
 {"type":"preview","blockId":"blk_preview","title":[{"type":"plain_text","text":"Preview title"}],"description":[{"type":"plain_text","text":"desc"}],
  "thumb":{"url":"http://localhost:3000/avatar/admin"},"footer":{"type":"context","elements":[{"type":"plain_text","text":"footer"}]}},
 {"type":"conditional","blockId":"blk_cond","when":{"engine":["livechat"]},"render":[{"type":"section","text":{"type":"plain_text","text":"livechat only"}}]},
 {"type":"conditional","blockId":"blk_cond2","when":{"engine":["rocket.chat"]},"render":[{"type":"section","text":{"type":"plain_text","text":"rocket.chat only"}}]},
 {"type":"info_card","blockId":"blk_info","rows":[{"background":"default","elements":[{"type":"icon","icon":"phone","variant":"default","framed":true},{"type":"plain_text","text":"Call ended"}],
   "action":{"type":"icon_button","actionId":"ib","icon":{"type":"icon","icon":"info","variant":"default"},"label":"Info"}}]},
 {"type":"input","blockId":"blk_input","label":{"type":"plain_text","text":"Input in a message"},"element":{"type":"plain_text_input","actionId":"pti"}},
 {"type":"unknown_future_block","blockId":"blk_unknown"}]
```

A block action on this message to `fake-app-id` returned 500 "Cannot read properties of undefined"
(verified), so the UI must show an error toast and re-enable the element. The `toggle_switch`
without `initialOptions` in this set makes the web renderer throw (§2.4); Rustket must render it.

## 11. Edge cases

1. `blocks: []` → render the markdown body (only a non-empty array hides it).
2. Message blocks without any resolvable `appId` → interactive elements disabled.
3. Core-app `appId` in a persisted message → actions disabled even if it says `private: true` or
   `t: 'videoconf'` (client-settable, §3.2); only `video_conf` block actions stay enabled, and
   `url` buttons still open their URL without emitting (livechat "Join call", §3.2).
4. Ephemeral messages: upsert by `_id`; they vanish on reload (the web behaves the same); with
   `tmid` they go to that thread; a later non-ephemeral message with the same `_id` (ids are
   client-chosen) replaces it and loses the ephemeral trust flag.
5. Pushed interaction with an unknown, expired or already used `triggerId` → ignore silently.
6. Two interactions for one trigger (push then HTTP body) → only the first is applied.
7. `modal.open` while a modal is open → replace it; the old one gets no `viewClosed`.
8. `modal.update` / `errors` for a view id that is not open → ignore.
9. `contextual_bar.open` without a room context (user menu button, slash command in a closed
   room) → open in the active room, else show as a dialog.
10. DDP disconnected while emitting: the HTTP request still works, but pushed views cannot
    arrive; show the connection banner state and a hint if nothing came back.
11. HTTP 413 (big view) → fall back to the compact submit payload (§5.3); 429 → toast with retry time.
12. Timeout after 5 s → toast; a timed-out `viewSubmit` closes its view (web behaviour: only
    interactions with a top-level `viewId` dispose the view); a late answer is ignored.
13. Unknown block/element types → skip; never fail the message or the view.
14. Surface allow-lists (§2.5): skip blocks a surface does not render.
15. `users_select` values are usernames, `channels_select` values are room ids.
16. Checkbox/toggle: state array vs. toggled value in block actions (§2.4).
17. `initialValue: 0` / `""` kept (web drops them).
18. Duplicate `actionId` → shared value.
19. Missing `blockId` → `'default'` in state.
20. `url` buttons/overflow options: open (http/https/mailto only) **and** emit the action.
21. `linear_scale` with `max < min` → one point (web), with huge ranges → cap at 101 points.
22. More than 5 action elements → "Show more..." (not in banners).
23. E2E messages: app message actions show a disabled "Unavailable" item.
24. App icons may be SVG → initials.
25. Image URLs may be external: fetch without credentials; relative URLs resolve against the server.
26. `stream-apps` events arrive twice (own event name and `apps`); subscribe only to `apps`.
27. `commands.list` 202 `appsLoaded:false` → retry with backoff.
28. App settings GET returns an object keyed by id; POST wants an array.
29. Banner title as plain string or text object (the web hides the whole banner for a text object).
30. Interaction bodies may contain user input: no info-level logging.
31. `toggle_switch` / `checkbox` without `initialOptions` → empty selection (the web crashes / misbehaves).
32. Empty 200 body to `viewSubmit` (app handler returned nothing) → keep the view open unless a
    push for the trigger already decided (RN).

## 12. Implementation order

1. Protocol types and tolerant decoding with fixtures; `ui_interaction`, `action_buttons`,
   languages; typed `SessionEvent::UiInteraction`; `stream-apps` subscription.
2. Core: trigger registry, view state, server-interaction reducer, button filters, i18n, trust rule
   (including the per-message ephemeral flag in the timeline store, §3.2).
3. Read-only message rendering of all blocks (needs the client-side markdown parser for `mrkdwn`).
4. Message block actions (buttons, overflow, selects, date/time, scale, checkbox/radio/toggle).
5. Modals (state, submit/close, errors, focus, busy, timeout).
6. Contextual bar panel kind; action buttons in room header, message menu, composer, user menu;
   slash-command triggers.
7. Banner surface fed by `GET /api/v1/banners?platform=web` (§5.7); `video_conf` blocks wired to phase 9.
8. Phase 12 admin screens (installed apps, status, settings, logs).
