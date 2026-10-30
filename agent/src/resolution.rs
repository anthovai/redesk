//! Changing the display resolution from the viewer (`res_list` / `res_set`, feature
//! "resolution"). macOS uses CoreGraphics display modes, Windows `ChangeDisplaySettingsExW`
//! with `CDS_FULLSCREEN` (nothing is written to the registry). Every display a session changed
//! gets its original mode back when the session ends ([`Changer`] restores on drop).
//!
//! `AEGIRON_TEST_RESOLUTION=fake` replaces the platform with an in-memory list of modes, so
//! tests never touch the real screen.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};

/// A display mode: size in the display's own units (points on macOS, pixels elsewhere) and
/// refresh rate (0 = unknown).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Mode {
    pub w: u32,
    pub h: u32,
    pub hz: u32,
}

pub trait Backend: Send {
    /// Modes this display can switch to, largest first, without duplicates.
    fn modes(&self, display: usize) -> Result<Vec<Mode>>;
    fn current(&self, display: usize) -> Result<Mode>;
    fn set(&mut self, display: usize, mode: Mode) -> Result<()>;
    /// Puts back the mode the display had before the session changed it.
    fn restore(&mut self, display: usize, original: Mode) -> Result<()> {
        self.set(display, original)
    }
}

fn fake() -> bool {
    std::env::var("AEGIRON_TEST_RESOLUTION").is_ok_and(|v| v == "fake")
}

/// Whether this machine can change resolutions at all.
pub fn supported() -> bool {
    fake() || cfg!(any(target_os = "macos", windows))
}

/// The backend for this machine (None where resolution changes are not supported).
pub fn backend() -> Option<Box<dyn Backend>> {
    if fake() {
        return Some(Box::new(Fake::shared()));
    }
    #[cfg(target_os = "macos")]
    return Some(Box::new(mac::Mac));
    #[cfg(windows)]
    return Some(Box::new(win::Win));
    #[allow(unreachable_code)]
    None
}

/// Sorts largest first (then highest refresh rate) and drops duplicates.
fn tidy(mut modes: Vec<Mode>) -> Vec<Mode> {
    modes.sort_by(|a, b| (b.w * b.h, b.w, b.hz).cmp(&(a.w * a.h, a.w, a.hz)));
    modes.dedup();
    modes
}

/// One session's resolution changes. Restores every changed display when dropped.
pub struct Changer {
    backend: Box<dyn Backend>,
    original: HashMap<usize, Mode>,
}

impl Changer {
    pub fn new(backend: Box<dyn Backend>) -> Self {
        Self { backend, original: HashMap::new() }
    }

    /// Reply to `res_list`.
    pub fn list(&self, display: usize) -> Value {
        let current = match self.backend.current(display) {
            Ok(m) => m,
            Err(e) => return json!({"t": "res_error", "display": display, "message": format!("{e:#}")}),
        };
        let modes = self.backend.modes(display).unwrap_or_default();
        let original = self.original.get(&display).copied().unwrap_or(current);
        json!({"t": "res_list", "display": display, "current": current, "original": original, "modes": modes})
    }

    /// Switches `display` to the listed mode `w`×`h` (at `hz` when given, else the fastest).
    pub fn set(&mut self, display: usize, w: u32, h: u32, hz: Option<u32>) -> Result<Mode> {
        let modes = self.backend.modes(display)?;
        let mode = modes
            .iter()
            .filter(|m| m.w == w && m.h == h && hz.is_none_or(|hz| hz == 0 || m.hz == hz))
            .max_by_key(|m| m.hz)
            .copied()
            .ok_or_else(|| anyhow!("{w}×{h} is not a mode of this display"))?;
        let current = self.backend.current(display)?;
        if current == mode {
            return Ok(mode);
        }
        self.original.entry(display).or_insert(current);
        self.backend.set(display, mode)?;
        let n = display;
        tracing::info!("display {n}: resolution {}×{} @{} Hz", mode.w, mode.h, mode.hz);
        Ok(mode)
    }

    /// Puts the display's original mode back (no-op if this session never changed it).
    pub fn restore(&mut self, display: usize) -> Result<Option<Mode>> {
        let Some(m) = self.original.remove(&display) else { return Ok(None) };
        self.backend.restore(display, m)?;
        let n = display;
        tracing::info!("display {n}: resolution restored to {}×{}", m.w, m.h);
        Ok(Some(m))
    }

    pub fn changed(&self) -> bool {
        !self.original.is_empty()
    }
}

impl Drop for Changer {
    fn drop(&mut self) {
        for n in self.original.keys().copied().collect::<Vec<_>>() {
            if let Err(e) = self.restore(n) {
                tracing::warn!("display {n}: cannot restore the resolution: {e:#}");
            }
        }
    }
}

/// Test backend: two displays with a few modes each, shared by every session of the process.
#[derive(Clone)]
pub struct Fake {
    state: &'static Mutex<HashMap<usize, Mode>>,
}

impl Fake {
    pub const MODES: [Mode; 4] = [
        Mode { w: 1920, h: 1080, hz: 60 },
        Mode { w: 1600, h: 900, hz: 60 },
        Mode { w: 1280, h: 720, hz: 60 },
        Mode { w: 1024, h: 768, hz: 60 },
    ];

    pub fn shared() -> Self {
        static STATE: std::sync::OnceLock<Mutex<HashMap<usize, Mode>>> = std::sync::OnceLock::new();
        Self { state: STATE.get_or_init(Mutex::default) }
    }

    /// A fake with its own state (unit tests).
    pub fn isolated() -> Self {
        Self { state: Box::leak(Box::default()) }
    }
}

impl Backend for Fake {
    fn modes(&self, display: usize) -> Result<Vec<Mode>> {
        if display > 1 {
            bail!("no display {display}");
        }
        Ok(Self::MODES.to_vec())
    }
    fn current(&self, display: usize) -> Result<Mode> {
        self.modes(display)?;
        Ok(*self.state.lock().unwrap().get(&display).unwrap_or(&Self::MODES[0]))
    }
    fn set(&mut self, display: usize, mode: Mode) -> Result<()> {
        self.modes(display)?;
        self.state.lock().unwrap().insert(display, mode);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    use anyhow::{Result, anyhow, bail};

    use super::{Backend, Mode, tidy};

    type ModeRef = *mut c_void;
    type CFArrayRef = *const c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDisplayCopyAllDisplayModes(display: u32, options: *const c_void) -> CFArrayRef;
        fn CGDisplayCopyDisplayMode(display: u32) -> ModeRef;
        fn CGDisplayModeGetWidth(mode: ModeRef) -> usize;
        fn CGDisplayModeGetHeight(mode: ModeRef) -> usize;
        fn CGDisplayModeGetPixelWidth(mode: ModeRef) -> usize;
        fn CGDisplayModeGetRefreshRate(mode: ModeRef) -> f64;
        fn CGDisplayModeIsUsableForDesktopGUI(mode: ModeRef) -> bool;
        fn CGDisplayModeRelease(mode: ModeRef);
        fn CGDisplaySetDisplayMode(display: u32, mode: ModeRef, options: *const c_void) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: CFArrayRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFArrayRef, i: isize) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }

    /// CoreGraphics display id of the capture display `index` (xcap reports it as the id).
    fn display_id(index: usize) -> Result<u32> {
        let monitors = xcap::Monitor::all()?;
        Ok(monitors.get(index).ok_or_else(|| anyhow!("no display {index}"))?.id()?)
    }

    fn mode_of(m: ModeRef) -> Mode {
        unsafe { Mode { w: CGDisplayModeGetWidth(m) as u32, h: CGDisplayModeGetHeight(m) as u32, hz: CGDisplayModeGetRefreshRate(m).round() as u32 } }
    }

    /// Calls `f` with every desktop-usable mode of the display, keeping the array alive.
    fn with_modes<T>(id: u32, f: impl FnOnce(&[ModeRef]) -> T) -> Result<T> {
        unsafe {
            let all = CGDisplayCopyAllDisplayModes(id, std::ptr::null());
            if all.is_null() {
                bail!("cannot read the display modes");
            }
            let modes: Vec<ModeRef> = (0..CFArrayGetCount(all))
                .map(|i| CFArrayGetValueAtIndex(all, i) as ModeRef)
                .filter(|m| CGDisplayModeIsUsableForDesktopGUI(*m))
                .collect();
            let out = f(&modes);
            CFRelease(all);
            Ok(out)
        }
    }

    pub struct Mac;

    impl Backend for Mac {
        fn modes(&self, display: usize) -> Result<Vec<Mode>> {
            let modes = with_modes(display_id(display)?, |ms| ms.iter().map(|m| mode_of(*m)).collect())?;
            Ok(tidy(modes))
        }

        fn current(&self, display: usize) -> Result<Mode> {
            let id = display_id(display)?;
            unsafe {
                let m = CGDisplayCopyDisplayMode(id);
                if m.is_null() {
                    bail!("cannot read the current display mode");
                }
                let mode = mode_of(m);
                CGDisplayModeRelease(m);
                Ok(mode)
            }
        }

        fn set(&mut self, display: usize, mode: Mode) -> Result<()> {
            let id = display_id(display)?;
            // The same size exists as HiDPI and as low resolution: take the sharpest.
            let err = with_modes(id, |ms| {
                let best = ms.iter().filter(|m| mode_of(**m) == mode).max_by_key(|m| unsafe { CGDisplayModeGetPixelWidth(**m) });
                best.map(|m| unsafe { CGDisplaySetDisplayMode(id, *m, std::ptr::null()) })
            })?;
            match err {
                Some(0) => Ok(()),
                Some(e) => bail!("CGDisplaySetDisplayMode failed ({e})"),
                None => bail!("{}×{} is not a mode of this display", mode.w, mode.h),
            }
        }
    }
}

#[cfg(windows)]
mod win {
    use anyhow::{Result, anyhow, bail};
    use windows::Win32::Graphics::Gdi::{
        CDS_FULLSCREEN, CDS_TYPE, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
        ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsExW,
    };
    use windows::core::PCWSTR;

    use super::{Backend, Mode, tidy};

    /// GDI device name (`\\.\DISPLAY1`) of the capture display `index`, NUL-terminated.
    fn device(index: usize) -> Result<Vec<u16>> {
        let monitors = xcap::Monitor::all()?;
        let name = monitors.get(index).ok_or_else(|| anyhow!("no display {index}"))?.name()?;
        Ok(name.encode_utf16().chain([0]).collect())
    }

    fn devmode() -> DEVMODEW {
        let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
        dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        dm
    }

    fn mode_of(dm: &DEVMODEW) -> Mode {
        Mode { w: dm.dmPelsWidth, h: dm.dmPelsHeight, hz: if dm.dmDisplayFrequency > 1 { dm.dmDisplayFrequency } else { 0 } }
    }

    fn change(dev: &[u16], dm: Option<&DEVMODEW>) -> Result<()> {
        let r = unsafe { ChangeDisplaySettingsExW(PCWSTR(dev.as_ptr()), dm.map(|d| d as *const _), None, if dm.is_some() { CDS_FULLSCREEN } else { CDS_TYPE(0) }, None) };
        if r != DISP_CHANGE_SUCCESSFUL {
            bail!("ChangeDisplaySettingsEx failed ({})", r.0);
        }
        Ok(())
    }

    pub struct Win;

    impl Backend for Win {
        fn modes(&self, display: usize) -> Result<Vec<Mode>> {
            let dev = device(display)?;
            let mut out = Vec::new();
            // Only modes with the current color depth (32-bit desktops).
            let bpp = {
                let mut dm = devmode();
                unsafe { EnumDisplaySettingsExW(PCWSTR(dev.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm, ENUM_DISPLAY_SETTINGS_FLAGS(0)) }.ok()?;
                dm.dmBitsPerPel
            };
            for i in 0..4096u32 {
                let mut dm = devmode();
                if !unsafe { EnumDisplaySettingsExW(PCWSTR(dev.as_ptr()), ENUM_DISPLAY_SETTINGS_MODE(i), &mut dm, ENUM_DISPLAY_SETTINGS_FLAGS(0)) }.as_bool() {
                    break;
                }
                if dm.dmBitsPerPel == bpp && dm.dmPelsWidth >= 640 {
                    out.push(mode_of(&dm));
                }
            }
            Ok(tidy(out))
        }

        fn current(&self, display: usize) -> Result<Mode> {
            let dev = device(display)?;
            let mut dm = devmode();
            unsafe { EnumDisplaySettingsExW(PCWSTR(dev.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm, ENUM_DISPLAY_SETTINGS_FLAGS(0)) }.ok()?;
            Ok(mode_of(&dm))
        }

        fn set(&mut self, display: usize, mode: Mode) -> Result<()> {
            let dev = device(display)?;
            let mut dm = devmode();
            dm.dmPelsWidth = mode.w;
            dm.dmPelsHeight = mode.h;
            dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
            if mode.hz > 0 {
                dm.dmDisplayFrequency = mode.hz;
                dm.dmFields |= DM_DISPLAYFREQUENCY;
            }
            change(&dev, Some(&dm))
        }

        /// Back to the mode saved in the registry (CDS_FULLSCREEN changes never touched it).
        fn restore(&mut self, display: usize, original: Mode) -> Result<()> {
            let dev = device(display)?;
            change(&dev, None).or_else(|_| self.set(display, original))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_restore_on_drop() {
        let fake = Fake::isolated();
        {
            let mut c = Changer::new(Box::new(fake.clone()));
            let list = c.list(0);
            assert_eq!(list["current"]["w"], 1920);
            assert_eq!(list["modes"].as_array().unwrap().len(), 4);
            assert!(c.set(0, 1111, 222, None).is_err());
            assert_eq!(c.set(0, 1280, 720, None).unwrap(), Mode { w: 1280, h: 720, hz: 60 });
            assert_eq!(fake.current(0).unwrap().w, 1280);
            c.set(0, 1024, 768, Some(60)).unwrap();
            // The original is the mode before the first change.
            assert_eq!(c.list(0)["original"]["w"], 1920);
            assert!(c.changed());
        }
        assert_eq!(fake.current(0).unwrap(), Fake::MODES[0]);
    }

    #[test]
    fn explicit_restore() {
        let fake = Fake::isolated();
        let mut c = Changer::new(Box::new(fake.clone()));
        assert_eq!(c.restore(1).unwrap(), None);
        c.set(1, 1600, 900, None).unwrap();
        assert_eq!(c.restore(1).unwrap(), Some(Fake::MODES[0]));
        assert!(!c.changed());
        assert_eq!(c.list(5)["t"], "res_error");
    }

    #[test]
    fn tidy_sorts_and_dedups() {
        let m = |w, h, hz| Mode { w, h, hz };
        assert_eq!(tidy(vec![m(800, 600, 60), m(1920, 1080, 60), m(800, 600, 60), m(1920, 1080, 75)]), vec![m(1920, 1080, 75), m(1920, 1080, 60), m(800, 600, 60)]);
    }
}
