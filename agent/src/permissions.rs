//! macOS privacy (TCC) permissions the agent needs to be controlled remotely.
//!
//! Users grant these in System Settings > Privacy & Security; there is no entitlement for them.
//! On other systems every permission reports as granted.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Capture the screen to send it to the viewer.
    ScreenRecording,
    /// Inject mouse and keyboard events (CGEventPost).
    Accessibility,
    /// Listen to keyboard events system-wide (not needed for basic remote control).
    InputMonitoring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permission {
    pub kind: Kind,
    pub granted: bool,
    /// Remote control does not work without it.
    pub required: bool,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::ScreenRecording, Kind::Accessibility, Kind::InputMonitoring];

    /// Name as shown in System Settings.
    pub fn name(self) -> &'static str {
        match self {
            Kind::ScreenRecording => "Screen Recording",
            Kind::Accessibility => "Accessibility",
            Kind::InputMonitoring => "Input Monitoring",
        }
    }

    /// Why Aegiron needs it (Thai, for the UI).
    pub fn reason(self) -> &'static str {
        match self {
            Kind::ScreenRecording => "ส่งภาพหน้าจอและเสียงให้ผู้ควบคุม (หลังอนุญาตต้องเปิด Aegiron ใหม่)",
            Kind::Accessibility => "ให้ผู้ควบคุมใช้เมาส์และคีย์บอร์ดของเครื่องนี้",
            Kind::InputMonitoring => "รับปุ่มลัดของระบบระหว่างควบคุม (ไม่บังคับ)",
        }
    }

    pub fn required(self) -> bool {
        !matches!(self, Kind::InputMonitoring)
    }

    fn settings_anchor(self) -> &'static str {
        match self {
            Kind::ScreenRecording => "Privacy_ScreenCapture",
            Kind::Accessibility => "Privacy_Accessibility",
            Kind::InputMonitoring => "Privacy_ListenEvent",
        }
    }

    /// Current status (cheap enough to call every couple of seconds).
    pub fn granted(self) -> bool {
        imp::granted(self)
    }

    /// Shows the system prompt (only the first time macOS asks; later calls just return the
    /// status). Returns whether it is granted now.
    pub fn request(self) -> bool {
        imp::request(self)
    }

    /// Opens the matching pane of System Settings > Privacy & Security.
    pub fn open_settings(self) -> std::io::Result<()> {
        if cfg!(target_os = "macos") {
            let url = format!("x-apple.systempreferences:com.apple.preference.security?{}", self.settings_anchor());
            std::process::Command::new("/usr/bin/open").arg(url).spawn().map(|_| ())
        } else {
            Ok(())
        }
    }
}

/// Status of all permissions, in display order.
pub fn status() -> Vec<Permission> {
    Kind::ALL.iter().map(|&kind| Permission { kind, granted: kind.granted(), required: kind.required() }).collect()
}

/// True if every permission needed for remote control is granted.
pub fn all_required_granted() -> bool {
    Kind::ALL.iter().filter(|k| k.required()).all(|k| k.granted())
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Kind;
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;
    type CFIndex = isize;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFBooleanTrue: CFTypeRef;
        static kCFBooleanFalse: CFTypeRef;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        fn CFDictionaryCreate(
            allocator: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: CFIndex,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFTypeRef;
        fn CFRelease(cf: CFTypeRef);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        static kAXTrustedCheckOptionPrompt: CFTypeRef;
        fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> u8;
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOHIDCheckAccess(request: u32) -> u32;
        fn IOHIDRequestAccess(request: u32) -> bool;
    }

    const IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;
    const IOHID_ACCESS_TYPE_GRANTED: u32 = 0;

    fn ax_trusted(prompt: bool) -> bool {
        // SAFETY: plain CoreFoundation calls; the dictionary is released after use.
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [if prompt { kCFBooleanTrue } else { kCFBooleanFalse }];
            let dict = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let trusted = AXIsProcessTrustedWithOptions(dict) != 0;
            if !dict.is_null() {
                CFRelease(dict);
            }
            trusted
        }
    }

    pub fn granted(kind: Kind) -> bool {
        // SAFETY: status queries without arguments that need managing.
        unsafe {
            match kind {
                Kind::ScreenRecording => CGPreflightScreenCaptureAccess(),
                Kind::Accessibility => ax_trusted(false),
                Kind::InputMonitoring => IOHIDCheckAccess(IOHID_REQUEST_TYPE_LISTEN_EVENT) == IOHID_ACCESS_TYPE_GRANTED,
            }
        }
    }

    pub fn request(kind: Kind) -> bool {
        // SAFETY: as above; each call may show a system dialog.
        unsafe {
            match kind {
                Kind::ScreenRecording => CGRequestScreenCaptureAccess(),
                Kind::Accessibility => ax_trusted(true),
                Kind::InputMonitoring => IOHIDRequestAccess(IOHID_REQUEST_TYPE_LISTEN_EVENT),
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Kind;
    pub fn granted(_: Kind) -> bool {
        true
    }
    pub fn request(_: Kind) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lists_every_kind_once() {
        let s = status();
        assert_eq!(s.len(), Kind::ALL.len());
        assert!(s.iter().zip(Kind::ALL).all(|(p, k)| p.kind == k && p.required == k.required()));
        assert!(Kind::ScreenRecording.required() && Kind::Accessibility.required() && !Kind::InputMonitoring.required());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn granted_elsewhere() {
        assert!(all_required_granted());
    }
}
