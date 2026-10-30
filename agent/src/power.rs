//! Keeps the machine and its display awake while someone controls it.
//!
//! macOS: IOPMAssertion (PreventUserIdleDisplaySleep). Windows: a power request, which unlike
//! SetThreadExecutionState is not tied to the calling thread (sessions run on tokio workers).
//! Linux: a systemd-logind "sleep" inhibitor and the desktop's org.freedesktop.ScreenSaver
//! inhibition (screen blanking), whichever is available. Elsewhere it does nothing.

/// Held for the lifetime of a session; dropping it lets the machine sleep again.
pub struct KeepAwake {
    handle: imp::Handle,
}

impl KeepAwake {
    pub fn new(reason: &str) -> Self {
        let handle = imp::acquire(reason);
        if handle.is_none() {
            tracing::warn!("could not prevent sleep during the session");
        }
        Self { handle }
    }

    /// Whether the OS accepted the request.
    pub fn active(&self) -> bool {
        self.handle.is_some()
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        imp::release(&mut self.handle);
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CString, c_char, c_void};

    pub type Handle = Option<u32>;
    type CFStringRef = *const c_void;

    const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8
    const LEVEL_ON: u32 = 255; // kIOPMAssertionLevelOn

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, encoding: u32) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(kind: CFStringRef, level: u32, name: CFStringRef, id: *mut u32) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    fn cfstr(s: &str) -> CFStringRef {
        let c = CString::new(s.replace('\0', "")).unwrap_or_default();
        // SAFETY: c is a valid NUL-terminated string for the duration of the call.
        unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) }
    }

    pub fn acquire(reason: &str) -> Handle {
        let (kind, name) = (cfstr("PreventUserIdleDisplaySleep"), cfstr(reason));
        let mut id = 0u32;
        // SAFETY: both strings are valid CFStrings (or null, which the call rejects).
        let rc = unsafe { IOPMAssertionCreateWithName(kind, LEVEL_ON, name, &mut id) };
        for s in [kind, name] {
            if !s.is_null() {
                unsafe { CFRelease(s) };
            }
        }
        (rc == 0).then_some(id)
    }

    pub fn release(h: &mut Handle) {
        if let Some(id) = h.take() {
            unsafe { IOPMAssertionRelease(id) };
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    /// Power request HANDLE, stored as an integer so the guard stays Send.
    pub type Handle = Option<isize>;

    const POWER_REQUEST_CONTEXT_VERSION: u32 = 0;
    const POWER_REQUEST_CONTEXT_SIMPLE_STRING: u32 = 0x1;
    const POWER_REQUEST_DISPLAY_REQUIRED: i32 = 0;
    const POWER_REQUEST_SYSTEM_REQUIRED: i32 = 1;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[repr(C)]
    struct ReasonContext {
        version: u32,
        flags: u32,
        // Union of SimpleReasonString and the (larger) Detailed struct.
        reason: [usize; 3],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn PowerCreateRequest(context: *const ReasonContext) -> *mut c_void;
        fn PowerSetRequest(request: *mut c_void, kind: i32) -> i32;
        fn PowerClearRequest(request: *mut c_void, kind: i32) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
    }

    pub fn acquire(reason: &str) -> Handle {
        let mut wide: Vec<u16> = reason.encode_utf16().filter(|&c| c != 0).chain([0]).collect();
        let ctx = ReasonContext {
            version: POWER_REQUEST_CONTEXT_VERSION,
            flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            reason: [wide.as_mut_ptr() as usize, 0, 0],
        };
        // SAFETY: ctx and the string it points to outlive the call.
        unsafe {
            let h = PowerCreateRequest(&ctx);
            if h.is_null() || h as isize == INVALID_HANDLE_VALUE {
                return None;
            }
            let ok = PowerSetRequest(h, POWER_REQUEST_SYSTEM_REQUIRED) != 0;
            PowerSetRequest(h, POWER_REQUEST_DISPLAY_REQUIRED);
            if !ok {
                CloseHandle(h);
                return None;
            }
            Some(h as isize)
        }
    }

    pub fn release(h: &mut Handle) {
        if let Some(h) = h.take() {
            let h = h as *mut c_void;
            // SAFETY: h came from PowerCreateRequest and is closed exactly once.
            unsafe {
                PowerClearRequest(h, POWER_REQUEST_DISPLAY_REQUIRED);
                PowerClearRequest(h, POWER_REQUEST_SYSTEM_REQUIRED);
                CloseHandle(h);
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::os::fd::OwnedFd;
    use zbus::blocking::{Connection, Proxy};

    pub struct Inhibit {
        /// logind closes the inhibitor when this descriptor is closed.
        _sleep: Option<OwnedFd>,
        /// ScreenSaver cookie; also released if the connection closes.
        screen: Option<(Connection, u32)>,
    }
    pub type Handle = Option<Inhibit>;

    fn sleep(reason: &str) -> zbus::Result<OwnedFd> {
        let conn = Connection::system()?;
        let p = Proxy::new(&conn, "org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager")?;
        let fd: zbus::zvariant::OwnedFd = p.call("Inhibit", &("sleep:idle", "Aegiron", reason, "block"))?;
        Ok(fd.into())
    }

    fn screen(reason: &str) -> zbus::Result<(Connection, u32)> {
        let conn = Connection::session()?;
        let p = Proxy::new(&conn, "org.freedesktop.ScreenSaver", "/org/freedesktop/ScreenSaver", "org.freedesktop.ScreenSaver")?;
        let cookie: u32 = p.call("Inhibit", &("Aegiron", reason))?;
        Ok((conn, cookie))
    }

    pub fn acquire(reason: &str) -> Handle {
        let s = sleep(reason).map_err(|e| tracing::debug!("logind inhibit: {e}")).ok();
        let d = screen(reason).map_err(|e| tracing::debug!("screensaver inhibit: {e}")).ok();
        (s.is_some() || d.is_some()).then_some(Inhibit { _sleep: s, screen: d })
    }

    pub fn release(h: &mut Handle) {
        if let Some((conn, cookie)) = h.take().and_then(|i| i.screen) {
            if let Ok(p) = Proxy::new(&conn, "org.freedesktop.ScreenSaver", "/org/freedesktop/ScreenSaver", "org.freedesktop.ScreenSaver") {
                let _ = p.call_method("UnInhibit", &(cookie,));
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod imp {
    pub type Handle = Option<()>;
    pub fn acquire(_: &str) -> Handle {
        Some(())
    }
    pub fn release(h: &mut Handle) {
        h.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn guard_is_send_and_releases() {
        assert_send::<KeepAwake>(); // held across .await in session tasks
        // Linux test machines (containers, CI) often have neither logind nor a desktop session.
        let required = !cfg!(target_os = "linux");
        let g = KeepAwake::new("Aegiron test");
        assert!(g.active() || !required);
        drop(g);
        let g2 = KeepAwake::new("Aegiron test 2");
        assert!(g2.active() || !required);
    }
}
