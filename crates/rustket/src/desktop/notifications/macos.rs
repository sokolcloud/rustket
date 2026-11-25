//! macOS backend: `UNUserNotificationCenter` through objc2.
//!
//! Every notification's identifier is `rustket-<launch nonce>-<id>`
//! ([`text::mac_identifier`]). A room keeps one notification: the shared
//! [`Outgoing::replaces`] logic reuses the room's id, and posting removes
//! the delivered notification with that identifier before adding the new
//! request, so every message gets a fresh banner and sound. `close` removes
//! the delivered notification (read rooms, logout). Notifications of earlier
//! launches are removed at startup, and responses to any with a foreign
//! nonce are ignored; so a click on a leftover notification that relaunches
//! the app only opens the app.
//!
//! Authorization is asked for with the first notification (alerts and
//! sounds). While it is denied, posting ends [`Event::NotAllowed`] and the
//! app plays the sound itself; the user can allow notifications later in
//! System Settings, which the next notification picks up.
//!
//! Actions: each set of buttons is a `UNNotificationCategory`
//! ([`text::mac_category`]), registered when first used (again when the
//! labels change with the language). Categories ask for dismissals
//! (`CustomDismissAction`). One app-wide delegate ([`install`], before the
//! app finishes launching) reports responses through [`super::native`]:
//! clicking the notification is [`super::ACTION_DEFAULT`], a button its key,
//! a dismissal [`super::Signal::Closed`]. It also presents notifications
//! while the app is frontmost.
//!
//! Threading: the center is used from any thread; requests are built and
//! added on a blocking worker, and the delegate and completion handlers run
//! on a queue of the framework's choosing. A post's [`Event`] comes from its
//! completion handler, so a `close` (which needs the id from that event)
//! always follows the add.
//!
//! The center exists only for an app bundle with a bundle id (the framework
//! raises otherwise): unbundled development builds log once and show no
//! notifications.
//!
//! Sound: the notification plays [`Outgoing::sound`] (so Focus applies),
//! as a copy of the Rocket.Chat sound in `Library/Sounds` (the container's
//! under the App Sandbox) named by its file name
//! ([`sound::notification_sound`], prepared on the worker), else the
//! default notification sound. The avatar is an attachment written to a
//! temporary file that is removed once the request was added.
//!
//! Parity with the native API: the timeout and urgency of [`Outgoing`] have
//! no counterpart (banner or alert is the user's choice in System Settings).
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use block2::{DynBlock, RcBlock};
use futures::FutureExt;
use futures::channel::oneshot;
use futures::future::BoxFuture;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, MainThreadMarker, define_class, msg_send};
use objc2_foundation::{NSArray, NSBundle, NSError, NSSet, NSString, NSURL, ns_string};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationAction, UNNotificationActionOptions, UNNotificationAttachment,
    UNNotificationCategory, UNNotificationCategoryOptions, UNNotificationDefaultActionIdentifier,
    UNNotificationDismissActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSettings, UNNotificationSound,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use super::super::sound::{self, OsSound, Platform, Route, Toast};
use super::{Event, Notifier, Outgoing, native, text};

/// Nonce of this launch in the identifiers.
static NONCE: LazyLock<String> = LazyLock::new(|| {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    text::launch_nonce(std::process::id(), millis)
});

/// The delegate, once installed (`None`: no notification center).
static DELEGATE: OnceLock<Option<Retained<Delegate>>> = OnceLock::new();

/// The registered categories: id → buttons `(key, label)`.
static CATEGORIES: Mutex<BTreeMap<String, Vec<(String, String)>>> = Mutex::new(BTreeMap::new());

/// The notification center; `None` outside an app bundle.
fn center() -> Option<Retained<UNUserNotificationCenter>> {
    // `currentNotificationCenter` raises when the process has no bundle id.
    NSBundle::mainBundle().bundleIdentifier()?;
    Some(UNUserNotificationCenter::currentNotificationCenter())
}

fn identifiers(identifier: &NSString) -> Retained<NSArray<NSString>> {
    NSArray::from_slice(&[identifier])
}

/// Reports a response to one of this launch's notifications.
fn respond(center: &UNUserNotificationCenter, response: &UNNotificationResponse) {
    let identifier = response.notification().request().identifier();
    let Some(id) = text::parse_mac_identifier(&NONCE, &identifier.to_string()) else {
        return;
    };
    let action = response.actionIdentifier();
    let key = action.to_string();
    // SAFETY: immutable `NSString` constants of the framework.
    let (default, dismiss) = unsafe {
        (
            UNNotificationDefaultActionIdentifier,
            UNNotificationDismissActionIdentifier,
        )
    };
    let response = if action.isEqualToString(default) {
        text::MacResponse::Default
    } else if action.isEqualToString(dismiss) {
        text::MacResponse::Dismiss
    } else {
        text::MacResponse::Action(&key)
    };
    center.removeDeliveredNotificationsWithIdentifiers(&identifiers(&identifier));
    native::emit(id, text::mac_signal(response));
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Delegate` does
    // not implement `Drop`. `Delegate` is `Send + Sync` through NSObject
    // (needed by `DELEGATE`); it has no state, and its methods only use the
    // center and the signal channel. Any future ivar must itself be
    // `Send + Sync`.
    #[unsafe(super = NSObject)]
    #[name = "RustketUNNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the methods have the protocol's signatures.
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            // The app in front shows its notifications as in the background.
            handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            handler: &DynBlock<dyn Fn()>,
        ) {
            respond(center, response);
            handler.call(());
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        // SAFETY: NSObject's `init` takes no arguments and returns the object.
        unsafe { msg_send![Self::alloc(), init] }
    }
}

/// Sets up the notification center: removes earlier launches'
/// notifications and images and installs the delegate. Call on the main
/// thread before the iced application starts (a click that launches the
/// app is delivered once it finished launching).
pub fn install() {
    if MainThreadMarker::new().is_none() {
        tracing::warn!("the notification delegate must be installed on the main thread");
        return;
    }
    // No NSApplication (and so no autorelease pool) exists yet.
    DELEGATE.get_or_init(|| {
        autoreleasepool(|_| {
            let Some(center) = center() else {
                tracing::info!(
                    "no notification center (not running from the app bundle): notifications are off"
                );
                return None;
            };
            center.removeAllDeliveredNotifications();
            native::clear_images();
            let delegate = Delegate::new();
            // The center does not retain its delegate: `DELEGATE` keeps it
            // for the rest of the process.
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            Some(delegate)
        })
    });
}

fn delegate_installed() -> bool {
    DELEGATE.get().is_some_and(Option::is_some)
}

/// A completion handler body that sends its result once.
fn once<T: 'static>(tx: oneshot::Sender<T>) -> impl Fn(T) + 'static {
    let tx = Mutex::new(Some(tx));
    move |v| {
        if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
            let _ = tx.send(v);
        }
    }
}

/// The authorization status (`None`: no center).
fn authorization_status() -> Option<oneshot::Receiver<UNAuthorizationStatus>> {
    let (tx, rx) = oneshot::channel();
    let send = once(tx);
    autoreleasepool(|_| {
        let center = center()?;
        let block = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: the framework passes a valid settings object for the
            // duration of the call.
            send(unsafe { settings.as_ref() }.authorizationStatus());
        });
        center.getNotificationSettingsWithCompletionHandler(&block);
        Some(rx)
    })
}

/// Asks the user to allow alerts and sounds (`None`: no center).
fn request_authorization() -> Option<oneshot::Receiver<bool>> {
    let (tx, rx) = oneshot::channel();
    let send = once(tx);
    autoreleasepool(|_| {
        let center = center()?;
        let block = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            send(granted.as_bool());
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &block,
        );
        Some(rx)
    })
}

/// Whether notifications may be shown; asks on first use.
async fn authorized() -> bool {
    static DENIED_LOGGED: AtomicBool = AtomicBool::new(false);
    let Some(rx) = authorization_status() else {
        return false;
    };
    let Ok(status) = rx.await else {
        return false;
    };
    let allowed = if status == UNAuthorizationStatus::NotDetermined {
        match request_authorization() {
            Some(rx) => rx.await.unwrap_or(false),
            None => false,
        }
    } else {
        status == UNAuthorizationStatus::Authorized
            || status == UNAuthorizationStatus::Provisional
            || status == UNAuthorizationStatus::Ephemeral
    };
    if !allowed && !DENIED_LOGGED.swap(true, Ordering::Relaxed) {
        tracing::info!("notifications are not allowed in System Settings");
    }
    allowed
}

/// Registers the category of `cat` unless it is registered with these
/// buttons already (`setNotificationCategories` replaces the whole set).
fn ensure_category(center: &UNUserNotificationCenter, cat: &text::MacCategory<'_>) {
    let mut registered = CATEGORIES.lock().unwrap_or_else(|p| p.into_inner());
    let buttons: Vec<(String, String)> = cat
        .buttons
        .iter()
        .map(|(k, l)| ((*k).to_owned(), (*l).to_owned()))
        .collect();
    if registered.get(&cat.id) == Some(&buttons) {
        return;
    }
    registered.insert(cat.id.clone(), buttons);
    let categories: Vec<Retained<UNNotificationCategory>> = registered
        .iter()
        .map(|(id, buttons)| {
            let actions: Vec<Retained<UNNotificationAction>> = buttons
                .iter()
                .map(|(k, l)| {
                    UNNotificationAction::actionWithIdentifier_title_options(
                        &NSString::from_str(k),
                        &NSString::from_str(l),
                        UNNotificationActionOptions::Foreground,
                    )
                })
                .collect();
            UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &NSString::from_str(id),
                &NSArray::from_retained_slice(&actions),
                &NSArray::from_slice(&[]),
                UNNotificationCategoryOptions::CustomDismissAction,
            )
        })
        .collect();
    center.setNotificationCategories(&NSSet::from_retained_slice(&categories));
}

/// The avatar file as an attachment.
fn attachment(image: &native::TempImage) -> Option<Retained<UNNotificationAttachment>> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&image.path().to_string_lossy()));
    // SAFETY: no options dictionary is passed.
    let made = unsafe {
        UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(
            ns_string!("avatar"),
            &url,
            None,
        )
    };
    made.inspect_err(|e| {
        tracing::debug!(error = %e.localizedDescription(), "notification image not attached");
    })
    .ok()
}

/// Adds `n` as notification `id`, playing its sound as `route` says. The
/// receiver gets the error of the add, if any (`None`: no center).
fn post(id: u32, n: &Outgoing, route: &Route) -> Option<oneshot::Receiver<Option<String>>> {
    let (tx, rx) = oneshot::channel();
    let send = once(tx);
    let image = n
        .image
        .as_ref()
        .and_then(|(w, h, px)| native::temp_image(*w, *h, px));
    autoreleasepool(|_| {
        let center = center()?;
        let identifier = NSString::from_str(&text::mac_identifier(&NONCE, id));
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&n.title));
        content.setBody(&NSString::from_str(&n.body));
        let category = text::mac_category(&n.actions);
        ensure_category(&center, &category);
        content.setCategoryIdentifier(&NSString::from_str(&category.id));
        let sound = match route {
            Route::Os(OsSound::File(f)) => Some(match text::mac_sound_name(f) {
                Some(name) => UNNotificationSound::soundNamed(&NSString::from_str(name)),
                None => UNNotificationSound::defaultSound(),
            }),
            Route::Os(OsSound::Default) => Some(UNNotificationSound::defaultSound()),
            Route::Silent | Route::Local { .. } => None,
        };
        content.setSound(sound.as_deref());
        if let Some(a) = image.as_ref().and_then(attachment) {
            content.setAttachments(&NSArray::from_retained_slice(&[a]));
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &identifier,
            &content,
            None,
        );
        // Remove, then add: a fresh banner and sound for every message.
        center.removeDeliveredNotificationsWithIdentifiers(&identifiers(&identifier));
        // The image file lives until the add finished.
        let image = Mutex::new(image);
        let done = RcBlock::new(move |error: *mut NSError| {
            drop(image.lock().ok().and_then(|mut i| i.take()));
            // SAFETY: a non-null `error` is a valid `NSError` for the
            // duration of the call.
            let error = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
            send(error);
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&done));
        Some(rx)
    })
}

/// `UNUserNotificationCenter` notifications.
pub struct MacNotifier {
    next_id: AtomicU32,
}

impl MacNotifier {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU32::new(0),
        }
    }
}

impl Notifier for MacNotifier {
    fn show(&self, n: Outgoing) -> BoxFuture<'static, Event> {
        let id = n
            .replaces
            .unwrap_or_else(|| self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        async move {
            if !delegate_installed() {
                return Event::Failed("no notification center".into());
            }
            if !authorized().await {
                return Event::NotAllowed;
            }
            let posted = tokio::task::spawn_blocking(move || {
                let file = n.sound.as_ref().and_then(sound::notification_sound);
                let route = sound::route(
                    Platform::MacOs,
                    Toast::Shown { plays_sound: true },
                    n.sound.as_ref(),
                    file.as_deref(),
                );
                (post(id, &n, &route), route.by_os())
            })
            .await;
            match posted {
                Ok((Some(rx), server_sound)) => match rx.await {
                    Ok(None) => Event::Shown { id, server_sound },
                    Ok(Some(e)) => Event::Failed(e),
                    Err(_) => Event::Failed("the notification was not added".into()),
                },
                Ok((None, _)) => Event::Failed("no notification center".into()),
                Err(e) => Event::Failed(e.to_string()),
            }
        }
        .boxed()
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        if delegate_installed() {
            autoreleasepool(|_| {
                if let Some(center) = center() {
                    let identifier = NSString::from_str(&text::mac_identifier(&NONCE, id));
                    center.removeDeliveredNotificationsWithIdentifiers(&identifiers(&identifier));
                }
            });
        }
        async {}.boxed()
    }

    fn listens(&self) -> bool {
        true
    }
}
