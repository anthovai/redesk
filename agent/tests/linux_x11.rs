//! X11 checks against a real X server: screen capture (xcap), JPEG and H.264 encoding of what
//! was captured, pointer injection (enigo/XTEST) and the clipboard.
//!
//! Ignored by default because they need DISPLAY and change the pointer and clipboard. CI runs
//! them under Xvfb through deploy/linux/test-x11.sh; by hand:
//!
//!   Xvfb :99 -screen 0 1280x720x24 & DISPLAY=:99 cargo test --test linux_x11 -- --ignored
//!
//! Tools used: xsetroot (paints the screen a known color) and xclip.
#![cfg(target_os = "linux")]

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use aegiron_agent::capture::{self, Params};
use aegiron_agent::codec::{Codec, H264Decoder};
use aegiron_agent::{clipboard, input, proto};
use tokio::sync::{mpsc, watch};

/// The tests share one X server, pointer and clipboard.
static X: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn x_lock() -> Option<std::sync::MutexGuard<'static, ()>> {
    let guard = X.lock().unwrap_or_else(|e| e.into_inner());
    if std::env::var_os("DISPLAY").is_none() {
        assert!(std::env::var("AEGIRON_REQUIRE_X11").as_deref() != Ok("1"), "AEGIRON_REQUIRE_X11=1 but DISPLAY is not set");
        eprintln!("skipped: no DISPLAY");
        return None;
    }
    Some(guard)
}

fn paint_screen(color: &str) {
    let ok = Command::new("xsetroot").args(["-solid", color]).status().expect("xsetroot (x11-xserver-utils)").success();
    assert!(ok, "xsetroot failed");
}

/// Runs the capture loop until it sends a frame message (JPEG rectangle or video).
fn first_frame(codec: Codec) -> Vec<u8> {
    let params = Params { display: 0, fps: 10, quality: 80, max_width: 1920, codec };
    let (_params_tx, params_rx) = watch::channel(params);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(4);
    let stop = Arc::new(AtomicBool::new(false));
    let force = Arc::new(AtomicBool::new(true));
    let s = stop.clone();
    let t = std::thread::spawn(move || capture::run(params_rx, out_tx, s, force, "openh264", "openh264"));
    let deadline = Instant::now() + Duration::from_secs(20);
    let msg = loop {
        assert!(Instant::now() < deadline, "no frame from the capture loop");
        let m = out_rx.blocking_recv().expect("capture loop ended");
        if matches!(m.first(), Some(&proto::MSG_FRAME) | Some(&proto::MSG_VIDEO)) {
            break m;
        }
    };
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(out_rx);
    t.join().unwrap();
    msg
}

fn be16(b: &[u8]) -> usize {
    u16::from_be_bytes([b[0], b[1]]) as usize
}

fn assert_reddish(px: &[u8], what: &str) {
    assert!(px[0] > 180 && px[1] < 70 && px[2] < 70, "{what}: expected red, got {:?}", &px[..3]);
}

#[test]
#[ignore]
fn x11_capture_jpeg_and_h264() {
    let Some(_x) = x_lock() else { return };
    let displays = capture::displays();
    eprintln!("displays: {displays:?}");
    assert!(!displays.is_empty(), "xcap found no monitor");
    let (w, h) = (displays[0].width as usize, displays[0].height as usize);
    assert!(w >= 320 && h >= 200, "{w}x{h}");
    let g = capture::geometry(0).unwrap();
    assert_eq!((g.width as usize, g.height as usize), (w, h));

    paint_screen("#ff0000");

    // JPEG: one full-screen rectangle at first.
    let m = first_frame(Codec::Jpeg);
    assert_eq!(m[0], proto::MSG_FRAME);
    let (x, y, fw, fh) = (be16(&m[1..]), be16(&m[3..]), be16(&m[5..]), be16(&m[7..]));
    assert_eq!((x, y), (0, 0));
    assert_eq!((fw, fh), (w, h), "frame size");
    let img = image::load_from_memory_with_format(&m[9..], image::ImageFormat::Jpeg).unwrap().to_rgb8();
    assert_eq!((img.width() as usize, img.height() as usize), (w, h));
    assert_reddish(&img.get_pixel(w as u32 / 2, h as u32 / 2).0, "jpeg");

    // H.264 (OpenH264): a keyframe that decodes to the same picture.
    let m = first_frame(Codec::H264);
    assert_eq!(m[0], proto::MSG_VIDEO);
    assert_eq!(m[1], Codec::H264.id());
    assert_eq!(m[2] & proto::VIDEO_KEYFRAME, proto::VIDEO_KEYFRAME, "first video frame is a keyframe");
    let (vw, vh) = (be16(&m[3..]), be16(&m[5..]));
    assert_eq!((vw, vh), (w & !1, h & !1));
    let mut dec = H264Decoder::new().unwrap();
    let (dw, dh, rgba) = dec.decode(&m[7..]).unwrap().expect("a picture from the keyframe");
    assert_eq!((dw, dh), (vw, vh));
    let c = (dh / 2 * dw + dw / 2) * 4;
    assert_reddish(&rgba[c..c + 4], "h264");

    paint_screen("#000000");
}

#[test]
#[ignore]
fn x11_input_moves_pointer() {
    use enigo::Mouse;
    let Some(_x) = x_lock() else { return };
    let g = capture::geometry(0).unwrap();
    let tx = input::spawn(g);
    let probe = enigo::Enigo::new(&enigo::Settings::default()).unwrap();
    for (x, y) in [(0.25, 0.75), (0.9, 0.1)] {
        tx.send(input::Event::Move { x, y }).unwrap();
        let want = (g.x + (x * (g.width - 1) as f64).round() as i32, g.y + (y * (g.height - 1) as f64).round() as i32);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let at = probe.location().unwrap();
            if at == want {
                break;
            }
            assert!(Instant::now() < deadline, "pointer at {at:?}, expected {want:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    // Clicks, wheel and keys go through the same XTEST connection; they must not fail.
    tx.send(input::Event::Button { b: 0, down: true, x: 0.5, y: 0.5 }).unwrap();
    tx.send(input::Event::Button { b: 0, down: false, x: 0.5, y: 0.5 }).unwrap();
    tx.send(input::Event::Wheel { dx: 0, dy: 1 }).unwrap();
    tx.send(input::Event::Key { code: "KeyA".into(), key: "a".into(), down: true }).unwrap();
    tx.send(input::Event::Key { code: "KeyA".into(), key: "a".into(), down: false }).unwrap();
    drop(tx);
}

fn xclip(args: &[&str], stdin: Option<&str>) -> String {
    let mut c = Command::new("xclip");
    c.args(args).stdout(Stdio::piped()).stderr(Stdio::null());
    c.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = c.spawn().expect("xclip");
    if let Some(s) = stdin {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    if stdin.is_some() {
        // `xclip -i` keeps serving the selection in the background; only wait for the parent.
        let _ = child.wait();
        return String::new();
    }
    String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
}

#[test]
#[ignore]
fn x11_clipboard_both_ways() {
    let Some(_x) = x_lock() else { return };
    // Set here, read by another X client: the text stays available after the call returns.
    clipboard::with(|c| c.set_text("Aegiron คลิปบอร์ด 1")).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(xclip(&["-o", "-selection", "clipboard"], None), "Aegiron คลิปบอร์ด 1");
    // Set by another client, read here.
    xclip(&["-i", "-selection", "clipboard"], Some("from xclip"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let got = clipboard::with(|c| c.get_text()).unwrap_or_default();
        if got == "from xclip" {
            break;
        }
        assert!(Instant::now() < deadline, "clipboard reads {got:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}
