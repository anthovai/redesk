//! System audio on Linux: records the monitor of the default output (what the machine plays)
//! through PulseAudio's simple API, which PipeWire also serves (pipewire-pulse) on current
//! desktops.
//!
//! libpulse-simple is loaded at run time rather than linked, so the agent still starts on
//! machines without it (servers, minimal installs); they just have no audio.
//! `AEGIRON_AUDIO_SOURCE` picks another source than `@DEFAULT_MONITOR@` (see `pactl list
//! short sources`).

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;

use super::*;

#[repr(C)]
struct SampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

#[repr(C)]
struct BufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

const PA_SAMPLE_FLOAT32LE: c_int = 5;
const PA_STREAM_RECORD: c_int = 2;
/// 10 ms of 48 kHz stereo f32 per read.
const BLOCK: usize = SAMPLE_RATE as usize / 100 * CHANNELS;

type NewFn = unsafe extern "C" fn(
    server: *const c_char,
    name: *const c_char,
    dir: c_int,
    dev: *const c_char,
    stream_name: *const c_char,
    ss: *const SampleSpec,
    map: *const c_void,
    attr: *const BufferAttr,
    error: *mut c_int,
) -> *mut c_void;
type ReadFn = unsafe extern "C" fn(s: *mut c_void, data: *mut c_void, bytes: usize, error: *mut c_int) -> c_int;
type FreeFn = unsafe extern "C" fn(s: *mut c_void);
type StrErrorFn = unsafe extern "C" fn(error: c_int) -> *const c_char;

struct Lib {
    new: NewFn,
    read: ReadFn,
    free: FreeFn,
    strerror: StrErrorFn,
}

fn lib() -> Result<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(|| unsafe {
        // libpulse-simple links libpulse, which has pa_strerror.
        let h = libc::dlopen(c"libpulse-simple.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if h.is_null() {
            return None;
        }
        let sym = |n: &CStr| {
            let p = libc::dlsym(h, n.as_ptr());
            (!p.is_null()).then_some(p)
        };
        Some(Lib {
            new: std::mem::transmute::<*mut c_void, NewFn>(sym(c"pa_simple_new")?),
            read: std::mem::transmute::<*mut c_void, ReadFn>(sym(c"pa_simple_read")?),
            free: std::mem::transmute::<*mut c_void, FreeFn>(sym(c"pa_simple_free")?),
            strerror: std::mem::transmute::<*mut c_void, StrErrorFn>(sym(c"pa_strerror")?),
        })
    })
    .as_ref()
    .ok_or_else(|| anyhow!("libpulse-simple.so.0 not found (install libpulse0, or pipewire-pulse)"))
}

/// An open record stream.
struct Simple {
    lib: &'static Lib,
    s: *mut c_void,
}
// Only used from the capture thread that opened it.
unsafe impl Send for Simple {}

impl Simple {
    fn open() -> Result<Self> {
        let lib = lib()?;
        let source = std::env::var("AEGIRON_AUDIO_SOURCE").unwrap_or_else(|_| "@DEFAULT_MONITOR@".into());
        let source = CString::new(source).map_err(|_| anyhow!("bad AEGIRON_AUDIO_SOURCE"))?;
        let ss = SampleSpec { format: PA_SAMPLE_FLOAT32LE, rate: SAMPLE_RATE, channels: CHANNELS as u8 };
        // Small fragments keep the latency low; -1 lets the server choose the rest.
        let attr = BufferAttr { maxlength: u32::MAX, tlength: u32::MAX, prebuf: u32::MAX, minreq: u32::MAX, fragsize: (BLOCK * 4) as u32 };
        let mut err = 0;
        let s = unsafe {
            (lib.new)(
                std::ptr::null(),
                c"Aegiron".as_ptr(),
                PA_STREAM_RECORD,
                source.as_ptr(),
                c"remote session audio".as_ptr(),
                &ss,
                std::ptr::null(),
                &attr,
                &mut err,
            )
        };
        if s.is_null() {
            bail!("PulseAudio/PipeWire: cannot record {}: {}", source.to_string_lossy(), lib.error(err));
        }
        Ok(Self { lib, s })
    }

    fn read(&mut self, buf: &mut [f32]) -> Result<()> {
        let mut err = 0;
        let r = unsafe { (self.lib.read)(self.s, buf.as_mut_ptr().cast(), std::mem::size_of_val(buf), &mut err) };
        if r < 0 {
            bail!("PulseAudio read: {}", self.lib.error(err));
        }
        Ok(())
    }
}

impl Lib {
    fn error(&self, code: c_int) -> String {
        let p = unsafe { (self.strerror)(code) };
        if p.is_null() { format!("error {code}") } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
    }
}

impl Drop for Simple {
    fn drop(&mut self) {
        unsafe { (self.lib.free)(self.s) }
    }
}

/// True if the library is there and the sound server accepts a record stream on the monitor.
pub fn capture_supported() -> bool {
    match Simple::open() {
        Ok(_) => true,
        Err(e) => {
            tracing::info!("audio capture unavailable: {e:#}");
            false
        }
    }
}

pub struct Capture {
    stop: Arc<AtomicBool>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn start(mut sink: PcmSink) -> Result<Capture> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    let stop2 = stop.clone();
    std::thread::Builder::new()
        .name("aegiron-audio-in".into())
        .spawn(move || {
            let mut s = match Simple::open() {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            // The monitor delivers continuously (silence included), so the stop flag is seen
            // within a block.
            let mut buf = vec![0f32; BLOCK];
            while !stop2.load(Ordering::Relaxed) {
                if let Err(e) = s.read(&mut buf) {
                    tracing::warn!("audio capture stopped: {e:#}");
                    break;
                }
                sink(&buf, CHANNELS, SAMPLE_RATE);
            }
        })
        .map_err(|e| anyhow!("audio thread: {e}"))?;
    ready_rx.recv_timeout(Duration::from_secs(5)).map_err(|_| anyhow!("audio capture did not start"))??;
    Ok(Capture { stop })
}
