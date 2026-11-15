//! macOS backend: `NSUserNotificationCenter` through objc2.
//!
//! Every notification's identifier is `rustket-<launch nonce>-<id>`
//! ([`text::mac_identifier`]). A room keeps one notification: the shared
//! [`Outgoing::replaces`] logic reuses the room's id, and posting removes
//! the delivered notification with that identifier before delivering the
//! new one (a replacement in place by identifier updates Notification
//! Center silently, without a banner or sound). `close` removes the
//! delivered notification (read rooms, logout). Notifications of earlier
//! launches are removed at startup, and clicks on any with a foreign nonce
//! are ignored; so a click on a leftover notification that relaunches the
//! app only opens the app.
//!
//! One app-wide delegate ([`install`], on the main thread) receives the
//! clicks and alert dismissals and reports them through [`super::native`]:
//! clicking the notification is [`ACTION_DEFAULT`], the action button and
//! the additional actions (`NSUserNotificationAction`) report their key, and
//! closing an alert reports [`Signal::Closed`]. It also asks for
//! notifications to be presented while the app is frontmost.
//!
//! Threading: the notification (a plain data object) and its image are
//! built on a blocking worker; every call on the notification center
//! (deliver, enumerate, remove) runs on the main thread, where its delegate
//! callbacks arrive too: workers hand the work to the delegate object with
//! `performSelectorOnMainThread:`. Apple documents no thread safety for the
//! center, and the main run loop (winit's) is always running. A `close`
//! runs after its post: [`super`] only learns the id from the post's
//! [`Event::Shown`], which happens after the post was queued, and the main
//! run loop's perform queue is lock-protected. That is happens-before plus
//! the queue's implementation, not an ordering Apple documents across
//! threads.
//!
//! The center exists only for an app bundle with a bundle id: unbundled
//! development builds log once and show no notifications.
//!
//! Parity with the native API: the timeout and urgency of [`Outgoing`] have
//! no `NSUserNotification` counterpart (banner or alert is the user's choice
//! in System Settings), and whether buttons show depends on that style.
//!
//! With `macos_events.rs` the only place in the project with `unsafe` code.
#![allow(unsafe_code)]
// The whole `NSUserNotification` API is deprecated (since macOS 11) but works.
#![allow(deprecated)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, OnceLock};

use futures::FutureExt;
use futures::future::BoxFuture;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{AnyThread, ClassType, MainThreadMarker, define_class, msg_send, sel};
use objc2_app_kit::NSImage;
use objc2_foundation::{
    NSArray, NSDictionary, NSObjectNSThreadPerformAdditions, NSString, NSUserNotification,
    NSUserNotificationAction, NSUserNotificationActivationType, NSUserNotificationCenter,
    NSUserNotificationCenterDelegate, NSUserNotificationDefaultSoundName, ns_string,
};

use super::{Event, Notifier, Outgoing, Signal, native, text};

/// `userInfo` key of the action button's action key.
const MAIN_KEY: &str = "rustket.action";

/// Nonce of this launch in the identifiers.
static NONCE: LazyLock<String> = LazyLock::new(|| {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    text::launch_nonce(std::process::id(), millis)
});

/// The delegate, once installed (`None`: no notification center).
static DELEGATE: OnceLock<Option<Retained<Delegate>>> = OnceLock::new();

/// The notification center; `nil` outside an app bundle.
fn center() -> Option<Retained<NSUserNotificationCenter>> {
    // SAFETY: `+defaultUserNotificationCenter` takes no arguments and returns
    // the shared center or nil; nil is why the result is read as an Option
    // (the generated binding assumes non-null).
    unsafe {
        msg_send![
            NSUserNotificationCenter::class(),
            defaultUserNotificationCenter
        ]
    }
}

/// Removes the delivered notifications with `identifier`.
fn remove_delivered(center: &NSUserNotificationCenter, identifier: &NSString) {
    for note in center.deliveredNotifications().to_vec() {
        if note
            .identifier()
            .is_some_and(|i| i.isEqualToString(identifier))
        {
            center.removeDeliveredNotification(&note);
        }
    }
}

fn our_id(note: &NSUserNotification) -> Option<u32> {
    let identifier = note.identifier()?.to_string();
    text::parse_mac_identifier(&NONCE, &identifier)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Delegate` does
    // not implement `Drop`. `Delegate` is `Send + Sync` through NSObject
    // (needed by `DELEGATE` and the workers' `&'static Delegate`); that is
    // sound because other threads only call `performSelectorOnMainThread:`
    // on it and every method body runs on the main thread. Any future ivar
    // must itself be `Send + Sync`.
    #[unsafe(super = NSObject)]
    #[name = "RustketNotificationDelegate"]
    struct Delegate;

    impl Delegate {
        // SAFETY: called through `performSelectorOnMainThread:` with an
        // `NSUserNotification` (see `post`).
        #[unsafe(method(post:))]
        fn post(&self, note: &NSUserNotification) {
            let Some(center) = center() else {
                return;
            };
            // Remove, then deliver: a fresh banner and sound for every message.
            if let Some(identifier) = note.identifier() {
                remove_delivered(&center, &identifier);
            }
            center.deliverNotification(note);
        }

        // SAFETY: called through `performSelectorOnMainThread:` with an
        // `NSString` (see `MacNotifier::close`).
        #[unsafe(method(withdraw:))]
        fn withdraw(&self, identifier: &NSString) {
            if let Some(center) = center() {
                remove_delivered(&center, identifier);
            }
        }

        // SAFETY: the signature of the undocumented delegate callback for a
        // closed alert (`- (void)userNotificationCenter:(NSUserNotificationCenter *)c
        // didDismissAlert:(NSUserNotification *)n`); never called if absent.
        #[unsafe(method(userNotificationCenter:didDismissAlert:))]
        fn did_dismiss_alert(&self, center: &NSUserNotificationCenter, note: &NSUserNotification) {
            if let Some(id) = our_id(note) {
                native::emit(id, Signal::Closed);
            }
            center.removeDeliveredNotification(note);
        }
    }

    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the methods have the protocol's signatures.
    unsafe impl NSUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:didActivateNotification:))]
        fn did_activate(
            &self,
            center: &NSUserNotificationCenter,
            note: &NSUserNotification,
        ) {
            let Some(id) = our_id(note) else {
                return;
            };
            let additional = note
                .additionalActivationAction()
                .and_then(|a| a.identifier())
                .map(|i| i.to_string());
            let main_key = note
                .userInfo()
                .and_then(|info| info.objectForKey(ns_string!(MAIN_KEY)))
                .and_then(|v| v.downcast::<NSString>().ok())
                .map(|s| s.to_string());
            let activation = match note.activationType() {
                NSUserNotificationActivationType::ContentsClicked => {
                    text::MacActivation::Contents
                }
                NSUserNotificationActivationType::ActionButtonClicked => {
                    text::MacActivation::ActionButton
                }
                NSUserNotificationActivationType::AdditionalActionClicked => {
                    text::MacActivation::Additional(additional.as_deref())
                }
                NSUserNotificationActivationType::Replied => text::MacActivation::Replied,
                _ => text::MacActivation::None,
            };
            if let Some(signal) = text::mac_signal(activation, main_key.as_deref()) {
                center.removeDeliveredNotification(note);
                native::emit(id, signal);
            }
        }

        #[unsafe(method(userNotificationCenter:shouldPresentNotification:))]
        fn should_present(
            &self,
            _center: &NSUserNotificationCenter,
            _note: &NSUserNotification,
        ) -> bool {
            true
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        // SAFETY: NSObject's `init` takes no arguments and returns the object.
        unsafe { msg_send![Self::alloc(), init] }
    }

    /// Runs `selector` (one of ours, taking one object) with `arg` on the
    /// main thread.
    fn on_main(&self, selector: Sel, arg: &AnyObject) {
        // SAFETY: only called with `post:` (an `NSUserNotification`) and
        // `withdraw:` (an `NSString`), which `Delegate` implements; the call
        // retains `self` and `arg` until it ran.
        unsafe {
            self.performSelectorOnMainThread_withObject_waitUntilDone(selector, Some(arg), false)
        }
    }
}

/// Sets up the notification center: removes earlier launches'
/// notifications and installs the delegate. Call on the main thread before
/// the iced application starts.
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
            let delegate = Delegate::new();
            // SAFETY: `delegate` stays alive for the rest of the process (held
            // by `DELEGATE`), as the center does not retain its delegate.
            unsafe { center.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
            Some(delegate)
        })
    });
}

fn delegate() -> Option<&'static Delegate> {
    DELEGATE.get().and_then(|d| d.as_deref())
}

/// Builds `n` as notification `id` and hands it to the main thread.
fn post(delegate: &Delegate, id: u32, n: &Outgoing) {
    let image = n
        .image
        .as_ref()
        .and_then(|(w, h, px)| native::image_file(*w, *h, px));
    autoreleasepool(|_| {
        let note = NSUserNotification::new();
        note.setIdentifier(Some(&NSString::from_str(&text::mac_identifier(&NONCE, id))));
        note.setTitle(Some(&NSString::from_str(&n.title)));
        note.setInformativeText(Some(&NSString::from_str(&n.body)));
        if n.sound {
            // SAFETY: an immutable `NSString` constant of Foundation.
            let sound = unsafe { NSUserNotificationDefaultSoundName };
            note.setSoundName(Some(sound));
        }
        if let Some(path) = image
            && let Some(img) = NSImage::initWithContentsOfFile(
                NSImage::alloc(),
                &NSString::from_str(&path.to_string_lossy()),
            )
        {
            // SAFETY: `contentImage` is an `NSImage` property of
            // `NSUserNotification` (not in the generated bindings).
            let () = unsafe { msg_send![&*note, setContentImage: &*img] };
        }
        match text::mac_buttons(&n.actions) {
            Some(buttons) => {
                note.setHasActionButton(true);
                note.setActionButtonTitle(&NSString::from_str(buttons.main.1));
                let key = NSString::from_str(buttons.main.0);
                let info = NSDictionary::<NSString, AnyObject>::from_slices(
                    &[ns_string!(MAIN_KEY)],
                    &[key.as_ref()],
                );
                // SAFETY: the dictionary maps an `NSString` to an `NSString`,
                // a property-list type as `userInfo` requires.
                unsafe { note.setUserInfo(Some(&info)) };
                if !buttons.more.is_empty() {
                    let actions: Vec<_> = buttons
                        .more
                        .iter()
                        .map(|(k, l)| {
                            NSUserNotificationAction::actionWithIdentifier_title(
                                Some(&NSString::from_str(k)),
                                Some(&NSString::from_str(l)),
                            )
                        })
                        .collect();
                    note.setAdditionalActions(Some(&NSArray::from_retained_slice(&actions)));
                }
            }
            None => note.setHasActionButton(false),
        }
        delegate.on_main(sel!(post:), note.as_ref());
    });
}

/// `NSUserNotification`s.
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
            if delegate().is_none() {
                return Event::Failed("no notification center".into());
            }
            let server_sound = n.sound;
            let posted = tokio::task::spawn_blocking(move || {
                if let Some(d) = delegate() {
                    post(d, id, &n);
                }
            })
            .await;
            match posted {
                Ok(()) => Event::Shown { id, server_sound },
                Err(e) => Event::Failed(e.to_string()),
            }
        }
        .boxed()
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        if let Some(d) = delegate() {
            autoreleasepool(|_| {
                let identifier = NSString::from_str(&text::mac_identifier(&NONCE, id));
                d.on_main(sel!(withdraw:), identifier.as_ref());
            });
        }
        async {}.boxed()
    }

    fn listens(&self) -> bool {
        true
    }
}
