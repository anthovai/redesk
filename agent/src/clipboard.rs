//! Clipboard access shared by the agent and the viewer.
//!
//! On Linux (X11 and Wayland) a copied text or file list is served by the process that set it,
//! for as long as its clipboard handle lives: a handle dropped right after setting loses the
//! content unless a clipboard manager takes it over. So one handle stays open for the life of
//! the process there. Elsewhere the system keeps the content and a fresh handle is fine.

/// Runs `f` with a clipboard handle.
pub fn with<R>(f: impl FnOnce(&mut arboard::Clipboard) -> Result<R, arboard::Error>) -> Result<R, arboard::Error> {
    #[cfg(target_os = "linux")]
    {
        static KEEP: std::sync::Mutex<Option<arboard::Clipboard>> = std::sync::Mutex::new(None);
        let mut keep = KEEP.lock().unwrap_or_else(|e| e.into_inner());
        if keep.is_none() {
            *keep = Some(arboard::Clipboard::new()?);
        }
        f(keep.as_mut().unwrap())
    }
    #[cfg(not(target_os = "linux"))]
    f(&mut arboard::Clipboard::new()?)
}
