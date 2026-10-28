//! macOS system-audio capture with ScreenCaptureKit (macOS 13+). Uses the Screen Recording
//! permission the agent already needs; no microphone access.
//!
//! The framework is loaded with dlopen and driven through the Objective-C runtime, so the app
//! still launches on macOS 11/12 (where audio is simply reported as unavailable).

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use block2::RcBlock;
use objc2::encode::{Encode, Encoding};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, ClassBuilder, MessageReceiver, NSObject, Sel};
use objc2::{ClassType, msg_send, sel};

use super::PcmSink;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetFormatDescription(sb: *const c_void) -> *const c_void;
    fn CMAudioFormatDescriptionGetStreamBasicDescription(desc: *const c_void) -> *const Asbd;
    fn CMSampleBufferGetNumSamples(sb: *mut c_void) -> isize;
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sb: *mut c_void,
        size_needed: *mut usize,
        list: *mut AudioBufferList,
        list_size: usize,
        struct_alloc: *const c_void,
        block_alloc: *const c_void,
        flags: u32,
        block_out: *mut *mut c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

#[repr(C)]
struct Asbd {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBuffer {
    channels: u32,
    size: u32,
    data: *mut c_void,
}

const MAX_BUFFERS: usize = 8;

#[repr(C)]
struct AudioBufferList {
    count: u32,
    buffers: [AudioBuffer; MAX_BUFFERS],
}

const FLAG_FLOAT: u32 = 1;
const FLAG_NON_INTERLEAVED: u32 = 1 << 5;
const OUTPUT_SCREEN: isize = 0;
const OUTPUT_AUDIO: isize = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

unsafe impl Encode for CMTime {
    const ENCODING: Encoding = Encoding::Struct("?", &[i64::ENCODING, i32::ENCODING, u32::ENCODING, i64::ENCODING]);
}

fn framework_loaded() -> bool {
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| {
        let path = c"/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit";
        !unsafe { dlopen(path.as_ptr(), 1) }.is_null()
    })
}

pub fn capture_supported() -> bool {
    framework_loaded()
        && AnyClass::get(c"SCStream").is_some()
        && AnyClass::get(c"SCStreamConfiguration").is_some_and(|c| c.instance_method(sel!(setCapturesAudio:)).is_some())
}

type SharedSink = Arc<Mutex<Option<PcmSink>>>;

/// Objective-C class receiving the stream's sample buffers (SCStreamOutput).
fn output_class() -> &'static AnyClass {
    static CLS: OnceLock<&'static AnyClass> = OnceLock::new();
    CLS.get_or_init(|| {
        let mut b = ClassBuilder::new(c"AegironAudioOutput", NSObject::class()).expect("class name is free");
        if let Some(p) = AnyProtocol::get(c"SCStreamOutput") {
            b.add_protocol(p);
        }
        b.add_ivar::<*mut c_void>(c"sink");
        unsafe {
            b.add_method(
                sel!(stream:didOutputSampleBuffer:ofType:),
                did_output as unsafe extern "C-unwind" fn(*mut NSObject, Sel, *mut AnyObject, *mut c_void, isize),
            );
        }
        b.register()
    })
}

unsafe extern "C-unwind" fn did_output(this: *mut NSObject, _: Sel, _stream: *mut AnyObject, sb: *mut c_void, kind: isize) {
    if kind != OUTPUT_AUDIO || sb.is_null() || this.is_null() {
        return;
    }
    let ivar = output_class().instance_variable(c"sink").unwrap();
    let ptr = unsafe { *ivar.load::<*mut c_void>(&*this) } as *const Mutex<Option<PcmSink>>;
    if ptr.is_null() {
        return;
    }
    let sink = unsafe { &*ptr };
    if let Some((pcm, ch, rate)) = unsafe { read_pcm(sb) }
        && let Some(f) = sink.lock().unwrap().as_mut()
    {
        f(&pcm, ch, rate);
    }
}

/// Interleaved f32 samples from an audio CMSampleBuffer.
unsafe fn read_pcm(sb: *mut c_void) -> Option<(Vec<f32>, usize, u32)> {
    unsafe {
        let desc = CMSampleBufferGetFormatDescription(sb);
        if desc.is_null() {
            return None;
        }
        let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(desc).as_ref()?;
        let frames = CMSampleBufferGetNumSamples(sb).max(0) as usize;
        let ch = asbd.channels_per_frame as usize;
        if frames == 0 || ch == 0 {
            return None;
        }
        // Ask for the exact list size first (one buffer per channel when non-interleaved).
        let mut needed = 0usize;
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sb,
            &mut needed,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
        );
        let mut storage = vec![0u128; needed.max(std::mem::size_of::<AudioBufferList>()).div_ceil(16)];
        let list_ptr = storage.as_mut_ptr() as *mut AudioBufferList;
        let mut block: *mut c_void = std::ptr::null_mut();
        let st = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sb,
            std::ptr::null_mut(),
            list_ptr,
            needed,
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut block,
        );
        if st != 0 {
            return None;
        }
        let list = &*list_ptr;
        let float = asbd.format_flags & FLAG_FLOAT != 0 && asbd.bits_per_channel == 32;
        let int16 = asbd.format_flags & FLAG_FLOAT == 0 && asbd.bits_per_channel == 16;
        let mut out = vec![0f32; frames * ch];
        let bufs = &list.buffers[..(list.count as usize).min(MAX_BUFFERS)];
        let sample = |b: &AudioBuffer, i: usize| -> f32 {
            if float {
                *(b.data as *const f32).add(i)
            } else {
                *(b.data as *const i16).add(i) as f32 / 32768.0
            }
        };
        let width = if float { 4 } else { 2 };
        if float || int16 {
            if asbd.format_flags & FLAG_NON_INTERLEAVED != 0 {
                for (c, b) in bufs.iter().take(ch).enumerate() {
                    let n = frames.min(b.size as usize / width);
                    for i in 0..n {
                        out[i * ch + c] = sample(b, i);
                    }
                }
            } else if let Some(b) = bufs.first() {
                let n = (frames * ch).min(b.size as usize / width);
                for (i, o) in out.iter_mut().enumerate().take(n) {
                    *o = sample(b, i);
                }
            }
        }
        if !block.is_null() {
            CFRelease(block);
        }
        (float || int16).then_some((out, ch, asbd.sample_rate as u32))
    }
}

struct SendPtr(Option<Retained<AnyObject>>, Option<String>);
unsafe impl Send for SendPtr {}

fn describe(err: *mut AnyObject) -> String {
    if err.is_null() {
        return "unknown error".into();
    }
    unsafe {
        let s: *mut AnyObject = msg_send![err, localizedDescription];
        if s.is_null() {
            return "unknown error".into();
        }
        let p: *const c_char = msg_send![s, UTF8String];
        if p.is_null() { "unknown error".into() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

pub struct Capture {
    stream: Retained<AnyObject>,
    _output: Retained<AnyObject>,
    sink: SharedSink,
}

// The stream is only messaged from the owning thread and on drop.
unsafe impl Send for Capture {}

impl Drop for Capture {
    fn drop(&mut self) {
        *self.sink.lock().unwrap() = None;
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let block = RcBlock::new(move |_err: *mut AnyObject| {
            let _ = tx.send(());
        });
        unsafe {
            let _: () = msg_send![&*self.stream, stopCaptureWithCompletionHandler: &*block];
        }
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
}

pub fn start(sink: PcmSink) -> Result<Capture> {
    start_with(sink, true)
}

/// `exclude_self`: leave out this process's own sound (the agent never plays any, but a
/// desktop app that is also viewing another machine would otherwise echo it back).
pub fn start_with(sink: PcmSink, exclude_self: bool) -> Result<Capture> {
    if !capture_supported() {
        bail!("ScreenCaptureKit audio needs macOS 13 or later");
    }
    autoreleasepool(|_| unsafe { start_inner(sink, exclude_self) })
}

unsafe fn start_inner(sink: PcmSink, exclude_self: bool) -> Result<Capture> {
    unsafe {
        // 1. A display to attach the (audio-only, in practice) stream to.
        let (tx, rx) = std::sync::mpsc::channel::<SendPtr>();
        let block = RcBlock::new(move |content: *mut AnyObject, err: *mut AnyObject| {
            let r = if content.is_null() { SendPtr(None, Some(describe(err))) } else { SendPtr(Retained::retain(content), None) };
            let _ = tx.send(r);
        });
        let cls = AnyClass::get(c"SCShareableContent").ok_or_else(|| anyhow!("SCShareableContent missing"))?;
        let _: () = msg_send![cls, getShareableContentExcludingDesktopWindows: Bool::YES, onScreenWindowsOnly: Bool::YES, completionHandler: &*block];
        let content = match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(SendPtr(Some(c), _)) => c,
            Ok(SendPtr(None, e)) => bail!("screen content unavailable (Screen Recording permission?): {}", e.unwrap_or_default()),
            Err(_) => bail!("timed out listing displays"),
        };
        let displays: *mut AnyObject = msg_send![&*content, displays];
        let display: *mut AnyObject = if displays.is_null() { std::ptr::null_mut() } else { msg_send![displays, firstObject] };
        if display.is_null() {
            bail!("no display to capture audio from");
        }

        // 2. Filter + configuration: audio at 48 kHz stereo, video as small and rare as possible.
        let nsarray = AnyClass::get(c"NSArray").unwrap();
        let empty: *mut AnyObject = msg_send![nsarray, array];
        let filter: Option<Retained<AnyObject>> =
            msg_send![msg_send![AnyClass::get(c"SCContentFilter").unwrap(), alloc], initWithDisplay: display, excludingWindows: empty];
        let filter = filter.ok_or_else(|| anyhow!("SCContentFilter init failed"))?;
        let config: Retained<AnyObject> = msg_send![AnyClass::get(c"SCStreamConfiguration").unwrap(), new];
        let _: () = msg_send![&*config, setCapturesAudio: Bool::YES];
        let _: () = msg_send![&*config, setSampleRate: super::SAMPLE_RATE as isize];
        let _: () = msg_send![&*config, setChannelCount: super::CHANNELS as isize];
        let _: () = msg_send![&*config, setExcludesCurrentProcessAudio: Bool::new(exclude_self)];
        let _: () = msg_send![&*config, setWidth: 2usize];
        let _: () = msg_send![&*config, setHeight: 2usize];
        let _: () = msg_send![&*config, setMinimumFrameInterval: CMTime { value: 1, timescale: 1, flags: 1, epoch: 0 }];

        let stream: Option<Retained<AnyObject>> = msg_send![
            msg_send![AnyClass::get(c"SCStream").unwrap(), alloc],
            initWithFilter: &*filter,
            configuration: &*config,
            delegate: std::ptr::null_mut::<AnyObject>()
        ];
        let stream = stream.ok_or_else(|| anyhow!("SCStream init failed"))?;

        // 3. Sample handler. Screen frames are registered too (and ignored) so the stream does
        //    not log about a missing output for every frame.
        let shared: SharedSink = Arc::new(Mutex::new(Some(sink)));
        let output: Retained<AnyObject> = msg_send![output_class(), new];
        let ivar = output_class().instance_variable(c"sink").unwrap();
        // The output object keeps one reference for as long as it lives (never released:
        // ScreenCaptureKit may still hold the object briefly after stopping).
        *ivar.load_ptr::<*mut c_void>(&output) = Arc::into_raw(shared.clone()) as *mut c_void;
        let add = Sel::register(c"addStreamOutput:type:sampleHandlerQueue:error:");
        for kind in [OUTPUT_SCREEN, OUTPUT_AUDIO] {
            let mut err: *mut AnyObject = std::ptr::null_mut();
            let ok: Bool = (&*stream).send_message(add, (&*output, kind, std::ptr::null_mut::<AnyObject>(), &mut err as *mut *mut AnyObject));
            if !ok.as_bool() {
                bail!("addStreamOutput: {}", describe(err));
            }
        }

        // 4. Start.
        let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
        let block = RcBlock::new(move |err: *mut AnyObject| {
            let _ = tx.send((!err.is_null()).then(|| describe(err)));
        });
        let _: () = msg_send![&*stream, startCaptureWithCompletionHandler: &*block];
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(None) => {}
            Ok(Some(e)) => bail!("audio capture did not start: {e}"),
            Err(_) => bail!("timed out starting audio capture"),
        }
        Ok(Capture { stream, _output: output, sink: shared })
    }
}
