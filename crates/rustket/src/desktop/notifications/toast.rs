//! Windows backend: toast notifications (WinRT `Windows.UI.Notifications`).
//!
//! Toasts are posted under the AppUserModelID [`crate::BUNDLE_ID`], which is
//! registered for the current user (`HKCU\Software\Classes\AppUserModelId`)
//! so toasts work from the portable zip as well as from the installed app.
//! Our id is the toast's tag (group `rustket`): a replacing toast reuses
//! the tag and so replaces the room's previous toast, and closing removes it
//! from the Action Center. Activation and dismissal handlers run on WinRT
//! worker threads and report through [`super::native`]. The toast objects
//! are kept alive while shown, as their handlers live on them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use futures::FutureExt;
use futures::future::BoxFuture;
use rust_i18n::t;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{
    ToastActivatedEventArgs, ToastDismissalReason, ToastDismissedEventArgs, ToastNotification,
    ToastNotificationManager,
};
use windows::core::{HSTRING, IInspectable, Interface, Ref};

use super::{ACTION_DEFAULT, Event, Notifier, Outgoing, Signal, native, text};

const GROUP: &str = "rustket";

/// Registers the AppUserModelID once (display name and icon of the toasts).
fn ensure_registered() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        if let Err(e) = register_aumid() {
            tracing::warn!(error = %e, "cannot register the AppUserModelID");
        }
    });
}

fn register_aumid() -> Result<(), String> {
    let icon = directories::ProjectDirs::from("", "", "rustket").and_then(|d| {
        let dir = d.data_local_dir().to_owned();
        let path = dir.join("rustket.png");
        let png = crate::desktop::icon::png(256, false)?;
        let current = std::fs::read(&path).is_ok_and(|b| b == png);
        if !current {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::write(&path, png).ok()?;
        }
        Some(path)
    });
    let key = windows_registry::CURRENT_USER
        .create(format!(
            r"Software\Classes\AppUserModelId\{}",
            crate::BUNDLE_ID
        ))
        .map_err(|e| e.to_string())?;
    key.set_string("DisplayName", &*t!("desktop.app_name"))
        .map_err(|e| e.to_string())?;
    if let Some(icon) = icon {
        key.set_string("IconUri", &*icon.to_string_lossy())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

struct Live {
    generation: u64,
    _toast: ToastNotification,
}

#[derive(Default)]
struct Inner {
    next_id: AtomicU32,
    generation: AtomicU64,
    live: Mutex<HashMap<u32, Live>>,
}

impl Inner {
    /// Forgets `id` if it still is showing `generation`; returns whether it was.
    fn forget(&self, id: u32, generation: u64) -> bool {
        let Ok(mut live) = self.live.lock() else {
            return false;
        };
        if live.get(&id).is_some_and(|l| l.generation == generation) {
            live.remove(&id);
            return true;
        }
        false
    }

    fn show(self: &Arc<Self>, n: &Outgoing) -> windows::core::Result<u32> {
        ensure_registered();
        let id = n
            .replaces
            .unwrap_or_else(|| self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        let image = n
            .image
            .as_ref()
            .and_then(|(w, h, px)| native::image_file(*w, *h, px));
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(text::toast_xml(n, image.as_deref())))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        toast.SetTag(&HSTRING::from(id.to_string()))?;
        toast.SetGroup(&HSTRING::from(GROUP))?;

        let weak: Weak<Inner> = Arc::downgrade(self);
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
            move |_, args: Ref<IInspectable>| {
                let key = args
                    .as_ref()
                    .and_then(|a| a.cast::<ToastActivatedEventArgs>().ok())
                    .and_then(|a| a.Arguments().ok())
                    .map(|a| a.to_string_lossy())
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| ACTION_DEFAULT.to_owned());
                if let Some(inner) = weak.upgrade() {
                    inner.forget(id, generation);
                }
                native::emit(id, Signal::Action(key));
                Ok(())
            },
        ))?;
        let weak: Weak<Inner> = Arc::downgrade(self);
        toast.Dismissed(&TypedEventHandler::<
            ToastNotification,
            ToastDismissedEventArgs,
        >::new(
            move |_, args: Ref<ToastDismissedEventArgs>| {
                // Timed out toasts move to the Action Center and stay clickable.
                let timed_out = args
                    .as_ref()
                    .and_then(|a| a.Reason().ok())
                    .is_some_and(|r| r == ToastDismissalReason::TimedOut);
                if !timed_out
                    && weak
                        .upgrade()
                        .is_some_and(|inner| inner.forget(id, generation))
                {
                    native::emit(id, Signal::Closed);
                }
                Ok(())
            },
        ))?;

        if let Ok(mut live) = self.live.lock() {
            live.insert(
                id,
                Live {
                    generation,
                    _toast: toast.clone(),
                },
            );
        }
        let notifier =
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(crate::BUNDLE_ID))?;
        if let Err(e) = notifier.Show(&toast) {
            self.forget(id, generation);
            return Err(e);
        }
        Ok(id)
    }

    fn close(&self, id: u32) {
        if let Ok(mut live) = self.live.lock() {
            live.remove(&id);
        }
        let removed = ToastNotificationManager::History().and_then(|h| {
            h.RemoveGroupedTagWithId(
                &HSTRING::from(id.to_string()),
                &HSTRING::from(GROUP),
                &HSTRING::from(crate::BUNDLE_ID),
            )
        });
        if let Err(e) = removed {
            tracing::debug!(error = %e, "cannot remove a toast");
        }
    }
}

/// Toast notifications.
pub struct ToastNotifier {
    inner: Arc<Inner>,
}

impl ToastNotifier {
    /// Registers the AppUserModelID in the background.
    pub fn new() -> Self {
        let spawned = std::thread::Builder::new()
            .name("toast-register".into())
            .spawn(ensure_registered);
        if let Err(e) = spawned {
            tracing::debug!(error = %e, "cannot start the AppUserModelID registration");
        }
        Self {
            inner: Arc::new(Inner::default()),
        }
    }
}

impl Notifier for ToastNotifier {
    fn show(&self, n: Outgoing) -> BoxFuture<'static, Event> {
        let inner = Arc::clone(&self.inner);
        async move {
            let shown = tokio::task::spawn_blocking(move || {
                inner.show(&n).map(|id| Event::Shown {
                    id,
                    server_sound: n.sound,
                })
            })
            .await;
            match shown {
                Ok(Ok(event)) => event,
                Ok(Err(e)) => Event::Failed(e.to_string()),
                Err(e) => Event::Failed(e.to_string()),
            }
        }
        .boxed()
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        let inner = Arc::clone(&self.inner);
        async move {
            let _ = tokio::task::spawn_blocking(move || inner.close(id)).await;
        }
        .boxed()
    }

    fn listens(&self) -> bool {
        true
    }
}
