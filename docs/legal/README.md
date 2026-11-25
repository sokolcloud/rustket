# Legal pages

The source of the pages published on GitHub Pages (branch `gh-pages` of this
repository); the app links to them from About, the first-launch terms screen and,
on macOS, the Help menu (`crates/rustket/src/legal.rs`). The URLs must never move.

| File | Published at |
|---|---|
| `privacy-policy.md` | https://sokolcloud.github.io/rustket/privacy/ |
| `terms.md` | https://sokolcloud.github.io/rustket/terms/ |
| `support.md` | https://sokolcloud.github.io/rustket/support/ |

The in-app terms screen (`crates/rustket/src/screens/terms.rs`, the `legal.terms_*`
strings) is the short form of `terms.md`. When the meaning of the terms changes, bump
`crate::legal::TERMS_VERSION` so everyone accepts them again, and announce privacy
changes in the release notes, as the privacy policy promises.

To publish a change: update "Last updated", copy the file to `<page>/index.md` on the
`gh-pages` branch under its front matter (`title`, `permalink`), push `gh-pages`, and
check the page once GitHub Pages has rebuilt it. The App Store Connect fields (Privacy
Policy URL, Support URL) point at the same URLs.
