//! File transfer inside the encrypted session: directory listing, upload (viewer → agent)
//! and download (agent → viewer). Chunks travel as MSG_FILE frames tagged with a transfer id.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use crate::proto;

const CHUNK: usize = 64 * 1024;
const MAX_UPLOAD: u64 = 8 << 30;
const MAX_PARALLEL: usize = 8;
const MAX_ENTRIES: usize = 5000;

struct Upload {
    file: std::fs::File,
    path: PathBuf,
    size: u64,
    written: u64,
}

pub struct Files {
    uploads: HashMap<u32, Upload>,
}

fn err(op: &str, id: Option<u32>, e: impl std::fmt::Display) -> Value {
    json!({"t": "fs_error", "op": op, "id": id, "message": e.to_string()})
}

impl Files {
    pub fn new() -> Self {
        Self { uploads: HashMap::new() }
    }

    pub fn list(&self, path: &str) -> Value {
        let dir = if path.is_empty() { dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")) } else { PathBuf::from(path) };
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => return err("ls", None, format!("{}: {e}", dir.display())),
        };
        let mut entries: Vec<Value> = Vec::new();
        for e in rd.flatten().take(MAX_ENTRIES) {
            // Follow symlinks for type/size, but still list broken ones.
            let meta = std::fs::metadata(e.path()).or_else(|_| e.metadata());
            let Ok(m) = meta else { continue };
            let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
            entries.push(json!({
                "name": e.file_name().to_string_lossy(),
                "dir": m.is_dir(),
                "size": if m.is_dir() { 0 } else { m.len() },
                "mtime": mtime,
            }));
        }
        entries.sort_by(|a, b| {
            (!a["dir"].as_bool().unwrap_or(false), a["name"].as_str().unwrap_or("").to_lowercase())
                .cmp(&(!b["dir"].as_bool().unwrap_or(false), b["name"].as_str().unwrap_or("").to_lowercase()))
        });
        json!({
            "t": "ls",
            "path": dir.display().to_string(),
            "parent": dir.parent().map(|p| p.display().to_string()),
            "sep": std::path::MAIN_SEPARATOR.to_string(),
            "entries": entries,
        })
    }

    pub fn put(&mut self, id: u32, dir: &str, name: &str, size: u64) -> Value {
        if self.uploads.len() >= MAX_PARALLEL {
            return err("put", Some(id), "too many parallel uploads");
        }
        if size > MAX_UPLOAD {
            return err("put", Some(id), "file too large");
        }
        let dir = if dir.is_empty() { default_upload_dir() } else { PathBuf::from(dir) };
        if !dir.is_dir() {
            return err("put", Some(id), format!("{} is not a directory", dir.display()));
        }
        let path = unique_path(&dir, &sanitize(name));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                let reply = json!({"t": "put_ok", "id": id, "path": path.display().to_string()});
                self.uploads.insert(id, Upload { file, path, size, written: 0 });
                reply
            }
            Err(e) => err("put", Some(id), e),
        }
    }

    /// Writes a chunk; returns an error message to send back if the upload failed.
    pub fn chunk(&mut self, id: u32, data: &[u8]) -> Option<Value> {
        let up = self.uploads.get_mut(&id)?; // unknown id: put already failed and was reported
        let res = if up.written + data.len() as u64 > up.size {
            Err(std::io::Error::other("more data than announced"))
        } else {
            up.file.write_all(data)
        };
        match res {
            Ok(()) => {
                up.written += data.len() as u64;
                None
            }
            Err(e) => {
                let up = self.uploads.remove(&id).unwrap();
                let _ = std::fs::remove_file(&up.path);
                Some(err("put", Some(id), e))
            }
        }
    }

    /// Finishes an upload. Returns (reply, Some((path, size)) on success).
    pub fn put_end(&mut self, id: u32) -> (Value, Option<(String, u64)>) {
        let Some(mut up) = self.uploads.remove(&id) else { return (err("put", Some(id), "unknown transfer"), None) };
        if up.written != up.size {
            let _ = std::fs::remove_file(&up.path);
            return (err("put", Some(id), format!("incomplete: {} of {} bytes", up.written, up.size)), None);
        }
        if let Err(e) = up.file.flush() {
            return (err("put", Some(id), e), None);
        }
        let path = up.path.display().to_string();
        (json!({"t": "put_done", "id": id, "path": path}), Some((path, up.size)))
    }

    pub fn cancel(&mut self, id: u32) {
        if let Some(up) = self.uploads.remove(&id) {
            let _ = std::fs::remove_file(&up.path);
        }
    }

    /// Streams a file to the viewer on a background task.
    pub fn get(&self, id: u32, path: String, out: mpsc::Sender<Vec<u8>>) {
        tokio::spawn(async move {
            if let Err(e) = send_file(id, &path, &out).await {
                let _ = out.send(proto::json(&err("get", Some(id), e))).await;
            }
        });
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        // Session ended mid-upload: don't leave partial files behind.
        for (_, up) in self.uploads.drain() {
            let _ = std::fs::remove_file(&up.path);
        }
    }
}

async fn send_file(id: u32, path: &str, out: &mpsc::Sender<Vec<u8>>) -> std::io::Result<()> {
    let mut f = tokio::fs::File::open(path).await?;
    let meta = f.metadata().await?;
    if meta.is_dir() {
        return Err(std::io::Error::other("cannot download a directory"));
    }
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let closed = || std::io::Error::other("session closed");
    out.send(proto::json(&json!({"t": "get_start", "id": id, "name": name, "size": meta.len()}))).await.map_err(|_| closed())?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        out.send(proto::file_chunk(id, &buf[..n])).await.map_err(|_| closed())?;
    }
    out.send(proto::json(&json!({"t": "get_end", "id": id}))).await.map_err(|_| closed())?;
    Ok(())
}

pub fn default_upload_dir() -> PathBuf {
    let d = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from(".")).join("ReDesk");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Keeps only the final path component and strips characters that are unsafe in file names.
fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() { "file".into() } else { clean.chars().take(200).collect() }
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_paths() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("C:\\Users\\a\\report.pdf"), "report.pdf");
        assert_eq!(sanitize(".hidden"), "hidden");
        assert_eq!(sanitize(".."), "file");
    }

    #[test]
    fn upload_round_trip() {
        let dir = std::env::temp_dir().join(format!("redesk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = Files::new();
        let d = dir.display().to_string();
        assert_eq!(f.put(1, &d, "a.txt", 5)["t"], "put_ok");
        assert!(f.chunk(1, b"hello").is_none());
        let (reply, ok) = f.put_end(1);
        assert_eq!(reply["t"], "put_done");
        assert_eq!(std::fs::read_to_string(ok.unwrap().0).unwrap(), "hello");
        // Same name again gets a suffix instead of overwriting.
        assert!(f.put(2, &d, "a.txt", 1)["path"].as_str().unwrap().ends_with("a (1).txt"));
        drop(f); // removes the unfinished upload
        assert!(!dir.join("a (1).txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
