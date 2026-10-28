//! macOS: `aegiron://` links delivered as Apple Events.
//!
//! Clicking a link (or `open aegiron://connect/<id>`) does not start a new process when the
//! app is running, and does not pass the URL as an argument when it is not: LaunchServices
//! sends a `kAEGetURL` ('GURL'/'GURL') Apple Event to the app. winit/eframe do not expose it, so
//! this installs a handler with `NSAppleEventManager`, which works alongside winit (winit's
//! application delegate does not implement `application:openURLs:`).
//!
//! Timing matters for the link that launches the app: AppKit installs its own default handlers
//! in `finishLaunching` (replacing one set earlier) and then dispatches the launch event before
//! `applicationDidFinishLaunching`. Apple's guidance is to install handlers in
//! `applicationWillFinishLaunching`, so the handler also observes
//! `NSApplicationWillFinishLaunchingNotification` and installs itself again there.
//!
//! The URL string goes through [`crate::local::parse_link`]; only a valid 9-digit ID ever
//! reaches the app, which then asks the user before connecting.

use std::ffi::{CStr, c_char};
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, Sel};
use objc2::{AllocAnyThread, define_class, msg_send, sel};

/// 'GURL': kInternetEventClass and kAEGetURL share this four-char code.
const K_AE_GET_URL: u32 = u32::from_be_bytes(*b"GURL");
/// '----': keyDirectObject, the URL string.
const KEY_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
/// URLs longer than this are dropped without parsing.
const MAX_URL: usize = 256;

type Callback = Box<dyn Fn(Result<String, String>) + Send + Sync>;
static CALLBACK: OnceLock<Callback> = OnceLock::new();
/// The handler object (NSAppleEventManager does not retain it).
static HANDLER: OnceLock<usize> = OnceLock::new();

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "AegironURLEventHandler"]
    struct UrlHandler;

    impl UrlHandler {
        #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
        fn handle_get_url(&self, event: Option<&AnyObject>, _reply: Option<&AnyObject>) {
            let url = event.and_then(event_url);
            dispatch(url);
        }

        #[unsafe(method(applicationWillFinishLaunching:))]
        fn will_finish_launching(&self, _note: Option<&AnyObject>) {
            set_handler(self as *const Self as *mut AnyObject);
        }
    }
);

/// The direct-object string of a GURL event.
fn event_url(event: &AnyObject) -> Option<String> {
    unsafe {
        let desc: Option<Retained<AnyObject>> = msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT];
        let s: Option<Retained<AnyObject>> = msg_send![&*desc?, stringValue];
        let p: *const c_char = msg_send![&*s?, UTF8String];
        (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
    }
}

/// Validates the URL and hands the result to the app: Ok(device id) or Err(short, sanitized
/// copy of the rejected input for the log).
fn dispatch(url: Option<String>) {
    let result = match url {
        Some(u) if u.len() <= MAX_URL => crate::local::parse_link(&u).ok_or(u),
        Some(u) => Err(format!("{}… ({} bytes)", u.chars().take(40).collect::<String>(), u.len())),
        None => Err(String::new()),
    };
    let result = result.map_err(|u| u.chars().take(80).filter(|c| !c.is_control()).collect());
    if let Some(cb) = CALLBACK.get() {
        cb(result);
    }
}

/// Installs (or re-installs) the GURL handler. Call before the event loop starts (this also
/// arranges a re-install at `applicationWillFinishLaunching`, so a link that launched the app
/// is not missed), and optionally again once the app is running. `on_link` is set by the first
/// call only.
pub fn install(on_link: impl Fn(Result<String, String>) + Send + Sync + 'static) {
    let _ = CALLBACK.set(Box::new(on_link));
    let mut first = false;
    let handler = *HANDLER.get_or_init(|| {
        first = true;
        let h: Retained<UrlHandler> = unsafe { msg_send![super(UrlHandler::alloc().set_ivars(())), init] };
        Retained::into_raw(h) as usize // lives for the whole process
    }) as *mut AnyObject;
    if first {
        observe_will_finish_launching(handler);
    }
    set_handler(handler);
}

/// Registers `handler` for GURL events with the shared `NSAppleEventManager`.
fn set_handler(handler: *mut AnyObject) {
    let Some(cls) = AnyClass::get(c"NSAppleEventManager") else {
        tracing::warn!("NSAppleEventManager unavailable: aegiron:// links only work at launch");
        return;
    };
    let selector: Sel = sel!(handleGetURLEvent:withReplyEvent:);
    unsafe {
        let mgr: Option<Retained<AnyObject>> = msg_send![cls, sharedAppleEventManager];
        let Some(mgr) = mgr else { return };
        let _: () = msg_send![&*mgr, setEventHandler: handler, andSelector: selector, forEventClass: K_AE_GET_URL, andEventID: K_AE_GET_URL];
    }
}

/// Calls `applicationWillFinishLaunching:` on `handler` when AppKit posts that notification.
fn observe_will_finish_launching(handler: *mut AnyObject) {
    let (Some(center_cls), Some(string_cls)) = (AnyClass::get(c"NSNotificationCenter"), AnyClass::get(c"NSString")) else {
        return;
    };
    unsafe {
        let center: Option<Retained<AnyObject>> = msg_send![center_cls, defaultCenter];
        let name: Option<Retained<AnyObject>> =
            msg_send![string_cls, stringWithUTF8String: c"NSApplicationWillFinishLaunchingNotification".as_ptr()];
        let (Some(center), Some(name)) = (center, name) else { return };
        let none: *const AnyObject = std::ptr::null();
        let _: () = msg_send![&*center, addObserver: handler, selector: sel!(applicationWillFinishLaunching:), name: &*name, object: none];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn handler_validates_links() {
        static GOT: Mutex<Vec<Result<String, String>>> = Mutex::new(Vec::new());
        install(|r| GOT.lock().unwrap().push(r));
        dispatch(Some("aegiron://connect/123456789".into()));
        dispatch(Some("aegiron://connect/123456789?password=x".into()));
        dispatch(Some(format!("aegiron://connect/{}", "1".repeat(500))));
        dispatch(None);
        let got = GOT.lock().unwrap().clone();
        assert_eq!(got[0], Ok("123456789".into()));
        assert!(got[1].is_err() && got[2].is_err() && got[3].is_err());
        assert!(got[2].as_ref().unwrap_err().len() < 100);
    }
}
