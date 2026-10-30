//! Privacy mode: black windows over every display hide the screen from the people in front of
//! it, while the viewer still sees what is under them. The session also blocks local input
//! (see `controls`), so nobody works blind on a black screen.
//!
//! - Windows (10 2004 and later): topmost black windows with
//!   `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`, which leaves them out of every screen
//!   capture (GDI and Windows Graphics Capture alike), so the stream shows the desktop. On older
//!   versions the call fails and privacy mode reports an error instead of streaming black.
//! - macOS (desktop app only): borderless black windows at the shielding level, created on the
//!   app's main thread. Capture leaves them out explicitly: while they are shown the agent
//!   captures with `CGWindowListCreateImageFromArray` over every on-screen window except these
//!   (see `capture`). `sharingType = none` is not enough, since macOS 15 ignores it for
//!   ScreenCaptureKit and it only hides a window from other processes. The headless
//!   `aegiron-agent` has no AppKit main loop, so it does not offer privacy mode on macOS.
//!
//! The windows belong to this process and disappear with it.

use std::sync::Mutex;

use anyhow::Result;

/// (sessions using privacy mode, the windows)
static STATE: Mutex<(usize, Option<imp::Shown>)> = Mutex::new((0, None));

/// Text on the black screen for the person at the machine.
fn notice() -> String {
    format!("หน้าจอนี้ถูกซ่อนระหว่างการควบคุมจากระยะไกล (Aegiron)\nกด {} เพื่อยกเลิก", crate::controls::ESCAPE_HINT)
}

/// The screen stays blacked out while at least one of these exists.
pub struct Private(());

impl Drop for Private {
    fn drop(&mut self) {
        let mut st = STATE.lock().unwrap();
        st.0 = st.0.saturating_sub(1);
        if st.0 == 0 {
            let shown = st.1.take();
            drop(st);
            drop(shown);
            tracing::info!("privacy mode off");
        }
    }
}

/// `ui`: the desktop app runs this process (macOS needs its main thread).
pub fn supported(ui: bool) -> bool {
    if cfg!(target_os = "macos") { ui } else { cfg!(windows) }
}

pub fn show() -> Result<Private> {
    let mut st = STATE.lock().unwrap();
    if st.1.is_none() {
        st.1 = Some(imp::show()?);
        tracing::info!("privacy mode on");
    }
    st.0 += 1;
    Ok(Private(()))
}

/// macOS: window numbers the capture must leave out (empty when privacy mode is off).
#[cfg(target_os = "macos")]
pub fn excluded_windows() -> Vec<u32> {
    imp::NUMBERS.lock().unwrap().clone()
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CString, c_void};
    use std::sync::Mutex;
    use std::time::Duration;

    use anyhow::{Result, anyhow, bail};
    use objc2::encode::{Encode, Encoding};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::msg_send;

    /// Window numbers of the black windows, for capture.
    pub static NUMBERS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    /// The windows themselves (retained NSWindow pointers), only touched on the main thread.
    static WINDOWS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct Size {
        w: f64,
        h: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct Rect {
        origin: Point,
        size: Size,
    }
    unsafe impl Encode for Point {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for Size {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for Rect {
        const ENCODING: Encoding = Encoding::Struct("CGRect", &[Point::ENCODING, Size::ENCODING]);
    }

    #[link(name = "System")]
    unsafe extern "C" {
        static _dispatch_main_q: c_void;
        fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
        fn pthread_main_np() -> i32;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGShieldingWindowLevel() -> i32;
    }

    type Job = Box<dyn FnOnce() + Send>;

    extern "C" fn trampoline(ctx: *mut c_void) {
        // SAFETY: ctx is the Box<Job> leaked by `on_main`, consumed exactly once.
        let job = unsafe { Box::from_raw(ctx as *mut Job) };
        job();
    }

    /// Runs `f` on the main thread (the app's AppKit loop) and waits for it.
    fn on_main(f: impl FnOnce() -> Result<()> + Send + 'static) -> Result<()> {
        // SAFETY: pthread_main_np has no preconditions.
        if unsafe { pthread_main_np() } == 1 {
            return f();
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let job: Job = Box::new(move || {
            let _ = tx.send(f());
        });
        // SAFETY: the main queue outlives the process; the context is a leaked Box<Job>.
        unsafe { dispatch_async_f(&raw const _dispatch_main_q, Box::into_raw(Box::new(job)) as *mut c_void, trampoline) };
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| anyhow!("the app's main thread did not respond"))?
    }

    fn class(name: &std::ffi::CStr) -> Result<&'static AnyClass> {
        AnyClass::get(name).ok_or_else(|| anyhow!("{name:?} missing"))
    }

    pub struct Shown;

    impl Drop for Shown {
        fn drop(&mut self) {
            if let Err(e) = on_main(hide) {
                tracing::error!("cannot remove the privacy windows: {e:#}");
            }
        }
    }

    pub fn show() -> Result<Shown> {
        on_main(|| {
            // SAFETY: AppKit calls on the main thread with valid receivers.
            let r = unsafe { create() };
            if r.is_err() {
                hide()?;
            }
            r
        })?;
        Ok(Shown)
    }

    unsafe fn create() -> Result<()> {
        unsafe {
            let screens: *mut AnyObject = msg_send![class(c"NSScreen")?, screens];
            let n: usize = if screens.is_null() { 0 } else { msg_send![screens, count] };
            if n == 0 {
                bail!("no display");
            }
            let black: *mut AnyObject = msg_send![class(c"NSColor")?, blackColor];
            let white: *mut AnyObject = msg_send![class(c"NSColor")?, whiteColor];
            let text = CString::new(super::notice()).unwrap_or_default();
            for i in 0..n {
                let screen: *mut AnyObject = msg_send![screens, objectAtIndex: i];
                let frame: Rect = msg_send![screen, frame];
                // Borderless, buffered, created now so it has a window number before it shows.
                let win: Option<Retained<AnyObject>> = msg_send![
                    msg_send![class(c"NSWindow")?, alloc],
                    initWithContentRect: frame,
                    styleMask: 0usize,
                    backing: 2usize,
                    defer: Bool::NO
                ];
                let win = win.ok_or_else(|| anyhow!("cannot create the privacy window"))?;
                let _: () = msg_send![&*win, setReleasedWhenClosed: Bool::NO];
                let _: () = msg_send![&*win, setBackgroundColor: black];
                let _: () = msg_send![&*win, setOpaque: Bool::YES];
                let _: () = msg_send![&*win, setHasShadow: Bool::NO];
                let _: () = msg_send![&*win, setLevel: CGShieldingWindowLevel() as isize];
                // All spaces, stationary, out of the window cycle, over full-screen apps.
                let _: () = msg_send![&*win, setCollectionBehavior: (1usize | 16 | 64 | 256)];
                let _: () = msg_send![&*win, setSharingType: 0usize];
                // Clicks land on the black window, not on what is hidden under it.
                let _: () = msg_send![&*win, setIgnoresMouseEvents: Bool::NO];
                let _: () = msg_send![&*win, setFrame: frame, display: Bool::NO];

                let s: *mut AnyObject = msg_send![class(c"NSString")?, stringWithUTF8String: text.as_ptr()];
                let label: *mut AnyObject = msg_send![class(c"NSTextField")?, labelWithString: s];
                if !label.is_null() {
                    let font: *mut AnyObject = msg_send![class(c"NSFont")?, systemFontOfSize: 18.0f64];
                    let _: () = msg_send![label, setFont: font];
                    let _: () = msg_send![label, setTextColor: white];
                    let _: () = msg_send![label, setAlignment: 2isize]; // NSTextAlignmentCenter
                    let _: () = msg_send![label, sizeToFit];
                    let size: Rect = msg_send![label, frame];
                    let at = Point { x: (frame.size.w - size.size.w) / 2.0, y: (frame.size.h - size.size.h) / 2.0 };
                    let _: () = msg_send![label, setFrameOrigin: at];
                    let view: *mut AnyObject = msg_send![&*win, contentView];
                    let _: () = msg_send![view, addSubview: label];
                }

                let mut number: isize = msg_send![&*win, windowNumber];
                if number > 0 {
                    // Left out of the capture before it can appear in a frame.
                    NUMBERS.lock().unwrap().push(number as u32);
                }
                let _: () = msg_send![&*win, orderFrontRegardless];
                if number <= 0 {
                    number = msg_send![&*win, windowNumber];
                    NUMBERS.lock().unwrap().push(number as u32);
                }
                WINDOWS.lock().unwrap().push(Retained::into_raw(win) as usize);
            }
        }
        Ok(())
    }

    fn hide() -> Result<()> {
        let windows: Vec<usize> = std::mem::take(&mut *WINDOWS.lock().unwrap());
        for w in windows {
            // SAFETY: each pointer came from Retained::into_raw in `create`, on this thread.
            unsafe {
                let win = Retained::from_raw(w as *mut AnyObject).expect("window");
                let _: () = msg_send![&*win, orderOut: std::ptr::null_mut::<AnyObject>()];
                let _: () = msg_send![&*win, close];
            }
        }
        // Only once they are off screen may capture include everything again.
        NUMBERS.lock().unwrap().clear();
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use std::thread::JoinHandle;

    use anyhow::{Result, anyhow};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, DT_CENTER, DT_WORDBREAK, DrawTextW, EndPaint, EnumDisplayMonitors, FillRect, GetStockObject, HBRUSH, HDC, HMONITOR, PAINTSTRUCT,
        SetBkMode, SetTextColor, BLACK_BRUSH, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW, HWND_TOPMOST, MA_NOACTIVATE, MSG,
        PostThreadMessageW, RegisterClassW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetTimer, SetWindowDisplayAffinity, SetWindowPos,
        ShowWindow, TranslateMessage, WDA_EXCLUDEFROMCAPTURE, WINDOW_EX_STYLE, WM_MOUSEACTIVATE, WM_PAINT, WM_QUIT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };
    use windows::core::{BOOL, PCWSTR, w};

    pub struct Shown {
        thread_id: u32,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Shown {
        fn drop(&mut self) {
            // SAFETY: posting WM_QUIT to the window thread's queue.
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        // SAFETY: standard window procedure work on our own window.
        unsafe {
            match msg {
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    let dc = BeginPaint(hwnd, &mut ps);
                    let mut r = RECT::default();
                    let _ = GetClientRect(hwnd, &mut r);
                    FillRect(dc, &r, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(dc, COLORREF(0x00FF_FFFF));
                    let mut text: Vec<u16> = super::notice().encode_utf16().collect();
                    r.top = r.bottom / 2 - 24;
                    DrawTextW(dc, &mut text, &mut r, DT_CENTER | DT_WORDBREAK);
                    let _ = EndPaint(hwnd, &ps);
                    LRESULT(0)
                }
                // Keep the black windows above other topmost windows that appear later.
                WM_TIMER => {
                    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                    LRESULT(0)
                }
                WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
                _ => DefWindowProcW(hwnd, msg, w, l),
            }
        }
    }

    unsafe extern "system" fn monitor(_m: HMONITOR, _dc: HDC, rect: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: data is the &mut Vec<RECT> passed to EnumDisplayMonitors; rect is valid.
        unsafe { (*(data.0 as *mut Vec<RECT>)).push(*rect) };
        BOOL(1)
    }

    pub fn show() -> Result<Shown> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<u32, String>>(1);
        let thread = std::thread::Builder::new().name("aegiron-privacy".into()).spawn(move || {
            // SAFETY: windows are created, pumped and destroyed on this thread.
            unsafe {
                let instance = GetModuleHandleW(None).map(|m| m.into()).unwrap_or_default();
                let class = w!("AegironPrivacy");
                let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance, lpszClassName: class, ..Default::default() };
                RegisterClassW(&wc); // fails harmlessly when already registered
                let mut rects: Vec<RECT> = Vec::new();
                let _ = EnumDisplayMonitors(None, None, Some(monitor), LPARAM(&mut rects as *mut Vec<RECT> as isize));
                let mut windows = Vec::new();
                let mut err = None;
                for r in &rects {
                    let ex = WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0);
                    let hwnd = match CreateWindowExW(ex, class, PCWSTR::null(), WS_POPUP, r.left, r.top, r.right - r.left, r.bottom - r.top, None, None, Some(instance), None) {
                        Ok(h) => h,
                        Err(e) => {
                            err = Some(format!("cannot create the privacy window: {e}"));
                            break;
                        }
                    };
                    windows.push(hwnd);
                    // Must succeed before the window shows, or the viewer would see black.
                    if let Err(e) = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) {
                        err = Some(format!("privacy mode needs Windows 10 version 2004 or later ({e})"));
                        break;
                    }
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    SetTimer(Some(hwnd), 1, 500, None);
                }
                if rects.is_empty() {
                    err = Some("no display".into());
                }
                if let Some(e) = err {
                    for h in windows {
                        let _ = DestroyWindow(h);
                    }
                    let _ = tx.send(Err(e));
                    return;
                }
                let _ = tx.send(Ok(GetCurrentThreadId()));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                for h in windows {
                    let _ = DestroyWindow(h);
                }
            }
        })?;
        match rx.recv() {
            Ok(Ok(thread_id)) => Ok(Shown { thread_id, thread: Some(thread) }),
            Ok(Err(e)) => Err(anyhow!(e)),
            Err(_) => Err(anyhow!("the privacy windows stopped")),
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use anyhow::{Result, bail};

    pub struct Shown;

    pub fn show() -> Result<Shown> {
        bail!("privacy mode is not supported on this system")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn supported_where_it_can_hide_from_capture() {
        assert!(!super::supported(false) || cfg!(windows));
        assert_eq!(super::supported(true), cfg!(any(target_os = "macos", windows)));
        assert!(super::notice().contains(crate::controls::ESCAPE_HINT));
    }
}
