//! Views of the settings editor: the group list with search, a group page
//! with tabs and (collapsible) sections, the save bar, and the dialogs.

use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::admin::settings_editor::GroupPage;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_BODY, TEXT_SMALL,
};
use rustket_ui::widget::{
    BannerKind, above, banner, dialog, ghost_button, loading_placeholder, primary_button,
    secondary_button,
};

use super::super::common::{back_row, input, muted, search_bar, strip_tags, title_row};
use super::super::{Message, V};
use super::{Dialog, GroupView, Msg, State, widgets};

pub fn list_view<'a>(s: &'a State, v: &V<'a>) -> Element<'a, Message> {
    let head = title_row(t!("admin.section.settings").into_owned(), vec![]);
    // The error has a slot of its own, so the search field keeps its state.
    let error = s
        .error
        .as_ref()
        .map(|e| banner(BannerKind::Error, e.clone(), None));
    if !s.loaded {
        let col = column![].push(container(loading_placeholder(None)).height(240));
        return column![head, above(error, SPACE_4, col)]
            .spacing(SPACE_4)
            .into();
    }
    let rest = list_body(s, v);
    column![head, above(error, SPACE_4, rest)]
        .spacing(SPACE_4)
        .into()
}

fn list_body<'a>(s: &'a State, v: &V<'a>) -> Element<'a, Message> {
    let m = Message::Settings;
    let mut col = column![].spacing(SPACE_4);
    col = col.push(search_bar(
        &s.search,
        t!("admin.settings.search").into_owned(),
        move |x| m(Msg::Search(x)),
        Message::Noop,
    ));
    if s.edit_ctx.is_read_only() {
        col = col.push(banner(
            BannerKind::Info,
            t!("admin.settings.read_only").into_owned(),
            None,
        ));
    }
    let i18n = v.i18n;
    let label = |id: &str, key: Option<&str>| {
        key.and_then(|k| i18n.t(k))
            .or_else(|| i18n.t(id))
            .unwrap_or(key.unwrap_or(id))
            .to_owned()
    };
    let matching: Vec<String> = s.index.search(&s.store, &s.search, |d| {
        vec![
            i18n.label_for(d),
            i18n.t(&d.id).unwrap_or_default().to_owned(),
        ]
    });
    let groups: Vec<_> = s
        .index
        .groups(|g| label(&g.id, g.i18n_label.as_deref()))
        .into_iter()
        .filter(|g| matching.contains(&g.id))
        .collect();
    if groups.is_empty() {
        return col.push(muted(t!("admin.settings.no_match"))).into();
    }
    let mut grid = column![].spacing(SPACE_3);
    for chunk in groups.chunks(2) {
        let mut r = row![].spacing(SPACE_3);
        for g in chunk {
            let desc = g
                .i18n_description
                .as_deref()
                .and_then(|k| i18n.t(k))
                .map(strip_tags)
                .unwrap_or_default();
            let desc = ellipsize(&desc, 140);
            r = r.push(
                button(
                    column![
                        text(label(&g.id, g.i18n_label.as_deref()))
                            .size(TEXT_BODY)
                            .font(FONT_SEMIBOLD),
                        text(desc).size(TEXT_SMALL).style(style::text_secondary),
                    ]
                    .spacing(SPACE_1),
                )
                .padding(SPACE_3)
                .width(Length::FillPortion(1))
                .style(style::button_secondary)
                .on_press(m(Msg::OpenGroup(g.id.clone()))),
            );
        }
        if chunk.len() < 2 {
            r = r.push(container(text("")).width(Length::FillPortion(1)));
        }
        grid = grid.push(r);
    }
    col.push(grid).into()
}

/// Cuts `s` to at most `max` characters at a word boundary, adding "…".
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max).collect();
    let cut = match cut.rfind(char::is_whitespace) {
        Some(i) if i > max / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end_matches([' ', ',', '.', ';', ':']))
}

pub fn page_view<'a>(s: &'a State, g: &'a GroupView, v: &V<'a>) -> Element<'a, Message> {
    let m = Message::Settings;
    let i18n = v.i18n;
    let group_def = s.store.get(&g.page.group);
    let title = group_def
        .map(|d| i18n.label_for(d))
        .unwrap_or_else(|| g.page.group.clone());
    let clean = !g.session.is_dirty(&s.store);
    let ldap_on = s.store.value("LDAP_Enable").and_then(|v| v.as_bool()) == Some(true);
    let mut buttons: Vec<Element<'a, Message>> = Vec::new();
    let enabled = |on: bool, msg: Msg| (on && !g.busy).then_some(m(msg));
    let perm = |p: &str| rustket_core::permissions::has_permission(v.ctx.workspace, p, None);
    let can_test = perm("test-admin-options");
    let can_oauth = perm("add-oauth-service");
    match g.page.group.as_str() {
        "LDAP" => {
            let ok = ldap_on && clean && can_test;
            buttons.push(
                secondary_button(t!("admin.settings.ldap_test"), enabled(ok, Msg::LdapTest)).into(),
            );
            buttons.push(
                secondary_button(
                    t!("admin.settings.ldap_search"),
                    enabled(ok, Msg::LdapSearch),
                )
                .into(),
            );
            buttons.push(
                secondary_button(
                    t!("admin.settings.ldap_sync"),
                    enabled(
                        ldap_on && clean && perm("sync-auth-services-users"),
                        Msg::LdapSync,
                    ),
                )
                .into(),
            );
        }
        "OAuth" => {
            let can = can_oauth;
            buttons.push(
                secondary_button(
                    t!("admin.settings.oauth_refresh"),
                    enabled(can, Msg::RefreshOAuth),
                )
                .icon(Icon::RefreshCw)
                .into(),
            );
            buttons.push(
                secondary_button(t!("admin.settings.oauth_add"), enabled(can, Msg::AddOAuth))
                    .icon(Icon::Plus)
                    .into(),
            );
        }
        "SAML" => {
            buttons.push(
                secondary_button(
                    t!("admin.settings.saml_metadata"),
                    enabled(can_test, Msg::Saml),
                )
                .into(),
            );
        }
        _ => {}
    }
    let mut col = column![back_row(title, m(Msg::Back), buttons)].spacing(SPACE_4);
    if let Some(desc) = group_def
        .and_then(|d| d.i18n_description.as_deref())
        .and_then(|k| i18n.t(k))
    {
        col = col.push(widgets::markdown(desc, TEXT_BODY, true));
    }
    if g.page.group == "Enterprise" {
        col = col.push(banner(
            BannerKind::Info,
            t!("admin.settings.license_hint").into_owned(),
            None,
        ));
    }
    let conflicts = g.session.remote_conflicts(&s.store);
    // The conflict notice comes with remote edits while the admin types: in
    // a slot, so the fields below keep their state (focus, editors).
    let notice = (!conflicts.is_empty()).then(|| {
        banner(
            BannerKind::Warning,
            t!("admin.settings.conflicts", n = conflicts.len()).into_owned(),
            None,
        )
    });
    let top = col;
    let mut col = column![].spacing(SPACE_4);
    if g.page.has_tabs() {
        let mut tabs = row![].spacing(SPACE_2);
        for (i, tab) in g.page.tabs.iter().enumerate() {
            let label = if tab.id.is_empty() {
                i18n.t_or_key(&g.page.group).to_owned()
            } else {
                i18n.section_label(&tab.id).to_owned()
            };
            tabs = tabs.push(
                button(text(label).size(TEXT_SMALL))
                    .padding([6, 12])
                    .style(if i == g.tab {
                        style::button_primary
                    } else {
                        style::button_secondary
                    })
                    .on_press(m(Msg::Tab(i))),
            );
        }
        col = col.push(tabs.wrap());
    }
    let Some(tab) = g.page.tabs.get(g.tab) else {
        col = col.push(muted(t!("admin.settings.empty_group")));
        return top.push(above(notice, SPACE_4, col)).into();
    };
    for (si, section) in tab.sections.iter().enumerate() {
        let collapsible = GroupPage::is_collapsible(tab, section);
        let collapsed = collapsible && g.collapsed.contains(&section.name);
        let changed = g.session.any_changed(&s.store, &section.setting_ids);
        let mut body = column![].spacing(SPACE_4);
        if !collapsed {
            for id in &section.setting_ids {
                if !g.session.is_visible(&s.store, id) {
                    continue;
                }
                if let Some(e) = widgets::setting(s, g, id, v, conflicts.contains(id)) {
                    body = body.push(e);
                }
            }
            let mut foot = row![].spacing(SPACE_2).align_y(Alignment::Center);
            if g.session.section_can_reset(&s.store, &s.edit_ctx, section) {
                foot = foot.push(
                    ghost_button(
                        i18n.t("Reset_section_settings")
                            .map(str::to_owned)
                            .unwrap_or_else(|| t!("admin.settings.restore_defaults").into_owned()),
                        Some(m(Msg::ResetSection(g.tab, si))),
                    )
                    .icon(Icon::RotateCcw),
                );
            }
            if let Some(oauth) = section.name.strip_prefix("Custom OAuth: ") {
                let site = s
                    .store
                    .value("Site_Url")
                    .and_then(|v| v.as_str())
                    .unwrap_or(v.ctx.server.as_str())
                    .trim_end_matches('/')
                    .to_owned();
                let url = format!("{site}/_oauth/{}", oauth.to_lowercase());
                body = body.push(muted(
                    i18n.t_with("Custom_oauth_helper", &[("url", &url)])
                        .map(|h| strip_tags(&h))
                        .unwrap_or(url),
                ));
                foot = foot.push(
                    ghost_button(
                        t!("admin.settings.oauth_remove"),
                        can_oauth.then(|| m(Msg::RemoveOAuth(oauth.to_owned()))),
                    )
                    .icon(Icon::Trash2),
                );
            }
            body = body.push(foot);
        }
        let mut section_col = column![].spacing(SPACE_3);
        if !section.name.is_empty() {
            let mut head = row![
                text(i18n.section_label(&section.name).to_owned())
                    .size(TEXT_BODY)
                    .font(FONT_SEMIBOLD)
                    .width(Length::Fill)
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
            if changed {
                head = head.push(muted(t!("admin.settings.changed")));
            }
            if collapsible {
                head = head.push(icon::sized(
                    if collapsed {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronUp
                    },
                    16.0,
                ));
            }
            let head: Element<'a, Message> = if collapsible {
                button(head)
                    .padding(0)
                    .width(Length::Fill)
                    .style(style::button_bare)
                    .on_press(m(Msg::ToggleSection(section.name.clone())))
                    .into()
            } else {
                head.into()
            };
            section_col = section_col.push(head);
        }
        if !collapsed {
            section_col = section_col.push(body);
        }
        col = col.push(
            container(section_col)
                .padding(SPACE_4)
                .width(Length::Fill)
                .style(style::container_card),
        );
    }
    top.push(above(notice, SPACE_4, col)).into()
}

/// The save bar (shown while the group has changes).
pub fn footer<'a>(s: &'a State, g: &'a GroupView, v: &V<'a>) -> Option<Element<'a, Message>> {
    if s.edit_ctx.is_read_only() || !g.session.is_dirty(&s.store) {
        return None;
    }
    let m = Message::Settings;
    let n = g
        .page
        .setting_ids()
        .filter(|id| g.session.is_changed(&s.store, id))
        .count();
    let invalid: Vec<String> = g
        .invalid_ids()
        .into_iter()
        .map(|id| {
            s.store
                .get(id)
                .map(|d| v.i18n.label_for(d))
                .unwrap_or_else(|| id.to_owned())
        })
        .collect();
    let status: Element<'a, Message> = if invalid.is_empty() {
        text(t!("admin.settings.n_changed", n = n))
            .size(TEXT_SMALL)
            .style(style::text_secondary)
            .width(Length::Fill)
            .into()
    } else {
        text(t!(
            "admin.settings.invalid",
            n = invalid.len(),
            names = invalid.join(", ")
        ))
        .size(TEXT_SMALL)
        .style(style::text_danger)
        .width(Length::Fill)
        .into()
    };
    Some(
        container(
            row![
                status,
                secondary_button(t!("admin.cancel"), Some(m(Msg::Cancel))),
                primary_button(
                    v.i18n
                        .t("Save_changes")
                        .map(str::to_owned)
                        .unwrap_or_else(|| t!("admin.settings.save").into_owned()),
                    (!g.saving).then_some(m(Msg::Save))
                )
                .loading(g.saving),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        )
        .padding([SPACE_3, SPACE_6])
        .width(Length::Fill)
        .into(),
    )
}

pub fn dialog_view<'a>(s: &'a State, v: &V<'a>) -> Option<Element<'a, Message>> {
    let m = Message::Settings;
    let d = s.dialog.as_ref()?;
    let close = m(Msg::CloseDialog);
    let text_dialog = |title: String, help: String, value: &'a str, ok: Msg| {
        dialog(
            title,
            column![
                text(help).size(TEXT_BODY).style(style::text_secondary),
                input(value, Some(move |x| Message::Settings(Msg::DialogText(x))))
                    .on_submit(Message::Settings(ok.clone()))
            ]
            .spacing(SPACE_3),
            vec![
                secondary_button(t!("admin.cancel"), Some(close.clone())).into(),
                primary_button(
                    t!("admin.ok"),
                    (!value.trim().is_empty()).then(|| Message::Settings(ok.clone())),
                )
                .into(),
            ],
            Some(close.clone()),
        )
    };
    let _ = v;
    Some(match d {
        Dialog::Alerts(list) => {
            let mut body = column![
                text(t!("admin.settings.alerts_body"))
                    .size(TEXT_BODY)
                    .style(style::text_secondary)
            ]
            .spacing(SPACE_3);
            for (label, alert) in list {
                body = body.push(
                    column![
                        text(label.clone()).size(TEXT_BODY).font(FONT_SEMIBOLD),
                        banner(BannerKind::Warning, alert.clone(), None),
                    ]
                    .spacing(SPACE_1),
                );
            }
            dialog(
                t!("admin.settings.alerts_title"),
                body,
                vec![
                    secondary_button(t!("admin.cancel"), Some(close.clone())).into(),
                    primary_button(t!("admin.save"), Some(m(Msg::SaveConfirmed))).into(),
                ],
                Some(close),
            )
        }
        Dialog::LdapUser(u) => text_dialog(
            t!("admin.settings.ldap_search").into_owned(),
            t!("admin.settings.ldap_user_help").into_owned(),
            u,
            Msg::LdapSearchNow,
        ),
        Dialog::OAuthName(n) => text_dialog(
            t!("admin.settings.oauth_add").into_owned(),
            t!("admin.settings.oauth_name_help").into_owned(),
            n,
            Msg::AddOAuthNow,
        ),
        Dialog::SamlUrl(u) => text_dialog(
            t!("admin.settings.saml_metadata").into_owned(),
            t!("admin.settings.saml_help").into_owned(),
            u,
            Msg::SamlNow,
        ),
    })
}
