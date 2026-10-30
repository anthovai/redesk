//! Wayland: screen capture and remote input through the XDG desktop portal.
//!
//! A Wayland program cannot read the screen or inject input on its own. The portal
//! (xdg-desktop-portal with the desktop's backend: GNOME, KDE, wlroots, ...) asks the local
//! user once, then hands out one PipeWire video stream per shared monitor (`ScreenCast`) and
//! accepts pointer and keyboard events for them (`RemoteDesktop`). The wlroots backend has
//! `ScreenCast` only: sessions are then view-only.
//!
//! One portal session is shared by every remote session that runs at the same time and closed
//! when the last one ends (the desktop's "screen is being shared" indicator goes away). The
//! restore token the portal returns is kept, so later sessions start without asking again
//! until the user revokes it in the desktop's settings.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use image::RgbaImage;
use serde::de::DeserializeOwned;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{self, DeserializeDict, OwnedObjectPath, OwnedValue, Type, Value};

use crate::capture::{DisplayInfo, Geometry};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const SCREENCAST: &str = "org.freedesktop.portal.ScreenCast";
const REMOTE_DESKTOP: &str = "org.freedesktop.portal.RemoteDesktop";

/// SourceType MONITOR; DeviceType KEYBOARD | POINTER; CursorMode EMBEDDED; PersistMode
/// "until revoked".
const SOURCE_MONITOR: u32 = 1;
const DEVICES: u32 = 1 | 2;
const CURSOR_EMBEDDED: u32 = 2;
const PERSIST_UNTIL_REVOKED: u32 = 2;

/// Whether screen capture and input go through the portal: a Wayland session, unless
/// `AEGIRON_CAPTURE=x11` forces X11 (Xwayland only shows X11 windows, so that is rarely useful).
/// `AEGIRON_CAPTURE=portal` uses the portal on X11 too (tests, or X11 desktops that have one).
pub fn wanted() -> bool {
    match std::env::var("AEGIRON_CAPTURE").as_deref() {
        Ok("x11") => false,
        Ok("portal") => true,
        _ => std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland") || std::env::var_os("WAYLAND_DISPLAY").is_some(),
    }
}

/// One shared monitor, in the compositor's logical coordinates.
#[derive(Debug, Clone)]
pub struct Stream {
    pub node: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Latest frame of every stream, written by the PipeWire thread.
#[derive(Default)]
struct Frames {
    slots: Vec<Mutex<Option<(u64, Arc<RgbaImage>)>>>,
    seq: AtomicU64,
}

pub struct Portal {
    conn: Connection,
    session: OwnedObjectPath,
    /// RemoteDesktop session (input works), or ScreenCast only (view-only).
    input: bool,
    pub streams: Vec<Stream>,
    frames: Arc<Frames>,
    quit: Option<pipewire::channel::Sender<()>>,
}

static CURRENT: Mutex<Weak<Portal>> = Mutex::new(Weak::new());
/// Serialises portal dialogs (two sessions starting at once ask once).
static OPENING: Mutex<()> = Mutex::new(());

/// The portal session in use, if any.
pub fn current() -> Option<Arc<Portal>> {
    CURRENT.lock().unwrap().upgrade()
}

/// The running portal session, or a new one (which may show the desktop's sharing dialog and
/// blocks until the local user answers). `input` asks for keyboard and pointer too.
pub fn acquire(input: bool) -> Result<Arc<Portal>> {
    let _one = OPENING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = current().filter(|p| p.input || !input) {
        return Ok(p);
    }
    let p = Arc::new(Portal::open(input)?);
    *CURRENT.lock().unwrap() = Arc::downgrade(&p);
    Ok(p)
}

#[derive(DeserializeDict, Type, Debug)]
#[zvariant(signature = "dict")]
struct StartResults {
    streams: Option<Vec<(u32, StreamProps)>>,
    restore_token: Option<String>,
    devices: Option<u32>,
}

#[derive(DeserializeDict, Type, Debug)]
#[zvariant(signature = "dict")]
struct StreamProps {
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
}

fn random_token() -> String {
    let b: [u8; 8] = crate::crypto::random_bytes();
    format!("aegiron_{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>())
}

/// Where the restore token is kept: it lets the next session start without the dialog.
fn token_path() -> Option<PathBuf> {
    Some(dirs::state_dir().or_else(dirs::data_local_dir)?.join("aegiron/portal-restore-token"))
}

/// Explains a missing portal in terms of what to install.
fn no_portal(e: zbus::Error) -> anyhow::Error {
    anyhow!(
        "this is a Wayland session but the XDG desktop portal has no ScreenCast interface ({e}). \
         Install xdg-desktop-portal with the backend for this desktop (xdg-desktop-portal-gnome, -kde or -wlr) \
         and PipeWire, or log in with an X11 session"
    )
}

impl Portal {
    fn open(want_input: bool) -> Result<Self> {
        let conn = Connection::session().context("no D-Bus session bus (needed for the screen-sharing portal)")?;
        let screencast = Proxy::new(&conn, DEST, PATH, SCREENCAST)?;
        let sc_version: u32 = screencast.get_property("version").map_err(no_portal)?;
        let cursor_modes: u32 = screencast.get_property("AvailableCursorModes").unwrap_or(0);
        let remote = Proxy::new(&conn, DEST, PATH, REMOTE_DESKTOP)?;
        let rd_version: Option<u32> = remote.get_property("version").ok();
        let rd_devices: u32 = remote.get_property("AvailableDeviceTypes").unwrap_or(0);
        let input = want_input && rd_version.is_some() && rd_devices & DEVICES != 0;
        if want_input && !input {
            tracing::warn!("the desktop portal has no RemoteDesktop interface: remote input is unavailable (view only)");
        }
        let restore = token_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());

        // CreateSession on RemoteDesktop (with input) or ScreenCast; the session path is
        // derived from our token, as the portal documents.
        let session_token = random_token();
        let (iface, persist_on_rd) = if input { (REMOTE_DESKTOP, rd_version.unwrap_or(1) >= 2) } else { (SCREENCAST, false) };
        let _: HashMap<String, OwnedValue> = request(&conn, iface, "CreateSession", |t| {
            let mut o: HashMap<&str, Value> = HashMap::new();
            o.insert("handle_token", Value::from(t.to_string()));
            o.insert("session_handle_token", Value::from(session_token.clone()));
            (o,)
        })?;
        let sender = unique_path_part(&conn)?;
        let session = OwnedObjectPath::try_from(format!("{PATH}/session/{sender}/{session_token}"))?;

        if input {
            let _: HashMap<String, OwnedValue> = request(&conn, REMOTE_DESKTOP, "SelectDevices", |t| {
                let mut o: HashMap<&str, Value> = HashMap::new();
                o.insert("handle_token", Value::from(t.to_string()));
                o.insert("types", Value::from(DEVICES & rd_devices));
                if persist_on_rd {
                    o.insert("persist_mode", Value::from(PERSIST_UNTIL_REVOKED));
                    if let Some(r) = &restore {
                        o.insert("restore_token", Value::from(r.clone()));
                    }
                }
                (session.clone(), o)
            })?;
        }
        let _: HashMap<String, OwnedValue> = request(&conn, SCREENCAST, "SelectSources", |t| {
            let mut o: HashMap<&str, Value> = HashMap::new();
            o.insert("handle_token", Value::from(t.to_string()));
            o.insert("types", Value::from(SOURCE_MONITOR));
            o.insert("multiple", Value::from(true));
            if cursor_modes & CURSOR_EMBEDDED != 0 {
                // The pointer drawn into the video: there is no other way to know where it is.
                o.insert("cursor_mode", Value::from(CURSOR_EMBEDDED));
            }
            // With RemoteDesktop the persist mode belongs to SelectDevices.
            if !input && sc_version >= 4 {
                o.insert("persist_mode", Value::from(PERSIST_UNTIL_REVOKED));
                if let Some(r) = &restore {
                    o.insert("restore_token", Value::from(r.clone()));
                }
            }
            (session.clone(), o)
        })?;
        let started: StartResults = request(&conn, iface, "Start", |t| {
            let mut o: HashMap<&str, Value> = HashMap::new();
            o.insert("handle_token", Value::from(t.to_string()));
            (session.clone(), "", o)
        })
        .map_err(|e| anyhow!("the local user did not allow screen sharing, or the portal failed: {e:#}"))?;
        if let (Some(tok), Some(path)) = (&started.restore_token, token_path()) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, tok);
        }
        let input = input && started.devices.unwrap_or(DEVICES) & 2 != 0;
        let streams: Vec<Stream> = started
            .streams
            .unwrap_or_default()
            .into_iter()
            .map(|(node, p)| {
                let (x, y) = p.position.unwrap_or((0, 0));
                let (w, h) = p.size.unwrap_or((0, 0));
                Stream { node, x, y, width: w.max(0) as u32, height: h.max(0) as u32 }
            })
            .collect();
        if streams.is_empty() {
            bail!("the portal shared no monitor");
        }
        let fd: zvariant::OwnedFd = screencast
            .call("OpenPipeWireRemote", &(session.clone(), HashMap::<&str, Value>::new()))
            .context("OpenPipeWireRemote")?;
        let frames = Arc::new(Frames { slots: streams.iter().map(|_| Mutex::new(None)).collect(), seq: AtomicU64::new(0) });
        let quit = start_pipewire(fd.into(), streams.iter().map(|s| s.node).collect(), frames.clone())?;
        tracing::info!(
            "portal: sharing {} monitor(s) {:?}, input {}",
            streams.len(),
            streams.iter().map(|s| (s.x, s.y, s.width, s.height)).collect::<Vec<_>>(),
            if input { "on" } else { "off" }
        );
        Ok(Self { conn, session, input, streams, frames, quit: Some(quit) })
    }

    pub fn has_input(&self) -> bool {
        self.input
    }

    fn stream(&self, display: usize) -> (usize, &Stream) {
        let i = display.min(self.streams.len() - 1);
        (i, &self.streams[i])
    }

    /// Size of a stream: what the portal said, else the size of the frames it sends.
    fn size(&self, display: usize) -> (u32, u32) {
        let (i, s) = self.stream(display);
        if s.width > 0 && s.height > 0 {
            return (s.width, s.height);
        }
        self.latest(i).map(|(_, img)| (img.width(), img.height())).unwrap_or((0, 0))
    }

    pub fn displays(&self) -> Vec<DisplayInfo> {
        (0..self.streams.len())
            .map(|i| {
                let (width, height) = self.size(i);
                DisplayInfo { index: i, name: format!("Screen {}", i + 1), width, height, primary: i == 0 }
            })
            .collect()
    }

    pub fn geometry(&self, display: usize) -> Geometry {
        let (i, s) = self.stream(display);
        let (width, height) = self.size(i);
        Geometry { display: i, x: s.x, y: s.y, width, height }
    }

    /// The newest frame of a stream and its sequence number (which changes with every frame).
    pub fn latest(&self, display: usize) -> Option<(u64, Arc<RgbaImage>)> {
        let slot = self.frames.slots.get(display.min(self.frames.slots.len().saturating_sub(1)))?;
        slot.lock().unwrap().clone()
    }

    fn notify<B>(&self, method: &str, body: &B) -> Result<()>
    where
        B: serde::Serialize + zvariant::DynamicType,
    {
        if !self.input {
            bail!("no remote input in this portal session");
        }
        let p = Proxy::new(&self.conn, DEST, PATH, REMOTE_DESKTOP)?;
        p.call_method(method, body)?;
        Ok(())
    }

    fn no_opts() -> HashMap<&'static str, Value<'static>> {
        HashMap::new()
    }

    /// Moves the pointer to a position normalised to the display (0..1).
    pub fn pointer_to(&self, display: usize, x: f64, y: f64) -> Result<()> {
        let (i, s) = self.stream(display);
        let (w, h) = self.size(i);
        let (px, py) = (x.clamp(0.0, 1.0) * w.saturating_sub(1) as f64, y.clamp(0.0, 1.0) * h.saturating_sub(1) as f64);
        self.notify("NotifyPointerMotionAbsolute", &(self.session.clone(), Self::no_opts(), s.node, px, py))
    }

    /// Linux evdev button code (BTN_LEFT 0x110, BTN_RIGHT 0x111, BTN_MIDDLE 0x112).
    pub fn button(&self, code: i32, down: bool) -> Result<()> {
        self.notify("NotifyPointerButton", &(self.session.clone(), Self::no_opts(), code, down as u32))
    }

    /// Wheel clicks; positive scrolls down (vertical) or right (horizontal).
    pub fn scroll(&self, vertical: bool, steps: i32) -> Result<()> {
        self.notify("NotifyPointerAxisDiscrete", &(self.session.clone(), Self::no_opts(), if vertical { 0u32 } else { 1u32 }, steps))
    }

    pub fn keysym(&self, sym: u32, down: bool) -> Result<()> {
        self.notify("NotifyKeyboardKeysym", &(self.session.clone(), Self::no_opts(), sym as i32, down as u32))
    }
}

impl Drop for Portal {
    fn drop(&mut self) {
        if let Some(q) = self.quit.take() {
            let _ = q.send(());
        }
        if let Ok(p) = Proxy::new(&self.conn, DEST, self.session.as_str(), "org.freedesktop.portal.Session") {
            let _ = p.call_method("Close", &());
        }
        tracing::info!("portal: screen sharing stopped");
    }
}

/// D-Bus unique name as used in portal object paths (":1.42" -> "1_42").
fn unique_path_part(conn: &Connection) -> Result<String> {
    Ok(conn.unique_name().context("no D-Bus unique name")?.trim_start_matches(':').replace('.', "_"))
}

/// Calls a portal method that answers through a Request object, and waits for the answer
/// (which may take a while: the user is looking at a dialog). `body` gets the handle token.
fn request<B, T>(conn: &Connection, iface: &str, method: &str, body: impl FnOnce(&str) -> B) -> Result<T>
where
    B: serde::Serialize + zvariant::DynamicType,
    T: DeserializeOwned + Type,
{
    let token = random_token();
    let path = format!("{PATH}/request/{}/{token}", unique_path_part(conn)?);
    let req = Proxy::new(conn, DEST, path, "org.freedesktop.portal.Request")?;
    // Subscribe before calling, so a quick answer is not missed.
    let mut responses = req.receive_signal("Response")?;
    let proxy = Proxy::new(conn, DEST, PATH, iface)?;
    proxy.call_method(method, &body(&token)).map_err(|e| match iface {
        SCREENCAST => no_portal(e),
        _ => anyhow!("{iface}.{method}: {e}"),
    })?;
    let msg = responses.next().ok_or_else(|| anyhow!("{method}: the portal closed the request"))?;
    let (code, results): (u32, T) = msg.body().deserialize().with_context(|| format!("{method}: unexpected portal answer"))?;
    match code {
        0 => Ok(results),
        1 => bail!("{method}: cancelled by the user"),
        c => bail!("{method}: failed (portal response {c})"),
    }
}

/// Pixel layouts the PipeWire streams are asked for (all 4 bytes per pixel).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Layout {
    Rgbx,
    Bgrx,
}

/// Copies a frame with a row stride into a packed RGBA image.
pub(crate) fn to_rgba(layout: Layout, w: u32, h: u32, stride: usize, src: &[u8]) -> Option<RgbaImage> {
    let row = w as usize * 4;
    if w == 0 || h == 0 || stride < row || src.len() < stride * (h as usize - 1) + row {
        return None;
    }
    let mut out = vec![0u8; row * h as usize];
    for (y, dst) in out.chunks_exact_mut(row).enumerate() {
        let s = &src[y * stride..y * stride + row];
        match layout {
            Layout::Rgbx => {
                for (d, p) in dst.chunks_exact_mut(4).zip(s.chunks_exact(4)) {
                    d.copy_from_slice(&[p[0], p[1], p[2], 255]);
                }
            }
            Layout::Bgrx => {
                for (d, p) in dst.chunks_exact_mut(4).zip(s.chunks_exact(4)) {
                    d.copy_from_slice(&[p[2], p[1], p[0], 255]);
                }
            }
        }
    }
    RgbaImage::from_raw(w, h, out)
}

/// Runs the PipeWire loop for the portal's streams on its own thread; the returned sender
/// stops it.
fn start_pipewire(fd: OwnedFd, nodes: Vec<u32>, frames: Arc<Frames>) -> Result<pipewire::channel::Sender<()>> {
    let (quit_tx, quit_rx) = pipewire::channel::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    std::thread::Builder::new().name("aegiron-pipewire".into()).spawn(move || {
        let ready2 = ready_tx.clone();
        if let Err(e) = pipewire_loop(fd, nodes, frames, quit_rx, ready_tx) {
            tracing::error!("pipewire: {e:#}");
            let _ = ready2.send(Err(e));
        }
    })?;
    ready_rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow!("PipeWire did not start"))??;
    Ok(quit_tx)
}

fn pipewire_loop(fd: OwnedFd, nodes: Vec<u32>, frames: Arc<Frames>, quit: pipewire::channel::Receiver<()>, ready: std::sync::mpsc::Sender<Result<()>>) -> Result<()> {
    use pipewire::spa::param::ParamType;
    use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use pipewire::spa::param::format_utils;
    use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
    use pipewire::spa::pod::{self, Pod, serialize::PodSerializer};
    use pipewire::spa::utils::{Direction, Fraction, Rectangle, SpaTypes};
    use pipewire::stream::{StreamFlags, StreamRc};

    pipewire::init();
    let main_loop = pipewire::main_loop::MainLoopRc::new(None).map_err(|e| anyhow!("main loop: {e}"))?;
    let context = pipewire::context::ContextRc::new(&main_loop, None).map_err(|e| anyhow!("context: {e}"))?;
    let core = context.connect_fd_rc(fd, None).map_err(|e| anyhow!("connect to the portal's PipeWire: {e}"))?;

    let format = pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(FormatProperties::VideoFormat, Choice, Enum, Id, VideoFormat::BGRx, VideoFormat::BGRx, VideoFormat::BGRA, VideoFormat::RGBx, VideoFormat::RGBA),
        pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle { width: 1920, height: 1080 },
            Rectangle { width: 1, height: 1 },
            Rectangle { width: 8192, height: 8192 }
        ),
        pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 30, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction { num: 60, denom: 1 }
        ),
    );
    let format = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &pod::Value::Object(format))
        .map_err(|e| anyhow!("format: {e:?}"))?
        .0
        .into_inner();

    let mut keep = Vec::new();
    for (i, node) in nodes.into_iter().enumerate() {
        let props = pipewire::properties::properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Screen",
        };
        let stream = StreamRc::new(core.clone(), "aegiron-screen", props).map_err(|e| anyhow!("stream: {e}"))?;
        let frames = frames.clone();
        let listener = stream
            .add_local_listener_with_user_data(VideoInfoRaw::default())
            .state_changed(move |_, _, old, new| tracing::debug!("pipewire stream {node}: {old:?} -> {new:?}"))
            .param_changed(|_, info, id, param| {
                let Some(param) = param else { return };
                if id != ParamType::Format.as_raw() {
                    return;
                }
                if !matches!(format_utils::parse_format(param), Ok((MediaType::Video, MediaSubtype::Raw))) {
                    return;
                }
                match info.parse(param) {
                    Ok(_) => tracing::info!("pipewire: {:?} {}x{}", info.format(), info.size().width, info.size().height),
                    Err(e) => tracing::warn!("pipewire: bad video format: {e:?}"),
                }
            })
            .process(move |stream, info| {
                let Some(mut buffer) = stream.dequeue_buffer() else { return };
                let datas = buffer.datas_mut();
                let Some(d) = datas.first_mut() else { return };
                let (offset, size, stride) = (d.chunk().offset() as usize, d.chunk().size() as usize, d.chunk().stride());
                let layout = match info.format() {
                    VideoFormat::BGRx | VideoFormat::BGRA => Layout::Bgrx,
                    VideoFormat::RGBx | VideoFormat::RGBA => Layout::Rgbx,
                    _ => return,
                };
                let (w, h) = (info.size().width, info.size().height);
                let stride = if stride > 0 { stride as usize } else { w as usize * 4 };
                if size == 0 {
                    return; // no new picture (e.g. only the cursor moved)
                }
                let Some(data) = d.data() else { return };
                let Some(src) = data.get(offset..) else { return };
                if let Some(img) = to_rgba(layout, w, h, stride, src) {
                    let seq = frames.seq.fetch_add(1, Ordering::Relaxed) + 1;
                    *frames.slots[i].lock().unwrap() = Some((seq, Arc::new(img)));
                }
            })
            .register()
            .map_err(|e| anyhow!("listener: {e}"))?;
        let mut params = [Pod::from_bytes(&format).ok_or_else(|| anyhow!("format pod"))?];
        stream
            .connect(Direction::Input, Some(node), StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS, &mut params)
            .map_err(|e| anyhow!("connect stream {node}: {e}"))?;
        keep.push((stream, listener));
    }
    let ml = main_loop.clone();
    let _quit = quit.attach(main_loop.loop_(), move |()| ml.quit());
    let _ = ready.send(Ok(()));
    main_loop.run();
    drop(keep);
    Ok(())
}

/// X11 keysym for a key the viewer names (KeyboardEvent.key), see input::named_key.
pub fn keysym_named(k: &enigo::Key) -> Option<u32> {
    use enigo::Key;
    Some(match k {
        Key::Return => 0xff0d,
        Key::Backspace => 0xff08,
        Key::Tab => 0xff09,
        Key::Escape => 0xff1b,
        Key::Delete => 0xffff,
        Key::Home => 0xff50,
        Key::End => 0xff57,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::UpArrow => 0xff52,
        Key::DownArrow => 0xff54,
        Key::LeftArrow => 0xff51,
        Key::RightArrow => 0xff53,
        Key::Shift => 0xffe1,
        Key::Control => 0xffe3,
        Key::Alt => 0xffe9,
        Key::Meta => 0xffeb, // Super_L
        Key::CapsLock => 0xffe5,
        Key::F1 => 0xffbe,
        Key::F2 => 0xffbf,
        Key::F3 => 0xffc0,
        Key::F4 => 0xffc1,
        Key::F5 => 0xffc2,
        Key::F6 => 0xffc3,
        Key::F7 => 0xffc4,
        Key::F8 => 0xffc5,
        Key::F9 => 0xffc6,
        Key::F10 => 0xffc7,
        Key::F11 => 0xffc8,
        Key::F12 => 0xffc9,
        _ => return None,
    })
}

/// X11 keysym of a character: Latin-1 maps to itself, the rest to 0x01000000 + code point.
pub fn keysym_char(c: char) -> u32 {
    let u = c as u32;
    match u {
        0x20..=0x7e | 0xa0..=0xff => u,
        0x0a | 0x0d => 0xff0d,
        0x09 => 0xff09,
        _ => 0x0100_0000 | u,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_strided_frames() {
        // 2x2 BGRx with 4 bytes of padding per row.
        let src = [1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9, 7, 8, 9, 0, 10, 11, 12, 0, 9, 9, 9, 9];
        let img = to_rgba(Layout::Bgrx, 2, 2, 12, &src).unwrap();
        assert_eq!(img.as_raw(), &[3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]);
        let img = to_rgba(Layout::Rgbx, 2, 2, 12, &src).unwrap();
        assert_eq!(img.get_pixel(1, 1).0, [10, 11, 12, 255]);
        // Too short for the claimed size.
        assert!(to_rgba(Layout::Rgbx, 2, 3, 12, &src).is_none());
    }

    #[test]
    fn keysyms() {
        assert_eq!(keysym_char('a'), 0x61);
        assert_eq!(keysym_char('é'), 0xe9);
        assert_eq!(keysym_char('ก'), 0x0100_0e01);
        assert_eq!(keysym_named(&enigo::Key::Return), Some(0xff0d));
        assert_eq!(keysym_named(&enigo::Key::F12), Some(0xffc9));
    }
}
