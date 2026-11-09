//! StatusNotifierItem backend (ksni). The service runs inside the tray
//! subscription; the app's state reaches it through the `watch` channel.

use rust_i18n::t;

use super::{Message, Slot, TrayState, status_label, tooltip};

struct RustketTray {
    state: TrayState,
    tx: futures::channel::mpsc::UnboundedSender<Message>,
}

impl RustketTray {
    fn send(&self, m: Message) {
        let _ = self.tx.unbounded_send(m);
    }
}

impl ksni::Tray for RustketTray {
    fn id(&self) -> String {
        "rustket".into()
    }

    fn title(&self) -> String {
        t!("desktop.app_name").into_owned()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Communications
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        super::super::icon::tray_pixmaps(self.state.unread > 0 || self.state.mentions > 0)
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: t!("desktop.app_name").into_owned(),
            description: tooltip(&self.state),
            icon_pixmap: Vec::new(),
            icon_name: String::new(),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Message::Activate);
    }

    fn watcher_online(&self) {
        self.send(Message::Ready(true));
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        // Keep the service: the host may come back (shell restart).
        self.send(Message::Ready(false));
        true
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};
        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem {
                label: t!("desktop.tray_open").into_owned(),
                activate: Box::new(|t: &mut Self| t.send(Message::Show)),
                ..Default::default()
            }
            .into(),
        ];
        if let Some(status) = self.state.status
            && !self.state.offered.is_empty()
        {
            let offered = self.state.offered.clone();
            items.push(
                SubMenu {
                    label: t!("desktop.tray_status").into_owned(),
                    submenu: vec![
                        RadioGroup {
                            selected: offered.iter().position(|s| *s == status).unwrap_or(0),
                            select: Box::new(|t: &mut Self, i| {
                                if let Some(s) = t.state.offered.get(i) {
                                    t.send(Message::SetStatus(*s));
                                }
                            }),
                            options: offered
                                .iter()
                                .map(|s| RadioItem {
                                    label: status_label(*s),
                                    ..Default::default()
                                })
                                .collect(),
                        }
                        .into(),
                    ],
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(ksni::MenuItem::Separator);
        items.push(
            StandardItem {
                label: t!("desktop.tray_quit").into_owned(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| t.send(Message::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

pub(super) fn run(slot: &Slot) -> impl futures::Stream<Item = Message> + use<> {
    let mut rx = (*slot.0).clone();
    iced::stream::channel(
        16,
        async move |mut out: futures::channel::mpsc::Sender<Message>| {
            use futures::{SinkExt, StreamExt};
            use ksni::TrayMethods;
            let (tx, mut events) = futures::channel::mpsc::unbounded();
            let tray = RustketTray {
                state: rx.borrow_and_update().clone(),
                tx,
            };
            let handle = match tray.spawn().await {
                Ok(h) => h,
                Err(e) => {
                    tracing::info!(error = %e, "no system tray");
                    let _ = out.send(Message::Ready(false)).await;
                    return;
                }
            };
            let _ = out.send(Message::Ready(true)).await;
            loop {
                tokio::select! {
                    changed = rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let state = rx.borrow_and_update().clone();
                        if handle.update(|t| t.state = state).await.is_none() {
                            // The service died: no icon any more.
                            let _ = out.send(Message::Ready(false)).await;
                            return;
                        }
                    }
                    ev = events.next() => match ev {
                        Some(m) => {
                            if out.send(m).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }
            handle.shutdown().await;
        },
    )
}
