//! macOS Apple Events that winit 0.30 does not expose: `rocketchat://` links
//! (`kInternetEventClass`/`kAEGetURL`) and Dock-icon clicks on the running app
//! (`kCoreEventClass`/`kAEReopenApplication`). They go to [`crate::os_events`].
//!
//! winit's application delegate stays untouched: a separate handler object is
//! registered with the shared `NSAppleEventManager`. NSApplication installs
//! its own handlers while launching, so ours are installed again on
//! `NSApplicationWillFinishLaunchingNotification`, which comes after that and
//! before the event that launched the app is dispatched.
//!
//! The only place in the project with `unsafe` code.
#![allow(unsafe_code)]

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, ns_string,
};

use crate::os_events::{self, OsEvent};

const fn four_cc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}
/// `kInternetEventClass` and `kAEGetURL`.
const GET_URL: u32 = four_cc(b"GURL");
const CORE_EVENT_CLASS: u32 = four_cc(b"aevt");
const REOPEN_APPLICATION: u32 = four_cc(b"rapp");
const DIRECT_OBJECT: u32 = four_cc(b"----");

define_class!(
    // SAFETY: NSObject has no subclassing requirements; `Handler` has no
    // ivars and does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "RustketAppleEventHandler"]
    struct Handler;

    impl Handler {
        // SAFETY: the signature is the one the event manager calls
        // (`- (void)handle:(NSAppleEventDescriptor *)e withReplyEvent:(NSAppleEventDescriptor *)r`).
        #[unsafe(method(handleGetURL:withReplyEvent:))]
        fn get_url(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            let url = event
                .paramDescriptorForKeyword(DIRECT_OBJECT)
                .and_then(|d| d.stringValue());
            if let Some(url) = url {
                os_events::push(OsEvent::Url(url.to_string()));
            }
        }

        // SAFETY: as above.
        #[unsafe(method(handleReopen:withReplyEvent:))]
        fn reopen(&self, _event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            os_events::push(OsEvent::Reopen);
        }

        // SAFETY: notification observers take the notification.
        #[unsafe(method(willFinishLaunching:))]
        fn will_finish_launching(&self, _notification: &NSNotification) {
            self.install_handlers();
        }
    }
);

impl Handler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: NSObject's `init` takes no arguments and returns the object.
        unsafe { msg_send![Self::alloc(mtm), init] }
    }

    fn install_handlers(&self) {
        let manager = NSAppleEventManager::sharedAppleEventManager();
        // SAFETY: `self` implements both selectors with the signature the
        // manager calls, and lives for the rest of the process (see `install`).
        unsafe {
            manager.setEventHandler_andSelector_forEventClass_andEventID(
                self,
                sel!(handleGetURL:withReplyEvent:),
                GET_URL,
                GET_URL,
            );
            manager.setEventHandler_andSelector_forEventClass_andEventID(
                self,
                sel!(handleReopen:withReplyEvent:),
                CORE_EVENT_CLASS,
                REOPEN_APPLICATION,
            );
        }
    }
}

/// Registers the handlers; call on the main thread before the iced
/// application starts.
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("Apple Event handlers must be installed on the main thread");
        return;
    };
    let handler = Handler::new(mtm);
    handler.install_handlers();
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: `handler` implements `willFinishLaunching:` taking the
    // notification, and is never deallocated (leaked below).
    unsafe {
        center.addObserver_selector_name_object(
            &handler,
            sel!(willFinishLaunching:),
            Some(ns_string!("NSApplicationWillFinishLaunchingNotification")),
            None,
        );
    }
    // Neither the event manager nor the notification center retains it.
    std::mem::forget(handler);
}
