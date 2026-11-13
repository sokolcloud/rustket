//! Files of the room (`channels.files` / `groups.files` / `im.files`) with a
//! type filter, name search and download through a save dialog.

use iced::widget::{column, mouse_area, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::Paginated;
use rust_i18n::t;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, font_medium};
use rustket_ui::widget::{ellipsis, icon_button};

use super::api::{self, FILE_TYPES, FileItem};
use super::common;
use super::debounce;
use super::paged::{LoadState, PageReq, Paged};
use crate::error::Res;
use crate::room::render::{FileRef, ImageRef, RenderMsg, Transfer};
use crate::room::timeline::{Viewer, ViewerMsg};
use crate::room::{Action, UpdateCtx, ViewCtx};

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    Search(u64),
    Type(Option<&'static str>),
    Page(PageReq, Res<Paginated<FileItem>>),
    More,
    Retry,
    /// Downloads, media and the image lightbox (shared with the timeline).
    View(ViewerMsg),
}

#[derive(Default)]
pub struct Files {
    query: String,
    query_seq: u64,
    type_group: Option<&'static str>,
    list: Paged<FileItem>,
    viewer: Viewer,
}

/// `1.5 MB`-style size with translated units.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = [
        "room_panel.size_b",
        "room_panel.size_kb",
        "room_panel.size_mb",
        "room_panel.size_gb",
    ];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let unit = t!(UNITS[u]);
    if u == 0 {
        format!("{bytes} {unit}")
    } else {
        format!("{v:.1} {unit}")
    }
}

fn type_label(t: Option<&str>) -> String {
    match t {
        None => t!("room_panel.all"),
        Some("image") => t!("room_panel.files_images"),
        Some("video") => t!("room_panel.files_videos"),
        Some("audio") => t!("room_panel.files_audio"),
        Some("text") => t!("room_panel.files_texts"),
        _ => t!("room_panel.files_other"),
    }
    .into_owned()
}

fn file_icon(f: &FileItem) -> Icon {
    match f.type_group.as_deref() {
        Some("image") => Icon::Image,
        Some("video") => Icon::Video,
        Some("audio") => Icon::Mic,
        Some("text") => Icon::FileText,
        _ => Icon::File,
    }
}

impl Files {
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.reload(ctx)
    }

    fn reload(&mut self, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let req = self.list.reset();
        self.fetch(req, ctx)
    }

    fn fetch(&self, req: PageReq, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let t = ctx
            .workspace
            .room(ctx.rid)
            .map(|e| e.room_type())
            .unwrap_or_default();
        let (rid, q, tg) = (ctx.rid.to_owned(), self.query.clone(), self.type_group);
        Action::task(ctx.rest_task(
            move |rest| api::files(rest, t, rid, tg, q, req.offset),
            move |r| Msg::Page(req, r),
        ))
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Query(q) => {
                self.query = q;
                self.query_seq += 1;
                Action::task(debounce(Msg::Search(self.query_seq)))
            }
            Msg::Search(seq) if seq == self.query_seq => self.reload(ctx),
            Msg::Search(_) => Action::none(),
            Msg::Type(t) => {
                self.type_group = t;
                self.reload(ctx)
            }
            Msg::Page(req, r) => {
                let r = r.map(|mut p| {
                    crate::e2e::files::process_file_items(
                        ctx.workspace,
                        ctx.server,
                        ctx.rid,
                        &mut p.items,
                    );
                    p
                });
                self.list.finish(req, r);
                Action::none()
            }
            Msg::More => match self.list.begin_more() {
                Some(req) => self.fetch(req, ctx),
                None => Action::none(),
            },
            Msg::Retry => self.reload(ctx),
            Msg::View(m) => self.viewer.update(m, ctx).map(Msg::View),
        }
    }

    /// The image lightbox, if open, and the message that closes it.
    pub fn overlay<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<(Element<'a, Msg>, Msg)> {
        self.viewer
            .overlay(ctx)
            .map(|e| (e.map(Msg::View), Msg::View(ViewerMsg::CloseImage)))
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let top = column![
            common::search_field(
                &self.query,
                t!("room_panel.search_files").into_owned(),
                Msg::Query
            ),
            common::filter_pills(
                &FILE_TYPES.map(|t| (t, type_label(t))),
                self.type_group,
                Msg::Type,
            ),
        ]
        .spacing(SPACE_2)
        .padding([SPACE_3, SPACE_4]);
        let body: Element<'a, Msg> = match &self.list.state {
            LoadState::Loading => common::loading(),
            LoadState::Failed(e) if self.list.items.is_empty() => {
                common::error_state(e, Msg::Retry)
            }
            _ if self.list.loaded && self.list.items.is_empty() => {
                common::empty(Icon::Paperclip, t!("room_panel.no_files").into_owned())
            }
            _ => {
                let mut col = column![].spacing(0);
                for f in &self.list.items {
                    let mut meta = vec![human_size(f.size)];
                    if let Some(u) = f.user.as_ref().and_then(|u| u.username.clone()) {
                        meta.push(format!("@{u}"));
                    }
                    if f.uploaded_at.is_some() {
                        meta.push(common::time_label(f.uploaded_at, ctx));
                    }
                    let url = f.download_path().map(|p| ctx.server.absolute(p));
                    let file = url.as_ref().map(|url| FileRef {
                        mid: f.id.clone(),
                        url: url.clone(),
                        name: f.name.clone(),
                        size: Some(f.size),
                        mime: f.mime.clone(),
                    });
                    let transfer = url
                        .as_deref()
                        .and_then(|u| self.viewer.render.transfer(&f.id, u));
                    match transfer {
                        Some(Transfer::Running { received, total }) => {
                            let total = total.unwrap_or(f.size);
                            meta = vec![
                                t!(
                                    "room_panel.file_downloading",
                                    received = human_size(*received),
                                    total = human_size(total)
                                )
                                .into_owned(),
                            ];
                        }
                        Some(Transfer::Failed(e)) => meta = vec![e.clone()],
                        _ => {}
                    }
                    let action = match (transfer, &file) {
                        (Some(Transfer::Done(path)), _) => icon_button(
                            Icon::FolderOpen,
                            Some(Msg::View(ViewerMsg::Render(RenderMsg::OpenFile(
                                path.clone(),
                            )))),
                            t!("room_panel.file_open"),
                        ),
                        (Some(Transfer::Running { .. }), _) => {
                            icon_button(Icon::Download, None, t!("room_panel.download"))
                        }
                        (_, Some(file)) => icon_button(
                            Icon::Download,
                            Some(Msg::View(ViewerMsg::Render(RenderMsg::Download(
                                file.clone(),
                            )))),
                            t!("room_panel.download"),
                        ),
                        (_, None) => icon_button(Icon::Download, None, t!("room_panel.download")),
                    };
                    // Images open in the lightbox, audio/video in the player.
                    let open = match (f.type_group.as_deref(), &file) {
                        (Some("image"), Some(file)) => Some(RenderMsg::OpenImage(ImageRef {
                            mid: file.mid.clone(),
                            url: file.url.clone(),
                            title: file.name.clone(),
                            mime: file.mime.clone(),
                        })),
                        (Some("video" | "audio"), Some(file)) => {
                            Some(RenderMsg::Play(file.clone()))
                        }
                        _ => None,
                    };
                    let name: Element<'a, Msg> = match open {
                        Some(m) => mouse_area(
                            ellipsis(f.name.clone())
                                .size(TEXT_BODY)
                                .font(font_medium())
                                .style(|theme| Some(rustket_ui::theme::tokens(theme).link)),
                        )
                        .interaction(iced::mouse::Interaction::Pointer)
                        .on_press(Msg::View(ViewerMsg::Render(m)))
                        .into(),
                        None => ellipsis(f.name.clone())
                            .size(TEXT_BODY)
                            .font(font_medium())
                            .into(),
                    };
                    let line = row![
                        iced::widget::container(
                            icon::sized(file_icon(f), 20.0).style(style::text_secondary)
                        )
                        .padding(6)
                        .style(style::container_card),
                        column![
                            name,
                            text(meta.join(" · "))
                                .size(TEXT_CAPTION)
                                .style(style::text_hint),
                        ]
                        .width(Length::Fill),
                        action,
                    ]
                    .spacing(SPACE_3)
                    .align_y(Alignment::Center);
                    col = col.push(iced::widget::container(line).padding([SPACE_2, SPACE_4]));
                }
                col = col.push(common::load_more(
                    self.list.state == LoadState::LoadingMore,
                    self.list.has_more(),
                    Msg::More,
                ));
                scrollable(col)
                    .height(Length::Fill)
                    .style(style::scrollable)
                    .into()
            }
        };
        column![top, body].height(Length::Fill).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(11), "11 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
