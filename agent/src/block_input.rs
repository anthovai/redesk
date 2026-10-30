//! Blocks the keyboard and mouse of the person at this machine while a viewer asks for it (and
//! during privacy mode). Input the agent injects for the viewer still goes through.
//!
//! - macOS: a CGEventTap at the HID level drops local events. It needs the Accessibility
//!   permission, which input injection already needs. Events posted by this process (the
//!   viewer's input, injected through enigo) carry its pid and pass.
//! - Windows: low-level keyboard and mouse hooks drop events without the injected flag.
//!   `BlockInput` is not used: it needs administrator rights and would block injected input too.
//!
//! Local pointer movement, button and key releases still pass (nothing gets stuck), and so do
//! clicks on Aegiron's own windows, so the person at the machine can always reach the
//! connection manager and end the session. Ctrl+Alt+Shift+Esc ends every session that blocks
//! the machine ([`crate::controls::escaped`]). Hooks and taps belong to this process: when it
//! exits or crashes the system removes them, so input never stays blocked.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

/// (sessions blocking input, the running blocker)
static STATE: Mutex<(usize, Option<imp::Running>)> = Mutex::new((0, None));
/// The escape combination was pressed: let everything through until the last session ends.
static ESCAPED: AtomicBool = AtomicBool::new(false);
/// Screen rectangles (x, y, w, h in global logical points, top-left origin) of Aegiron windows
/// that local clicks may still reach on macOS. Windows asks the system which window is hit.
static PASSTHROUGH: Mutex<Vec<[f32; 4]>> = Mutex::new(Vec::new());

/// Input stays blocked while at least one of these exists.
pub struct Blocked(());

impl Drop for Blocked {
    fn drop(&mut self) {
        let mut st = STATE.lock().unwrap();
        st.0 = st.0.saturating_sub(1);
        if st.0 == 0 {
            let running = st.1.take();
            drop(st);
            drop(running); // stops the tap/hooks thread and waits for it
            tracing::info!("local input unblocked");
        }
    }
}

pub fn supported() -> bool {
    cfg!(any(target_os = "macos", windows))
}

/// Blocks local input (shared by every session that asks).
pub fn acquire() -> Result<Blocked> {
    let mut st = STATE.lock().unwrap();
    if st.1.is_none() {
        ESCAPED.store(false, Ordering::Relaxed);
        st.1 = Some(imp::start()?);
        tracing::info!("local input blocked ({} to end)", crate::controls::ESCAPE_HINT);
    }
    st.0 += 1;
    Ok(Blocked(()))
}

/// The desktop app publishes where its connection manager is, every frame it is shown.
pub fn set_passthrough(rects: Vec<[f32; 4]>) {
    *PASSTHROUGH.lock().unwrap() = rects;
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn passes(x: f64, y: f64) -> bool {
    let Ok(r) = PASSTHROUGH.try_lock() else { return false };
    inside(&r, x, y)
}

fn inside(rects: &[[f32; 4]], x: f64, y: f64) -> bool {
    rects.iter().any(|r| x >= r[0] as f64 && y >= r[1] as f64 && x < (r[0] + r[2]) as f64 && y < (r[1] + r[3]) as f64)
}

/// A key press completes Ctrl+Alt+Shift+Esc.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn is_escape(ctrl: bool, alt: bool, shift: bool, esc: bool) -> bool {
    ctrl && alt && shift && esc
}

/// Called from the tap/hook: never block there, hand the work to another thread.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn on_escape() {
    if !ESCAPED.swap(true, Ordering::Relaxed) {
        std::thread::spawn(crate::controls::escaped);
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use anyhow::{Result, anyhow};

    use super::{ESCAPED, is_escape, on_escape, passes};

    type Ref = *mut c_void;
    type Callback = extern "C" fn(proxy: *mut c_void, ty: u32, event: Ref, user: *mut c_void) -> Ref;

    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, callback: Callback, user: *mut c_void) -> Ref;
        fn CGEventTapEnable(tap: Ref, enable: bool);
        fn CGEventGetIntegerValueField(event: Ref, field: u32) -> i64;
        fn CGEventGetFlags(event: Ref) -> u64;
        fn CGEventGetLocation(event: Ref) -> CGPoint;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopCommonModes: *const c_void;
        fn CFMachPortCreateRunLoopSource(alloc: *const c_void, port: Ref, order: isize) -> Ref;
        fn CFMachPortInvalidate(port: Ref);
        fn CFRunLoopGetCurrent() -> Ref;
        fn CFRunLoopAddSource(rl: Ref, source: Ref, mode: *const c_void);
        fn CFRunLoopRun();
        fn CFRunLoopStop(rl: Ref);
        fn CFRelease(cf: *const c_void);
    }

    // CGEventType values.
    const LEFT_DOWN: u32 = 1;
    const LEFT_UP: u32 = 2;
    const RIGHT_DOWN: u32 = 3;
    const RIGHT_UP: u32 = 4;
    const MOVED: u32 = 5;
    const LEFT_DRAGGED: u32 = 6;
    const RIGHT_DRAGGED: u32 = 7;
    const KEY_DOWN: u32 = 10;
    const KEY_UP: u32 = 11;
    const FLAGS_CHANGED: u32 = 12;
    const SCROLL: u32 = 22;
    const OTHER_DOWN: u32 = 25;
    const OTHER_UP: u32 = 26;
    const OTHER_DRAGGED: u32 = 27;
    const DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
    const DISABLED_BY_USER: u32 = 0xFFFF_FFFF;
    /// Trackpad gestures (rotate, begin/end, gesture, magnify, swipe, smart magnify) and tablet
    /// events, delivered with these NSEvent type numbers.
    const OTHER_TYPES: [u32; 9] = [18, 19, 20, 23, 24, 29, 30, 31, 32];

    const FIELD_KEYCODE: u32 = 9; // kCGKeyboardEventKeycode
    const FIELD_SOURCE_PID: u32 = 41; // kCGEventSourceUnixProcessID
    const KEY_ESCAPE: i64 = 53;
    const FLAG_SHIFT: u64 = 0x2_0000;
    const FLAG_CONTROL: u64 = 0x4_0000;
    const FLAG_ALT: u64 = 0x8_0000;

    static TAP: AtomicUsize = AtomicUsize::new(0);

    fn mask() -> u64 {
        let base = [LEFT_DOWN, LEFT_UP, RIGHT_DOWN, RIGHT_UP, MOVED, LEFT_DRAGGED, RIGHT_DRAGGED, KEY_DOWN, KEY_UP, FLAGS_CHANGED, SCROLL, OTHER_DOWN, OTHER_UP, OTHER_DRAGGED];
        base.iter().chain(OTHER_TYPES.iter()).fold(0u64, |m, t| m | 1 << t)
    }

    extern "C" fn callback(_proxy: *mut c_void, ty: u32, event: Ref, _user: *mut c_void) -> Ref {
        // SAFETY: `event` is the event being delivered, valid for the duration of the callback.
        unsafe {
            if ty == DISABLED_BY_TIMEOUT || ty == DISABLED_BY_USER {
                // The system turns slow taps off; turn it back on.
                let tap = TAP.load(Ordering::Relaxed);
                if tap != 0 {
                    CGEventTapEnable(tap as Ref, true);
                }
                return event;
            }
            if ESCAPED.load(Ordering::Relaxed) || CGEventGetIntegerValueField(event, FIELD_SOURCE_PID) == std::process::id() as i64 {
                return event;
            }
            match ty {
                MOVED | LEFT_UP | RIGHT_UP | OTHER_UP | KEY_UP => event,
                KEY_DOWN => {
                    let f = CGEventGetFlags(event);
                    let esc = CGEventGetIntegerValueField(event, FIELD_KEYCODE) == KEY_ESCAPE;
                    if is_escape(f & FLAG_CONTROL != 0, f & FLAG_ALT != 0, f & FLAG_SHIFT != 0, esc) {
                        on_escape();
                    }
                    std::ptr::null_mut()
                }
                LEFT_DOWN | RIGHT_DOWN | OTHER_DOWN | LEFT_DRAGGED | RIGHT_DRAGGED | OTHER_DRAGGED | SCROLL => {
                    let p = CGEventGetLocation(event);
                    if passes(p.x, p.y) { event } else { std::ptr::null_mut() }
                }
                _ => std::ptr::null_mut(),
            }
        }
    }

    pub struct Running {
        run_loop: usize,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            let Some(t) = self.thread.take() else { return };
            // A stop sent before the loop runs is lost: repeat until the thread is gone.
            while !t.is_finished() {
                // SAFETY: the run loop belongs to the tap thread, which is still alive.
                unsafe { CFRunLoopStop(self.run_loop as Ref) };
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = t.join();
        }
    }

    pub fn start() -> Result<Running> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<usize, String>>(1);
        let thread = std::thread::Builder::new().name("aegiron-block-input".into()).spawn(move || {
            // SAFETY: CoreFoundation/CoreGraphics calls on this thread's own run loop; every
            // object created here is released before the thread ends.
            unsafe {
                // kCGHIDEventTap, kCGHeadInsertEventTap, kCGEventTapOptionDefault (can drop).
                let tap = CGEventTapCreate(0, 0, 0, mask(), callback, std::ptr::null_mut());
                if tap.is_null() {
                    let _ = tx.send(Err("ต้องอนุญาต Accessibility ให้ Aegiron ก่อน (cannot create the event tap)".into()));
                    return;
                }
                TAP.store(tap as usize, Ordering::Relaxed);
                let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
                let rl = CFRunLoopGetCurrent();
                CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
                CGEventTapEnable(tap, true);
                let _ = tx.send(Ok(rl as usize));
                CFRunLoopRun();
                CGEventTapEnable(tap, false);
                TAP.store(0, Ordering::Relaxed);
                CFMachPortInvalidate(tap);
                CFRelease(source);
                CFRelease(tap);
            }
        })?;
        match rx.recv() {
            Ok(Ok(run_loop)) => Ok(Running { run_loop, thread: Some(thread) }),
            Ok(Err(e)) => Err(anyhow!(e)),
            Err(_) => Err(anyhow!("the input blocker stopped")),
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn mask_covers_input_events_only() {
            let m = super::mask();
            for t in [1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 22, 25, 26, 27, 29, 30] {
                assert!(m & (1 << t) != 0, "type {t}");
            }
            assert_eq!(m & (1 << 0), 0, "null events");
            assert_eq!(m & (1 << 13), 0);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use anyhow::{Result, anyhow};
    use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_SHIFT};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GA_ROOT, GetAncestor, GetMessageW, GetWindowThreadProcessId, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
        LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL,
        WM_KEYDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEMOVE, WM_QUIT, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_XBUTTONUP, WindowFromPoint,
    };

    use super::{ESCAPED, is_escape, on_escape};

    // Modifiers as the hook saw them (blocked keys never reach the system's key state).
    static CTRL: AtomicBool = AtomicBool::new(false);
    static ALT: AtomicBool = AtomicBool::new(false);
    static SHIFT: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn keyboard(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && !ESCAPED.load(Ordering::Relaxed) {
            // SAFETY: for HC_ACTION, lParam points to a KBDLLHOOKSTRUCT.
            let k = unsafe { &*(l.0 as *const KBDLLHOOKSTRUCT) };
            if k.flags.0 & LLKHF_INJECTED.0 == 0 {
                let down = matches!(w.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                let vk = k.vkCode as u16;
                for (keys, flag) in [([VK_CONTROL, VK_LCONTROL, VK_RCONTROL], &CTRL), ([VK_MENU, VK_LMENU, VK_RMENU], &ALT), ([VK_SHIFT, VK_LSHIFT, VK_RSHIFT], &SHIFT)] {
                    if keys.iter().any(|v| v.0 == vk) {
                        flag.store(down, Ordering::Relaxed);
                    }
                }
                if down && is_escape(CTRL.load(Ordering::Relaxed), ALT.load(Ordering::Relaxed), SHIFT.load(Ordering::Relaxed), vk == VK_ESCAPE.0) {
                    on_escape();
                }
                if down {
                    return LRESULT(1); // releases pass, so nothing stays pressed
                }
            }
        }
        // SAFETY: forwarding the unchanged hook arguments.
        unsafe { CallNextHookEx(None, code, w, l) }
    }

    unsafe extern "system" fn mouse(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && !ESCAPED.load(Ordering::Relaxed) {
            // SAFETY: for HC_ACTION, lParam points to an MSLLHOOKSTRUCT.
            let m = unsafe { &*(l.0 as *const MSLLHOOKSTRUCT) };
            let passes = matches!(w.0 as u32, WM_MOUSEMOVE | WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP) || own_window(m.pt);
            if m.flags & LLMHF_INJECTED == 0 && !passes {
                return LRESULT(1);
            }
        }
        // SAFETY: forwarding the unchanged hook arguments.
        unsafe { CallNextHookEx(None, code, w, l) }
    }

    /// The pointer is over one of Aegiron's windows (the connection manager).
    fn own_window(pt: POINT) -> bool {
        // SAFETY: plain window queries.
        unsafe {
            let h = WindowFromPoint(pt);
            if h.is_invalid() {
                return false;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(GetAncestor(h, GA_ROOT), Some(&mut pid));
            pid == GetCurrentProcessId()
        }
    }

    pub struct Running {
        thread_id: u32,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            // SAFETY: posting WM_QUIT to the hook thread's queue.
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    pub fn start() -> Result<Running> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<u32, String>>(1);
        let thread = std::thread::Builder::new().name("aegiron-block-input".into()).spawn(move || {
            // SAFETY: hooks are installed and removed on this thread, which pumps messages
            // (low-level hooks are called on the installing thread's message loop).
            unsafe {
                let module = GetModuleHandleW(None).ok().map(|m| m.into());
                let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0);
                let ms = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), module, 0);
                let (kb, ms) = match (kb, ms) {
                    (Ok(kb), Ok(ms)) => (kb, ms),
                    (kb, ms) => {
                        let err = format!("cannot install input hooks: {:?}", kb.as_ref().err().or(ms.as_ref().err()));
                        for h in [kb, ms].into_iter().flatten() {
                            let _ = UnhookWindowsHookEx(h);
                        }
                        let _ = tx.send(Err(err));
                        return;
                    }
                };
                for f in [&CTRL, &ALT, &SHIFT] {
                    f.store(false, Ordering::Relaxed);
                }
                let _ = tx.send(Ok(GetCurrentThreadId()));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let _ = UnhookWindowsHookEx(kb);
                let _ = UnhookWindowsHookEx(ms);
            }
        })?;
        match rx.recv() {
            Ok(Ok(thread_id)) => Ok(Running { thread_id, thread: Some(thread) }),
            Ok(Err(e)) => Err(anyhow!(e)),
            Err(_) => Err(anyhow!("the input blocker stopped")),
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use anyhow::{Result, bail};

    pub struct Running;

    pub fn start() -> Result<Running> {
        bail!("blocking local input is not supported on this system")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_needs_all_three_modifiers() {
        assert!(is_escape(true, true, true, true));
        assert!(!is_escape(true, true, false, true));
        assert!(!is_escape(true, false, true, true));
        assert!(!is_escape(false, true, true, true));
        assert!(!is_escape(true, true, true, false));
    }

    #[test]
    fn clicks_pass_only_inside_published_windows() {
        let cm = [[1000.0, 200.0, 320.0, 580.0]];
        assert!(inside(&cm, 1000.0, 200.0));
        assert!(inside(&cm, 1319.5, 779.0));
        assert!(!inside(&cm, 1320.0, 300.0));
        assert!(!inside(&cm, 999.0, 300.0));
        assert!(!inside(&[], 0.0, 0.0));
    }
}
