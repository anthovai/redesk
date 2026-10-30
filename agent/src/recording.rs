//! Session recording (docs/PROTOCOL.md §7).
//!
//! Sessions are end-to-end encrypted, so only an endpoint can record them. The controlled
//! machine records what it streams: the already-encoded H.264 frames or JPEG rectangles,
//! pointer positions and, when the session streams sound, the Opus audio packets, with
//! timestamps, into a simple container (`.aerec`). After the session the
//! file is uploaded to the server over verified HTTPS, authenticated by the one-time token the
//! server sent with the session request and signed with the device key. Files that could not be
//! uploaded yet stay in `<config dir>/recordings` and are retried whenever the agent reconnects.
//!
//! ```text
//! "AEGREC1\n"
//! u32 BE header length, JSON header {"v":1,"sid","device_id","hostname","started_ms"}
//! records: u32 BE ms since start, u32 BE payload length, payload (a plaintext channel message)
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::proto;

pub const MAGIC: &[u8; 8] = b"AEGREC1\n";
/// Stop recording beyond this size (the server's default upload limit).
const MAX_BYTES: u64 = 4 << 30;
const MAX_RECORD: usize = 32 << 20;

/// Sent by the server with a session request when the session must be recorded.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordRequest {
    pub token: String,
}

/// Sidecar next to a finished recording: what the upload needs.
#[derive(Serialize, Deserialize)]
struct Pending {
    sid: String,
    token: String,
}

pub fn dir_for(config_path: &Path) -> PathBuf {
    config_path.with_file_name("recordings")
}

fn private_file(path: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

fn private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Whether a streamed message belongs in the recording: video, audio and the pointer, not the
/// clipboard, file chunks or other control traffic. Audio only reaches the recorder when the
/// session streams it (the device allows audio and the viewer has not muted it).
fn wanted(plain: &[u8]) -> bool {
    match plain.first() {
        Some(&proto::MSG_FRAME) | Some(&proto::MSG_VIDEO) => true,
        Some(&proto::MSG_AUDIO) => proto::parse_audio(plain).is_some(),
        Some(&proto::MSG_JSON) => {
            let head = &plain[1..plain.len().min(40)];
            head.windows(12).any(|w| w == br#""t":"cursor""#)
        }
        _ => false,
    }
}

pub struct Recorder {
    out: Option<BufWriter<File>>,
    part: PathBuf,
    done: PathBuf,
    started: Instant,
    written: u64,
}

impl Recorder {
    pub fn start(dir: &Path, sid: &str, device_id: &str, token: &str) -> Result<Self> {
        if sid.is_empty() || !sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            bail!("invalid session id");
        }
        private_dir(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let meta = serde_json::to_vec(&Pending { sid: sid.into(), token: token.into() })?;
        private_file(&dir.join(format!("{sid}.json")))?.write_all(&meta)?;
        let part = dir.join(format!("{sid}.aerec.part"));
        let mut out = BufWriter::with_capacity(256 << 10, private_file(&part)?);
        let started_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let header = serde_json::json!({
            "v": 1,
            "sid": sid,
            "device_id": device_id,
            "hostname": hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
            "os": std::env::consts::OS,
            "started_ms": started_ms,
        })
        .to_string();
        out.write_all(MAGIC)?;
        out.write_all(&(header.len() as u32).to_be_bytes())?;
        out.write_all(header.as_bytes())?;
        let written = (MAGIC.len() + 4 + header.len()) as u64;
        Ok(Self { out: Some(out), part, done: dir.join(format!("{sid}.aerec")), started: Instant::now(), written })
    }

    fn write(&mut self, plain: &[u8]) {
        let Some(out) = self.out.as_mut() else { return };
        if plain.is_empty() || plain.len() > MAX_RECORD {
            return;
        }
        if self.written + 8 + plain.len() as u64 > MAX_BYTES {
            tracing::warn!("recording reached its size limit; the rest of the session is not recorded");
            self.out = None;
            return;
        }
        let t = self.started.elapsed().as_millis().min(u32::MAX as u128) as u32;
        let res = out
            .write_all(&t.to_be_bytes())
            .and_then(|_| out.write_all(&(plain.len() as u32).to_be_bytes()))
            .and_then(|_| out.write_all(plain));
        match res {
            Ok(()) => self.written += 8 + plain.len() as u64,
            Err(e) => {
                tracing::error!("recording write failed: {e}; the rest of the session is not recorded");
                self.out = None;
            }
        }
    }

    /// Records one outgoing message if it is part of what the viewer sees.
    pub fn record(&mut self, plain: &[u8]) {
        if wanted(plain) {
            self.write(plain);
        }
    }

    /// Closes the file (with an end marker so the idle tail counts) and makes it uploadable.
    pub fn finish(mut self) -> Result<PathBuf> {
        self.write(&proto::json(&serde_json::json!({"t": "end"})));
        if let Some(mut out) = self.out.take() {
            out.flush()?;
            out.get_ref().sync_all()?;
        }
        fs::rename(&self.part, &self.done)?;
        Ok(self.done.clone())
    }
}

/// Truncates a recording left behind by a crash to its last complete record and marks it
/// uploadable. Returns false if the file is not a recording.
fn salvage(part: &Path) -> Result<bool> {
    let mut f = OpenOptions::new().read(true).write(true).open(part)?;
    let mut magic = [0u8; 8];
    let mut n = [0u8; 4];
    if f.read_exact(&mut magic).is_err() || &magic != MAGIC || f.read_exact(&mut n).is_err() {
        return Ok(false);
    }
    let mut good = 12 + u32::from_be_bytes(n) as u64;
    let len = f.metadata()?.len();
    if good > len {
        return Ok(false);
    }
    let mut hdr = [0u8; 8];
    loop {
        f.seek(SeekFrom::Start(good))?;
        if f.read_exact(&mut hdr).is_err() {
            break;
        }
        let size = u32::from_be_bytes(hdr[4..].try_into().unwrap()) as u64;
        if size == 0 || size > MAX_RECORD as u64 || good + 8 + size > len {
            break;
        }
        good += 8 + size;
    }
    f.set_len(good)?;
    f.sync_all()?;
    Ok(true)
}

/// Makes recordings interrupted by a crash or power loss uploadable. Call before any session
/// starts (it treats every `.part` file as abandoned).
pub fn recover(config_path: &Path) {
    let dir = dir_for(config_path);
    let Ok(entries) = fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(sid) = name.strip_suffix(".aerec.part") else { continue };
        match salvage(&path) {
            Ok(true) => {
                let _ = fs::rename(&path, dir.join(format!("{sid}.aerec")));
                tracing::info!("recovered interrupted recording of session {sid}");
            }
            _ => {
                let _ = fs::remove_file(&path);
                let _ = fs::remove_file(dir.join(format!("{sid}.json")));
            }
        }
    }
}

/// Upload outcome for one file.
enum Outcome {
    Stored,
    /// The server refused it for good (expired token, bad file, ...): drop it.
    Rejected(String),
    /// Network or server trouble: try again later.
    Retry(String),
}

fn upload_one(server: &str, device_id: &str, key: &SigningKey, sid: &str, token: &str, path: &Path) -> Outcome {
    let sum = match File::open(path).and_then(|mut f| {
        let mut h = Sha256::new();
        std::io::copy(&mut f, &mut h)?;
        Ok(h.finalize())
    }) {
        Ok(s) => s.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        Err(e) => return Outcome::Retry(format!("read: {e}")),
    };
    let sig = key.sign(format!("aegiron-recording-v1\n{device_id}\n{sid}\n{sum}").as_bytes());
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return Outcome::Retry(format!("read: {e}")),
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(6 * 3600)))
        .user_agent(concat!("Aegiron/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let res = agent
        .post(&format!("{server}/api/agent/recordings/{sid}"))
        .header("X-Aegiron", "1")
        .header("Authorization", &format!("Bearer {token}"))
        .header("X-Aegiron-Signature", &B64.encode(sig.to_bytes()))
        .header("Content-Type", "application/octet-stream")
        .send(file);
    match res {
        Ok(mut r) => {
            let status = r.status().as_u16();
            let body = r.body_mut().read_to_string().unwrap_or_default();
            match status {
                200..=299 => Outcome::Stored,
                400 | 401 | 404 | 409 | 413 => Outcome::Rejected(format!("{status} {}", body.trim())),
                _ => Outcome::Retry(format!("{status} {}", body.trim())),
            }
        }
        Err(e) => Outcome::Retry(e.to_string()),
    }
}

/// Uploads one recording file now (blocking), e.g. a test recording made by
/// `examples/make_recording.rs`. Sessions upload through `upload_pending`.
pub fn upload(server: &str, device_id: &str, key: &SigningKey, sid: &str, token: &str, path: &Path) -> Result<()> {
    match upload_one(server.trim_end_matches('/'), device_id, key, sid, token, path) {
        Outcome::Stored => Ok(()),
        Outcome::Rejected(why) | Outcome::Retry(why) => bail!("upload of {sid} failed: {why}"),
    }
}

static UPLOADING: AtomicBool = AtomicBool::new(false);

/// Uploads every finished recording (blocking; run it off the async threads). Only over
/// HTTPS with a verified certificate, or plain HTTP to this machine: never over a link that
/// `allow_insecure_transport` would otherwise permit.
pub fn upload_pending(agent: &crate::signal::Agent) {
    if UPLOADING.swap(true, Ordering::AcqRel) {
        return; // another upload pass is running and will pick everything up
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            UPLOADING.store(false, Ordering::Release);
        }
    }
    let _done = Done;
    let (server, device_id) = {
        let c = agent.config.lock().unwrap();
        (c.server_url.trim_end_matches('/').to_string(), c.id.clone())
    };
    if let Err(e) = crate::config::check_server_url(&server, false) {
        tracing::warn!("not uploading session recordings: {e}");
        return;
    }
    let dir = dir_for(&agent.config_path);
    let Ok(entries) = fs::read_dir(&dir) else { return };
    let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "aerec")).collect();
    files.sort();
    for path in files {
        let Some(sid) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else { continue };
        let meta_path = dir.join(format!("{sid}.json"));
        let pending: Option<Pending> = fs::read(&meta_path).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let Some(p) = pending.filter(|p| p.sid == sid) else {
            tracing::warn!("recording {sid} has no upload credentials; removing it");
            let _ = fs::remove_file(&path);
            continue;
        };
        match upload_one(&server, &device_id, &agent.identity, &sid, &p.token, &path) {
            Outcome::Stored => {
                tracing::info!("uploaded recording of session {sid}");
                let _ = fs::remove_file(&path);
                let _ = fs::remove_file(&meta_path);
            }
            Outcome::Rejected(why) => {
                tracing::warn!("server rejected recording of session {sid}: {why}; removing it");
                agent.report(&sid, "recording_failed", serde_json::json!({"stage": "upload", "error": why}));
                let _ = fs::remove_file(&path);
                let _ = fs::remove_file(&meta_path);
            }
            Outcome::Retry(why) => {
                tracing::warn!("recording upload for session {sid} failed: {why}; will retry");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(bytes: &[u8]) -> Vec<(u32, Vec<u8>)> {
        let n = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let mut at = 12 + n;
        let mut out = Vec::new();
        while at + 8 <= bytes.len() {
            let t = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
            let len = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            out.push((t, bytes[at + 8..at + 8 + len].to_vec()));
            at += 8 + len;
        }
        out
    }

    #[test]
    fn records_video_audio_and_pointer_only() {
        let root = std::env::temp_dir().join(format!("aegiron-rec-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = dir_for(&root.join("agent.toml"));
        let mut r = Recorder::start(&dir, "sid_1", "123456789", "tok").unwrap();
        r.record(&proto::video(1, true, 64, 48, &[0, 0, 0, 1, 0x65]));
        r.record(&proto::json(&serde_json::json!({"t": "clip", "s": "secret"})));
        r.record(&proto::file_chunk(1, b"file data"));
        r.record(&proto::json(&serde_json::json!({"t": "cursor", "x": 0.5, "y": 0.5, "visible": true})));
        r.record(&proto::audio(proto::AUDIO_OPUS, 7, &[0xfc, 0xff, 0xfe]));
        r.record(&[proto::MSG_AUDIO, proto::AUDIO_OPUS, 0, 0]); // malformed: not recorded
        let mut jpeg = proto::frame_header(0, 0, 64, 48);
        jpeg.extend_from_slice(&[0xff, 0xd8]);
        r.record(&jpeg);
        let path = r.finish().unwrap();
        assert!(dir.join("sid_1.json").is_file() && !dir.join("sid_1.aerec.part").exists());

        let bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], MAGIC);
        let header: serde_json::Value = serde_json::from_slice(&bytes[12..12 + u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize]).unwrap();
        assert_eq!(header["sid"], "sid_1");
        assert_eq!(header["device_id"], "123456789");
        let recs = records(&bytes);
        let types: Vec<u8> = recs.iter().map(|(_, p)| p[0]).collect();
        assert_eq!(types, vec![proto::MSG_VIDEO, proto::MSG_JSON, proto::MSG_AUDIO, proto::MSG_FRAME, proto::MSG_JSON]);
        assert!(recs.windows(2).all(|w| w[0].0 <= w[1].0));
        assert_eq!(proto::parse_audio(&recs[2].1), Some((proto::AUDIO_OPUS, 7, &[0xfc, 0xff, 0xfe][..])));
        assert!(String::from_utf8_lossy(&recs[4].1).contains("\"end\""));
        assert!(!bytes.windows(6).any(|w| w == b"secret"));

        // A crash mid-record: salvage keeps the complete records only.
        let part = dir.join("sid_2.aerec.part");
        fs::write(&part, &bytes[..bytes.len() - 3]).unwrap();
        fs::write(dir.join("sid_2.json"), b"{}").unwrap();
        recover(&root.join("agent.toml"));
        let salvaged = fs::read(dir.join("sid_2.aerec")).unwrap();
        assert_eq!(records(&salvaged).len(), 4);
        fs::remove_dir_all(&root).unwrap();
    }
}
