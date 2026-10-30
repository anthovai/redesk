//! Plaintext message framing inside the encrypted channel.
//!
//! Every decrypted message starts with a type byte:
//!   0x01  JSON control message (UTF-8)
//!   0x02  video rectangle: x, y, frame_w, frame_h (u16 BE) followed by a JPEG
//!   0x03  file chunk: transfer id (u32 BE) followed by raw bytes
//!   0x04  video frame: codec (u8, 1 = H.264, 2 = AV1), flags (u8, bit 0 = keyframe), width,
//!         height (u16 BE) followed by the encoded access unit (H.264: Annex B; AV1: one
//!         temporal unit of low-overhead OBUs)
//!   0x05  audio packet: codec (u8, 1 = Opus 48 kHz stereo), sequence (u32 BE, one per 20 ms
//!         frame) followed by one Opus packet. Only sent to viewers that asked for audio.

use serde::Deserialize;

pub const MSG_JSON: u8 = 0x01;
pub const MSG_FRAME: u8 = 0x02;
pub const MSG_FILE: u8 = 0x03;
pub const MSG_VIDEO: u8 = 0x04;
pub const MSG_AUDIO: u8 = 0x05;

pub const AUDIO_OPUS: u8 = 1;

pub const VIDEO_KEYFRAME: u8 = 0x01;

pub fn file_chunk(id: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + data.len());
    out.push(MSG_FILE);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(data);
    out
}

pub fn json(v: &serde_json::Value) -> Vec<u8> {
    let mut out = vec![MSG_JSON];
    out.extend_from_slice(v.to_string().as_bytes());
    out
}

pub fn frame_header(x: u16, y: u16, frame_w: u16, frame_h: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 * 1024);
    out.push(MSG_FRAME);
    for v in [x, y, frame_w, frame_h] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

pub fn video(codec: u8, keyframe: bool, w: u16, h: u16, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(7 + data.len());
    out.extend_from_slice(&[MSG_VIDEO, codec, if keyframe { VIDEO_KEYFRAME } else { 0 }]);
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(data);
    out
}

pub fn audio(codec: u8, seq: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + data.len());
    out.extend_from_slice(&[MSG_AUDIO, codec]);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// Splits a MSG_AUDIO message into (codec, sequence, payload).
pub fn parse_audio(msg: &[u8]) -> Option<(u8, u32, &[u8])> {
    if msg.len() < 7 || msg[0] != MSG_AUDIO {
        return None;
    }
    Some((msg[1], u32::from_be_bytes(msg[2..6].try_into().ok()?), &msg[6..]))
}

/// Messages the viewer may send after the handshake.
#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
pub enum ViewerMsg {
    #[serde(rename = "auth")]
    Auth {
        password: Option<String>,
        /// Video codecs the viewer can decode, e.g. ["h264", "jpeg"]; absent = JPEG only.
        #[serde(default)]
        codecs: Vec<String>,
        /// Optional viewer capabilities, e.g. ["clipfiles"] (absent on older viewers).
        #[serde(default)]
        features: Vec<String>,
        /// Audio codecs the viewer can play, e.g. ["opus"]; absent = no audio is sent.
        #[serde(default)]
        audio: Vec<String>,
        /// One-time token from `ctl {op:"restart"}`: reconnecting after the restart it asked
        /// for, without the password or a click (feature "restart").
        #[serde(default)]
        resume: Option<String>,
    },
    /// Viewer muted (`on: false`) or unmuted its speaker; the agent stops/resumes 0x05 packets.
    #[serde(rename = "audio")]
    Audio { on: bool },
    /// Pointer move; x, y are normalised to [0, 1] across the displayed frame.
    #[serde(rename = "mm")]
    MouseMove { x: f64, y: f64 },
    /// Mouse button (0 left, 1 middle, 2 right) press/release.
    #[serde(rename = "mb")]
    MouseButton { b: u8, down: bool, x: f64, y: f64 },
    #[serde(rename = "wh")]
    Wheel { dx: i32, dy: i32 },
    /// Key press/release using browser KeyboardEvent.code / .key.
    #[serde(rename = "key")]
    Key { code: String, key: String, down: bool },
    #[serde(rename = "text")]
    Text { s: String },
    #[serde(rename = "clip")]
    Clipboard { s: String },
    /// Stream settings; `codec` ("av1", "h264", "jpeg") switches to one of `info.codecs`.
    #[serde(rename = "cfg")]
    Config { fps: Option<u32>, quality: Option<u8>, max_width: Option<u32>, display: Option<usize>, codec: Option<String> },
    /// List a directory ("" = home).
    #[serde(rename = "ls")]
    List { path: String },
    /// Start an upload into `dir` ("" = ~/Downloads/Aegiron); chunks follow as MSG_FILE.
    /// With `rel` (feature "fs2") the file goes to that relative path inside `dir`.
    #[serde(rename = "put")]
    Put {
        id: u32,
        dir: String,
        #[serde(default)]
        name: String,
        size: u64,
        #[serde(default)]
        rel: Option<String>,
    },
    #[serde(rename = "put_end")]
    PutEnd { id: u32 },
    #[serde(rename = "get")]
    Get { id: u32, path: String },
    #[serde(rename = "cancel")]
    Cancel { id: u32 },
    /// Whiteboard: `op` = "line" (append points `p` to stroke `id`, color `c`, width `w`) or
    /// "clear". Only honoured when the agent advertised `info.whiteboard`.
    #[serde(rename = "wb")]
    Whiteboard {
        op: String,
        #[serde(default)]
        id: u32,
        #[serde(default)]
        c: String,
        #[serde(default)]
        w: f32,
        #[serde(default)]
        p: Vec<f32>,
    },
    /// Download a whole directory tree (feature "fs2").
    #[serde(rename = "get_dir")]
    GetDir { id: u32, path: String },
    /// Create a directory; `name` may be a relative path (feature "fs2").
    #[serde(rename = "mkdir")]
    Mkdir {
        #[serde(default)]
        id: Option<u32>,
        dir: String,
        name: String,
    },
    /// Delete a file or directory (feature "fs2").
    #[serde(rename = "rm")]
    Remove {
        #[serde(default)]
        id: Option<u32>,
        path: String,
    },
    /// Rename inside the same directory (feature "fs2").
    #[serde(rename = "mv")]
    Rename {
        #[serde(default)]
        id: Option<u32>,
        path: String,
        name: String,
    },
    /// Put these agent paths on the agent's clipboard as files (feature "clipfiles").
    #[serde(rename = "clip_files")]
    ClipFiles {
        #[serde(default)]
        id: Option<u32>,
        paths: Vec<String>,
    },
    /// The viewer stopped (`on: true`) or resumed sending input; shown in the connection
    /// manager (feature "view_only").
    #[serde(rename = "view_only")]
    ViewOnly { on: bool },
    /// Block the keyboard and mouse of the person at this machine (feature "block_input").
    #[serde(rename = "block_input")]
    BlockInput { on: bool },
    /// Black out this screen for the people in front of it (feature "privacy").
    #[serde(rename = "privacy")]
    Privacy { on: bool },
    /// Lock this machine now (feature "lock").
    #[serde(rename = "lock")]
    Lock,
    /// Lock this machine when the session ends (feature "lock").
    #[serde(rename = "lock_after")]
    LockAfter { on: bool },
    /// Restart this machine (feature "restart").
    #[serde(rename = "restart")]
    Restart,
    /// Keepalive while the viewer blocks input or uses privacy mode, so a viewer that vanished
    /// does not leave the machine blocked.
    #[serde(rename = "ka")]
    Keepalive,
    /// Resend the whole screen (JPEG) or a keyframe (H.264), e.g. after a decoder error.
    #[serde(rename = "refresh", alias = "keyframe")]
    Refresh,
    #[serde(rename = "bye")]
    Bye,
}
