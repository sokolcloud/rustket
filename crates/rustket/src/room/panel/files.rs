//! Files of the room (`channels.files` / `groups.files` / `im.files`) with a
//! type filter, name search and download through a save dialog.

use std::path::PathBuf;

use iced::futures::StreamExt;
use iced::widget::{column, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::Paginated;
use rust_i18n::t;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{FONT_MEDIUM, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{BannerKind, ellipsis, icon_button};
use tokio::io::AsyncWriteExt;

use super::api::{self, FILE_TYPES, FileItem};
use super::common;
use super::debounce;
use super::paged::{LoadState, PageReq, Paged};
use crate::error::Res;
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    Search(u64),
    Type(Option<&'static str>),
    Page(PageReq, Res<Paginated<FileItem>>),
    More,
    Retry,
    Download(usize),
    Downloaded(String, Res<Option<PathBuf>>),
}

#[derive(Default)]
pub struct Files {
    query: String,
    query_seq: u64,
    type_group: Option<&'static str>,
    list: Paged<FileItem>,
    downloading: Vec<String>,
}

/// `1.5 MB`-style size.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
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

/// Saves `path` (server-relative or absolute) to a file chosen in a dialog.
async fn save(
    rest: rocketchat::RestClient,
    url: String,
    name: String,
) -> rocketchat::Result<Option<PathBuf>> {
    let Some(handle) = rfd::AsyncFileDialog::new()
        .set_file_name(&name)
        .save_file()
        .await
    else {
        return Ok(None);
    };
    let target = handle.path().to_owned();
    let part = target.with_extension(match target.extension() {
        Some(e) => format!("{}.part", e.to_string_lossy()),
        None => "part".into(),
    });
    let dl = rest.download(&url).await?;
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = std::pin::pin!(dl.stream());
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => file.write_all(&bytes).await?,
            Err(e) => {
                drop(file);
                let _ = tokio::fs::remove_file(&part).await;
                return Err(e);
            }
        }
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&part, &target).await?;
    Ok(Some(target))
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
                self.list.finish(req, r);
                Action::none()
            }
            Msg::More => match self.list.begin_more() {
                Some(req) => self.fetch(req, ctx),
                None => Action::none(),
            },
            Msg::Retry => self.reload(ctx),
            Msg::Download(i) => {
                let Some(f) = self.list.items.get(i) else {
                    return Action::none();
                };
                let Some(url) = f.download_path().map(str::to_owned) else {
                    return Action::none();
                };
                if self.downloading.contains(&f.id) {
                    return Action::none();
                }
                self.downloading.push(f.id.clone());
                let (id, name) = (f.id.clone(), f.name.clone());
                Action::task(ctx.rest_task(
                    move |rest| save(rest, url, name),
                    move |r| Msg::Downloaded(id, r),
                ))
            }
            Msg::Downloaded(id, r) => {
                self.downloading.retain(|d| d != &id);
                match r {
                    Ok(Some(path)) => Action::effect(Effect::Toast(
                        BannerKind::Success,
                        t!("room_panel.downloaded").into_owned(),
                        path.display().to_string(),
                    )),
                    Ok(None) => Action::none(),
                    Err(e) => {
                        Action::effect(common::error_toast(&t!("room_panel.download_failed"), &e))
                    }
                }
            }
        }
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
                for (i, f) in self.list.items.iter().enumerate() {
                    let mut meta = vec![human_size(f.size)];
                    if let Some(u) = f.user.as_ref().and_then(|u| u.username.clone()) {
                        meta.push(format!("@{u}"));
                    }
                    if f.uploaded_at.is_some() {
                        meta.push(common::time_label(f.uploaded_at, ctx));
                    }
                    let busy = self.downloading.contains(&f.id);
                    let line = row![
                        iced::widget::container(
                            icon::sized(file_icon(f), 20.0).style(style::text_secondary)
                        )
                        .padding(6)
                        .style(style::container_card),
                        column![
                            ellipsis(f.name.clone()).size(TEXT_BODY).font(FONT_MEDIUM),
                            text(meta.join(" · "))
                                .size(TEXT_CAPTION)
                                .style(style::text_hint),
                        ]
                        .width(Length::Fill),
                        icon_button(
                            Icon::Download,
                            (!busy && f.download_path().is_some()).then_some(Msg::Download(i)),
                            t!("room_panel.download"),
                        ),
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
