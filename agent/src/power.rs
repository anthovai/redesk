//! Keeps the machine and its display awake while someone controls it.
//!
//! macOS: IOPMAssertion (PreventUserIdleDisplaySleep). Windows: a power request, which unlike
//! SetThreadExecutionState is not tied to the calling thread (sessions run on tokio workers).
//! Elsewhere it does nothing.

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

/// Locks the screen of this machine (after a remote session, when `lock_after_session` is on).
/// Best effort: failures are logged.
pub fn lock_screen() {
    if let Err(e) = lock::now() {
        tracing::warn!("could not lock the screen after the session: {e}");
    } else {
        tracing::info!("screen locked after the remote session");
    }
}

#[cfg(target_os = "macos")]
mod lock {
    /// SACLockScreenImmediate (login.framework) locks like the menu bar's "Lock Screen"; it is
    /// private API, so it is looked up at run time, with `pmset displaysleepnow` as fallback
    /// (which locks when the Mac asks for the password right after the display sleeps).
    pub fn now() -> Result<(), String> {
        const LOGIN: &[u8] = b"/System/Library/PrivateFrameworks/login.framework/Versions/Current/login\0";
        // SAFETY: dlopen/dlsym with NUL-terminated strings; the symbol has no arguments and
        // returns an int.
        unsafe {
            let h = libc::dlopen(LOGIN.as_ptr().cast(), libc::RTLD_LAZY);
            if !h.is_null() {
                let f = libc::dlsym(h, c"SACLockScreenImmediate".as_ptr());
                if !f.is_null() {
                    let lock: extern "C" fn() -> i32 = std::mem::transmute(f);
                    if lock() == 0 {
                        return Ok(());
                    }
                }
            }
        }
        let st = std::process::Command::new("/usr/bin/pmset").arg("displaysleepnow").status().map_err(|e| e.to_string())?;
        if st.success() { Ok(()) } else { Err(format!("pmset: {st}")) }
    }
}

#[cfg(windows)]
mod lock {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn LockWorkStation() -> i32;
    }

    pub fn now() -> Result<(), String> {
        // SAFETY: no arguments; fails (0) outside an interactive desktop.
        if unsafe { LockWorkStation() } != 0 { Ok(()) } else { Err(std::io::Error::last_os_error().to_string()) }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod lock {
    pub fn now() -> Result<(), String> {
        let st = std::process::Command::new("loginctl").arg("lock-session").status().map_err(|e| e.to_string())?;
        if st.success() { Ok(()) } else { Err(format!("loginctl: {st}")) }
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

#[cfg(not(any(target_os = "macos", windows)))]
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
        let g = KeepAwake::new("Aegiron test");
        assert!(g.active());
        drop(g);
        let g2 = KeepAwake::new("Aegiron test 2");
        assert!(g2.active());
    }
}
