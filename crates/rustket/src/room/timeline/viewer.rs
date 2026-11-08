//! Interaction state of rendered message bodies, shared by every message
//! list (room timeline, thread, pinned/search/files panels): revealed
//! spoilers, collapsed attachments, downloads with progress, media opened
//! with the system player and the image lightbox. Links, mentions and
//! copies become [`Effect`]s.

use std::path::PathBuf;

use iced::{Element, Task};
use rocketchat::models::Message as RcMessage;
use rust_i18n::t;
use rustket_ui::widget::BannerKind;

use super::actions;
use super::dialogs;
use super::transfer::{self, DlEvent};
use crate::images::ImageRequest;
use crate::room::panel::Kind;
use crate::room::render::{self, FileRef, ImageRef, RenderCtx, RenderMsg, RenderState, Transfer};
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone)]
pub enum ViewerMsg {
    Render(RenderMsg),
    CloseImage,
    ToggleZoom,
    /// Download the lightbox image privately and open it in the image viewer.
    OpenImageExternally,
    SaveImage,
    SaveTo(FileRef, Option<PathBuf>),
    Transfer(FileRef, DlEvent, bool),
    Opened(Result<(), String>),
}

#[derive(Debug, Clone, PartialEq)]
struct Lightbox {
    image: ImageRef,
    /// 1:1 with panning and wheel zoom instead of fit-to-window.
    zoom: bool,
}

#[derive(Debug, Default)]
pub struct Viewer {
    pub render: RenderState,
    lightbox: Option<Lightbox>,
}

fn toast_error(title: String, body: String) -> Effect {
    Effect::Toast(BannerKind::Error, title, body)
}

fn toast_ok(title: String) -> Effect {
    Effect::Toast(BannerKind::Success, title, String::new())
}

impl Viewer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Render context for bodies of this list.
    pub fn rc<'a>(&'a self, ctx: &ViewCtx<'a>) -> RenderCtx<'a> {
        RenderCtx::from_view(ctx, &self.render)
    }

    /// The interactive body of `m`.
    pub fn body<'a>(&self, m: &RcMessage, ctx: &ViewCtx<'a>) -> Element<'static, ViewerMsg> {
        render::message_body_with(m, &RenderCtx::from_view(ctx, &self.render))
            .map(ViewerMsg::Render)
    }

    pub fn image_open(&self) -> bool {
        self.lightbox.is_some()
    }

    /// Shows `image` in the lightbox (fetched right away).
    pub fn open_image(&mut self, image: ImageRef, ctx: &UpdateCtx<'_>) {
        ctx.images.want(ImageRequest {
            server_id: ctx.server_id.clone(),
            url: image.url.clone(),
        });
        self.lightbox = Some(Lightbox { image, zoom: false });
    }

    pub fn close_image(&mut self) {
        self.lightbox = None;
    }

    /// The lightbox, if open (shown as a modal by the room view).
    pub fn overlay<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<Element<'a, ViewerMsg>> {
        let lb = self.lightbox.as_ref()?;
        Some(dialogs::lightbox(
            &lb.image,
            lb.zoom,
            ctx.images.get(ctx.server_id, &lb.image.url),
        ))
    }

    pub fn update(&mut self, msg: ViewerMsg, ctx: &mut UpdateCtx<'_>) -> Action<ViewerMsg> {
        match msg {
            ViewerMsg::Render(r) => self.render_msg(r, ctx),
            ViewerMsg::CloseImage => {
                self.lightbox = None;
                Action::none()
            }
            ViewerMsg::ToggleZoom => {
                if let Some(lb) = &mut self.lightbox {
                    lb.zoom = !lb.zoom;
                }
                Action::none()
            }
            ViewerMsg::OpenImageExternally => match &self.lightbox {
                Some(Lightbox { image, .. }) => {
                    let ext = image
                        .mime
                        .as_deref()
                        .and_then(transfer::media_ext)
                        .filter(|e| transfer::media_ext(&format!("image/{e}")).is_some())
                        .or_else(|| transfer::image_ext_from_url(&image.url));
                    let f = FileRef {
                        mid: image.mid.clone(),
                        url: image.url.clone(),
                        name: image.title.clone(),
                        size: None,
                        mime: image.mime.clone(),
                    };
                    self.open_privately(f, ext, ctx)
                }
                None => Action::none(),
            },
            ViewerMsg::SaveImage => match &self.lightbox {
                Some(Lightbox { image, .. }) => {
                    let f = FileRef {
                        mid: image.mid.clone(),
                        url: image.url.clone(),
                        name: if image.title.is_empty() {
                            "image".into()
                        } else {
                            image.title.clone()
                        },
                        size: None,
                        mime: image.mime.clone(),
                    };
                    self.render_msg(RenderMsg::Download(f), ctx)
                }
                None => Action::none(),
            },
            ViewerMsg::SaveTo(f, path) => match path {
                Some(p) => self.start_download(f, p, transfer::Target::Chosen, false, ctx),
                None => Action::none(),
            },
            ViewerMsg::Transfer(f, ev, open_after) => match ev {
                DlEvent::Progress(received, total) => {
                    self.render
                        .set_transfer(&f.mid, &f.url, Transfer::Running { received, total });
                    Action::none()
                }
                DlEvent::Done(path) => {
                    if open_after && crate::e2e::files::is_marked(&f.url) {
                        // Decrypted media is not kept on disk (removed later).
                        self.render.clear_transfer(&f.mid, &f.url);
                        return Action::task(Task::perform(
                            async move {
                                let r = transfer::open_path(path.clone()).await;
                                crate::e2e::files::delete_later(path);
                                r
                            },
                            ViewerMsg::Opened,
                        ));
                    }
                    self.render
                        .set_transfer(&f.mid, &f.url, Transfer::Done(path.clone()));
                    if open_after {
                        Action::task(Task::perform(transfer::open_path(path), ViewerMsg::Opened))
                    } else {
                        Action::effect(Effect::Toast(
                            BannerKind::Success,
                            t!("room_timeline.downloaded").into_owned(),
                            path.display().to_string(),
                        ))
                    }
                }
                DlEvent::Failed(e) => {
                    self.render
                        .set_transfer(&f.mid, &f.url, Transfer::Failed(e.clone()));
                    Action::effect(toast_error(
                        t!("room_timeline.download_failed", name = f.name.as_str()).into_owned(),
                        e,
                    ))
                }
            },
            ViewerMsg::Opened(r) => match r {
                Ok(()) => Action::none(),
                Err(e) => {
                    Action::effect(toast_error(t!("room_timeline.open_failed").into_owned(), e))
                }
            },
        }
    }

    /// Handles an interaction inside a rendered body.
    pub fn render_msg(&mut self, r: RenderMsg, ctx: &mut UpdateCtx<'_>) -> Action<ViewerMsg> {
        if self.render.handle(&r) {
            return Action::none();
        }
        match r {
            RenderMsg::OpenUrl(u) => Action::effect(Effect::OpenUrl(u)),
            RenderMsg::OpenRoom(name) => match render::resolve_room(ctx.workspace, &name) {
                Some(rid) => Action::effect(Effect::OpenRoom(rid)),
                // Not joined: the web shows a preview; open the channel there.
                None => match actions::channel_url(ctx.server, ctx.workspace, &name) {
                    Some(url) => Action::effect(Effect::OpenUrl(url)),
                    None => Action::effect(toast_error(
                        t!("room_timeline.room_not_found", name = name.as_str()).into_owned(),
                        String::new(),
                    )),
                },
            },
            RenderMsg::OpenUser(u) => Action::effect(Effect::OpenPanel(Kind::UserInfo(u))),
            RenderMsg::Copy(s) => Action::task(iced::clipboard::write(s))
                .with(toast_ok(t!("room_timeline.copied").into_owned())),
            RenderMsg::OpenImage(image) => {
                self.open_image(image, ctx);
                Action::none()
            }
            RenderMsg::Download(f) => {
                let name = f.name.clone();
                Action::task(Task::perform(transfer::ask_save_path(name), move |p| {
                    ViewerMsg::SaveTo(f.clone(), p)
                }))
            }
            RenderMsg::Play(f) => {
                let ext = f.mime.as_deref().and_then(transfer::media_ext);
                if ext.is_none() {
                    // Not a known audio/video type: let the user save it.
                    return self.render_msg(RenderMsg::Download(f), ctx);
                }
                self.open_privately(f, ext, ctx)
            }
            RenderMsg::OpenFile(p) => {
                Action::task(Task::perform(transfer::open_path(p), ViewerMsg::Opened))
            }
            RenderMsg::ToggleSpoiler(..) | RenderMsg::ToggleCollapsed(..) => Action::none(),
        }
    }

    /// Downloads `f` into a new private media file with extension `ext` (from
    /// an allow-list of media types) and opens it with the system handler.
    fn open_privately(
        &mut self,
        f: FileRef,
        ext: Option<&'static str>,
        ctx: &UpdateCtx<'_>,
    ) -> Action<ViewerMsg> {
        let mut dir = transfer::media_dir(ctx.images.disk_dir());
        let mut purge = Task::none();
        if crate::e2e::files::is_marked(&f.url) {
            dir = dir.map(|d| crate::e2e::files::private_media_dir(&d));
            purge = Task::perform(crate::e2e::files::purge_private_media(true), |_| {
                ViewerMsg::Opened(Ok(()))
            });
        }
        match (ext, dir) {
            (Some(ext), Some(dir)) => {
                let path = transfer::media_path(&dir, ext);
                let mut dl = self.start_download(f, path, transfer::Target::Private, true, ctx);
                dl.task = Task::batch([purge, dl.task]);
                dl
            }
            _ => Action::effect(toast_error(
                t!("room_timeline.unsupported_media").into_owned(),
                String::new(),
            )),
        }
    }

    fn start_download(
        &mut self,
        f: FileRef,
        path: PathBuf,
        target: transfer::Target,
        open_after: bool,
        ctx: &UpdateCtx<'_>,
    ) -> Action<ViewerMsg> {
        let Some(rest) = ctx.rest() else {
            return Action::effect(toast_error(
                t!("room_timeline.download_failed", name = f.name.as_str()).into_owned(),
                t!("room_timeline.offline").into_owned(),
            ));
        };
        self.render.set_transfer(
            &f.mid,
            &f.url,
            Transfer::Running {
                received: 0,
                total: f.size,
            },
        );
        let stream = transfer::download(rest, f.url.clone(), path, target);
        Action::task(Task::run(stream, move |ev| {
            ViewerMsg::Transfer(f.clone(), ev, open_after)
        }))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn toggles_and_transfers() {
        let images = crate::images::Images::new(None);
        let prefs = rustket_core::config::UiPrefs::default();
        let sid = rustket_core::ServerId("s".into());
        let url = rocketchat::ServerUrl::parse("http://localhost:3000").unwrap();
        let mut ws = rustket_core::Workspace::new();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "r",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let mut v = Viewer::new();
        let k0 = v.render.key("m");
        let a = v.update(
            ViewerMsg::Render(RenderMsg::ToggleSpoiler("m".into(), 0)),
            &mut ctx,
        );
        assert!(a.effects.is_empty());
        assert!(v.render.is_revealed("m", 0));
        assert_ne!(v.render.key("m"), k0);
        // Images open the lightbox; closing it forgets it.
        let img = ImageRef {
            mid: "m".into(),
            url: "http://localhost:3000/a.png".into(),
            title: "a.png".into(),
            mime: Some("image/png".into()),
        };
        let _ = v.update(ViewerMsg::Render(RenderMsg::OpenImage(img)), &mut ctx);
        assert!(v.image_open());
        let _ = v.update(ViewerMsg::CloseImage, &mut ctx);
        assert!(!v.image_open());
        // Without a session a download fails right away with a toast.
        let f = FileRef {
            mid: "m".into(),
            url: "http://localhost:3000/f.pdf".into(),
            name: "f.pdf".into(),
            size: Some(10),
            mime: None,
        };
        let a = v.update(
            ViewerMsg::SaveTo(f.clone(), Some("/tmp/x".into())),
            &mut ctx,
        );
        assert!(matches!(a.effects.as_slice(), [Effect::Toast(..)]));
        // Progress events are recorded per message and URL.
        let _ = v.update(
            ViewerMsg::Transfer(f.clone(), DlEvent::Progress(5, Some(10)), false),
            &mut ctx,
        );
        assert_eq!(
            v.render.transfer("m", &f.url),
            Some(&Transfer::Running {
                received: 5,
                total: Some(10)
            })
        );
        let a = v.update(
            ViewerMsg::Transfer(f.clone(), DlEvent::Failed("x".into()), false),
            &mut ctx,
        );
        assert!(matches!(a.effects.as_slice(), [Effect::Toast(..)]));
        // Mentions become effects.
        let a = v.update(
            ViewerMsg::Render(RenderMsg::OpenUser("bob".into())),
            &mut ctx,
        );
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::OpenPanel(Kind::UserInfo(u))] if u == "bob"
        ));
    }
}
