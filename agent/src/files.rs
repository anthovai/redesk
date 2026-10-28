//! File transfer inside the encrypted session: directory listing, upload (viewer → agent),
//! download (agent → viewer), whole folders both ways and simple file management (new folder,
//! rename, delete). Chunks travel as MSG_FILE frames tagged with a transfer id.
//!
//! Relative paths that arrive from the other side (`put.rel`, `mkdir.name`, and on the viewer
//! `dir_file.rel`) are untrusted: [`rel_components`] and [`ensure_dirs`] are shared by the agent
//! and the native viewer so both sides apply the same rules.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use crate::proto;

const CHUNK: usize = 64 * 1024;
const MAX_UPLOAD: u64 = 8 << 30;
const MAX_PARALLEL: usize = 8;
const MAX_ENTRIES: usize = 5000;
/// Limits for one folder download / upload.
pub const MAX_TREE_ENTRIES: usize = 100_000;
pub const MAX_DEPTH: usize = 64;
/// Upper bound on the size of the `dirs` list in `dir_start` (the relay caps frames at 32 MiB).
const MAX_DIRS_BYTES: usize = 8 << 20;

/// Optional protocol capabilities this agent offers (`info.features`).
pub const FEATURES: [&str; 2] = ["fs2", "clipfiles"];

struct Upload {
    file: std::fs::File,
    path: PathBuf,
    size: u64,
    written: u64,
}

pub struct Files {
    uploads: HashMap<u32, Upload>,
    /// Downloads the viewer cancelled; the sending task checks this between chunks.
    cancelled: Arc<Mutex<HashSet<u32>>>,
}

fn err(op: &str, id: Option<u32>, e: impl std::fmt::Display) -> Value {
    json!({"t": "fs_error", "op": op, "id": id, "message": e.to_string()})
}

fn io_err(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::other(msg.into())
}

impl Default for Files {
    fn default() -> Self {
        Self::new()
    }
}

impl Files {
    pub fn new() -> Self {
        Self { uploads: HashMap::new(), cancelled: Arc::default() }
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

    /// Starts an upload. Without `rel` only the file name of `name` is kept; with `rel` the file
    /// goes to that relative path inside `dir`, creating directories on the way.
    pub fn put(&mut self, id: u32, dir: &str, name: &str, size: u64, rel: Option<&str>) -> Value {
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
        let path = match rel {
            Some(rel) => match rel_components(rel).and_then(|c| {
                let parent = ensure_dirs(&dir, &c[..c.len() - 1])?;
                Ok(unique_path(&parent, &c[c.len() - 1]))
            }) {
                Ok(p) => p,
                Err(e) => return err("put", Some(id), e),
            },
            None => unique_path(&dir, &sanitize(name)),
        };
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

    /// Cancels an upload (removing the partial file) or a running download.
    pub fn cancel(&mut self, id: u32) {
        if let Some(up) = self.uploads.remove(&id) {
            let _ = std::fs::remove_file(&up.path);
        } else {
            self.cancelled.lock().unwrap().insert(id);
        }
    }

    /// Streams a file to the viewer on a background task.
    pub fn get(&self, id: u32, path: String, out: mpsc::Sender<Vec<u8>>) {
        let cancelled = self.cancelled.clone();
        tokio::spawn(async move {
            if let Err(e) = send_file(id, &path, &out, &cancelled).await {
                let _ = out.send(proto::json(&err("get", Some(id), e))).await;
            }
            cancelled.lock().unwrap().remove(&id);
        });
    }

    /// Streams a whole directory tree to the viewer on a background task.
    pub fn get_dir(&self, id: u32, path: String, out: mpsc::Sender<Vec<u8>>) {
        let cancelled = self.cancelled.clone();
        tokio::spawn(async move {
            if let Err(e) = send_dir(id, &path, &out, &cancelled).await {
                let _ = out.send(proto::json(&err("get_dir", Some(id), e))).await;
            }
            cancelled.lock().unwrap().remove(&id);
        });
    }

    /// Creates a directory: `name` may be a relative path; the last component is made unique.
    pub fn mkdir(&self, id: Option<u32>, dir: &str, name: &str) -> Value {
        let dir = if dir.is_empty() { default_upload_dir() } else { PathBuf::from(dir) };
        let res = rel_components(name).and_then(|c| {
            let parent = ensure_dirs(&dir, &c[..c.len() - 1])?;
            let target = unique_path(&parent, &c[c.len() - 1]);
            std::fs::create_dir(&target)?;
            Ok(target)
        });
        match res {
            Ok(p) => json!({"t": "fs_ok", "op": "mkdir", "id": id, "path": p.display().to_string()}),
            Err(e) => err("mkdir", id, e),
        }
    }

    /// Deletes a file or a whole directory (symlinks are removed, never followed).
    pub fn rm(&self, id: Option<u32>, path: &str) -> (Value, bool) {
        match remove(Path::new(path)) {
            Ok(()) => (json!({"t": "fs_ok", "op": "rm", "id": id, "path": path}), true),
            Err(e) => (err("rm", id, e), false),
        }
    }

    /// Renames inside the same directory; never replaces an existing entry.
    pub fn mv(&self, id: Option<u32>, path: &str, name: &str) -> (Value, Option<String>) {
        match rename(Path::new(path), name) {
            Ok(p) => {
                let p = p.display().to_string();
                (json!({"t": "fs_ok", "op": "mv", "id": id, "path": p}), Some(p))
            }
            Err(e) => (err("mv", id, e), None),
        }
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

fn is_cancelled(set: &Mutex<HashSet<u32>>, id: u32) -> bool {
    set.lock().unwrap().contains(&id)
}

async fn send_file(id: u32, path: &str, out: &mpsc::Sender<Vec<u8>>, cancelled: &Mutex<HashSet<u32>>) -> std::io::Result<()> {
    let mut f = tokio::fs::File::open(path).await?;
    let meta = f.metadata().await?;
    if meta.is_dir() {
        return Err(io_err("cannot download a directory"));
    }
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let closed = || io_err("session closed");
    out.send(proto::json(&json!({"t": "get_start", "id": id, "name": name, "size": meta.len()}))).await.map_err(|_| closed())?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if is_cancelled(cancelled, id) {
            return Ok(());
        }
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        out.send(proto::file_chunk(id, &buf[..n])).await.map_err(|_| closed())?;
    }
    out.send(proto::json(&json!({"t": "get_end", "id": id}))).await.map_err(|_| closed())?;
    Ok(())
}

/// Files and directories of a tree, relative to its root with `/` separators.
#[derive(Debug, Default)]
pub struct Tree {
    pub dirs: Vec<String>,
    pub files: Vec<(String, PathBuf, u64)>,
}

impl Tree {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.2).sum()
    }

    /// Directories that contain nothing else from the tree (created explicitly on upload).
    pub fn empty_dirs(&self) -> Vec<&str> {
        self.dirs
            .iter()
            .filter(|d| {
                let prefix = format!("{d}/");
                !self.dirs.iter().any(|o| o.starts_with(&prefix)) && !self.files.iter().any(|f| f.0.starts_with(&prefix))
            })
            .map(String::as_str)
            .collect()
    }
}

/// Walks a directory without following symlinks (they and other special files are skipped).
pub fn walk(root: &Path) -> std::io::Result<Tree> {
    let mut tree = Tree::default();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    let mut count = 0usize;
    while let Some((dir, rel, depth)) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            count += 1;
            if count > MAX_TREE_ENTRIES {
                return Err(io_err(format!("too many files (more than {MAX_TREE_ENTRIES})")));
            }
            let Ok(ft) = e.file_type() else { continue }; // does not follow symlinks
            let name = e.file_name().to_string_lossy().into_owned();
            let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if ft.is_dir() {
                if depth + 1 >= MAX_DEPTH {
                    return Err(io_err("folder nesting is too deep"));
                }
                tree.dirs.push(r.clone());
                stack.push((e.path(), r, depth + 1));
            } else if ft.is_file() {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                tree.files.push((r, e.path(), size));
            }
        }
    }
    tree.dirs.sort();
    tree.files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(tree)
}

async fn send_dir(id: u32, path: &str, out: &mpsc::Sender<Vec<u8>>, cancelled: &Mutex<HashSet<u32>>) -> std::io::Result<()> {
    let root = PathBuf::from(path);
    let meta = tokio::fs::symlink_metadata(&root).await?;
    if !meta.is_dir() {
        return Err(io_err("not a directory"));
    }
    let walk_root = root.clone();
    let tree = tokio::task::spawn_blocking(move || walk(&walk_root)).await.map_err(io_err_join)??;
    if tree.dirs.iter().map(|d| d.len() + 4).sum::<usize>() > MAX_DIRS_BYTES {
        return Err(io_err("too many folders"));
    }
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "folder".into());
    let closed = || io_err("session closed");
    let start = json!({"t": "dir_start", "id": id, "name": name, "files": tree.files.len(), "size": tree.size(), "dirs": tree.dirs});
    out.send(proto::json(&start)).await.map_err(|_| closed())?;
    let mut buf = vec![0u8; CHUNK];
    for (rel, file, _) in &tree.files {
        if is_cancelled(cancelled, id) {
            return Ok(());
        }
        let opened = async {
            let f = tokio::fs::File::open(file).await?;
            let len = f.metadata().await?.len();
            Ok::<_, std::io::Error>((f, len))
        }
        .await;
        let (f, len) = match opened {
            Ok(x) => x,
            Err(e) => {
                out.send(proto::json(&json!({"t": "dir_skip", "id": id, "rel": rel, "message": e.to_string()}))).await.map_err(|_| closed())?;
                continue;
            }
        };
        out.send(proto::json(&json!({"t": "dir_file", "id": id, "rel": rel, "size": len}))).await.map_err(|_| closed())?;
        // Never send more than announced, even if the file grows meanwhile.
        let mut f = f.take(len);
        loop {
            if is_cancelled(cancelled, id) {
                return Ok(());
            }
            let n = f.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            out.send(proto::file_chunk(id, &buf[..n])).await.map_err(|_| closed())?;
        }
    }
    out.send(proto::json(&json!({"t": "get_end", "id": id}))).await.map_err(|_| closed())?;
    Ok(())
}

fn io_err_join(e: tokio::task::JoinError) -> std::io::Error {
    io_err(e.to_string())
}

pub fn default_upload_dir() -> PathBuf {
    let d = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from(".")).join("Aegiron");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Keeps only the final path component and strips characters that are unsafe in file names.
pub fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() { "file".into() } else { clean.chars().take(200).collect() }
}

/// Splits an untrusted relative path (`/` or `\` separated) into safe components.
///
/// Rejects empty, absolute, drive- or UNC-prefixed paths and `.`/`..` (or all-dot) components;
/// replaces characters that are invalid on Windows with `_`, as well as a trailing dot or space,
/// and prefixes reserved Windows device names (`CON`, `NUL`, `COM1`…) with `_`.
pub fn rel_components(rel: &str) -> std::io::Result<Vec<String>> {
    let bad = |why: &str| Err(io_err(format!("unsafe path {rel:?}: {why}")));
    if rel.is_empty() {
        return bad("empty");
    }
    if rel.starts_with(['/', '\\']) {
        return bad("absolute");
    }
    let b = rel.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return bad("drive prefix");
    }
    let mut out = Vec::new();
    for c in rel.split(['/', '\\']) {
        if c.is_empty() {
            return bad("empty component");
        }
        if c.chars().all(|ch| ch == '.') {
            return bad("dot component");
        }
        let mut s: String = c.chars().map(|ch| if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { ch }).collect();
        if s.ends_with(['.', ' ']) {
            s.pop();
            s.push('_');
        }
        let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            s.insert(0, '_');
        }
        if s.len() > 255 {
            return bad("name too long");
        }
        out.push(s);
    }
    if out.len() > MAX_DEPTH {
        return bad("too deep");
    }
    Ok(out)
}

/// Creates `root/c1/c2/…` one level at a time and returns it. An existing component that is a
/// symlink or not a directory is an error, so nothing can be redirected outside `root`.
pub fn ensure_dirs(root: &Path, comps: &[String]) -> std::io::Result<PathBuf> {
    let mut p = root.to_path_buf();
    for c in comps {
        p.push(c);
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => return Err(io_err(format!("{} is a symbolic link", p.display()))),
            Ok(m) if !m.is_dir() => return Err(io_err(format!("{} is not a directory", p.display()))),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match std::fs::create_dir(&p) {
                Ok(()) => {}
                // Created concurrently: accept only a real directory.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let m = std::fs::symlink_metadata(&p)?;
                    if m.file_type().is_symlink() || !m.is_dir() {
                        return Err(io_err(format!("{} is not a directory", p.display())));
                    }
                }
                Err(e) => return Err(e),
            },
            Err(e) => return Err(e),
        }
    }
    Ok(p)
}

/// `dir/name`, or `dir/name (1).ext`, `(2)`… if taken (a dangling symlink counts as taken).
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let taken = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let p = dir.join(name);
    if !taken(&p) {
        return p;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !taken(p)).unwrap()
}

fn remove(p: &Path) -> std::io::Result<()> {
    if !p.is_absolute() || p.parent().is_none() {
        return Err(io_err("refusing to delete this path"));
    }
    let meta = std::fs::symlink_metadata(p)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return std::fs::remove_file(p);
    }
    let real = p.canonicalize()?;
    let home = dirs::home_dir().and_then(|h| h.canonicalize().ok());
    if real.parent().is_none() || Some(&real) == home.as_ref() {
        return Err(io_err("refusing to delete this folder"));
    }
    // remove_dir_all does not follow symlinks inside the tree.
    std::fs::remove_dir_all(p)
}

fn rename(p: &Path, name: &str) -> std::io::Result<PathBuf> {
    let comps = rel_components(name)?;
    if comps.len() != 1 {
        return Err(io_err("a name cannot contain / or \\"));
    }
    std::fs::symlink_metadata(p)?;
    let parent = p.parent().filter(|_| p.is_absolute()).ok_or_else(|| io_err("bad path"))?;
    let target = parent.join(&comps[0]);
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(io_err(format!("{} already exists", comps[0])));
    }
    std::fs::rename(p, &target)?;
    Ok(target)
}

/// Folder name prefix (under the temp dir) where the desktop viewer keeps files it fetched from
/// a remote clipboard.
pub const CLIP_TMP_PREFIX: &str = "aegiron-clip-";

/// True for a file the desktop viewer on this same machine fetched from a remote clipboard.
/// The agent does not offer those back (it would loop when a machine controls itself).
pub fn is_viewer_clip_copy(p: &Path) -> bool {
    let tmp = std::env::temp_dir();
    let tmp = tmp.canonicalize().unwrap_or(tmp);
    p.strip_prefix(&tmp).ok().and_then(|r| r.components().next()).is_some_and(|c| c.as_os_str().to_string_lossy().starts_with(CLIP_TMP_PREFIX))
}

/// One entry of a clipboard file list (`clip_files`).
pub fn clip_entry(p: &Path) -> Value {
    let meta = std::fs::metadata(p).ok();
    let dir = meta.as_ref().is_some_and(|m| m.is_dir());
    json!({
        "name": p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        "path": p.display().to_string(),
        "dir": dir,
        "size": if dir { 0 } else { meta.map(|m| m.len()).unwrap_or(0) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aegiron-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitize_strips_paths() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("C:\\Users\\a\\report.pdf"), "report.pdf");
        assert_eq!(sanitize(".hidden"), "hidden");
        assert_eq!(sanitize(".."), "file");
    }

    #[test]
    fn rel_components_rejects_escapes() {
        for bad in [
            "", "/etc/passwd", "\\\\server\\share\\x", "\\x", "C:\\x", "c:x", "../x", "a/../../x", "a/./b", "a//b", "a/", "...", "a\\..\\b",
        ] {
            assert!(rel_components(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(rel_components("a/".repeat(MAX_DEPTH + 1).trim_end_matches('/')).is_err());
        assert!(rel_components(&"x".repeat(256)).is_err());
    }

    #[test]
    fn rel_components_cleans_names() {
        assert_eq!(rel_components("Photos/2024/a.jpg").unwrap(), ["Photos", "2024", "a.jpg"]);
        assert_eq!(rel_components("a\\b.txt").unwrap(), ["a", "b.txt"]);
        assert_eq!(rel_components(".git/config").unwrap(), [".git", "config"]);
        assert_eq!(rel_components("we:ird?/x*y|z\u{1}").unwrap(), ["we_ird_", "x_y_z_"]);
        assert_eq!(rel_components("trail./sp ").unwrap(), ["trail_", "sp_"]);
        assert_eq!(rel_components("con/NUL.txt/com1/COM10").unwrap(), ["_con", "_NUL.txt", "_com1", "COM10"]);
    }

    #[test]
    fn upload_round_trip() {
        let dir = scratch("put");
        let mut f = Files::new();
        let d = dir.display().to_string();
        assert_eq!(f.put(1, &d, "a.txt", 5, None)["t"], "put_ok");
        assert!(f.chunk(1, b"hello").is_none());
        let (reply, ok) = f.put_end(1);
        assert_eq!(reply["t"], "put_done");
        assert_eq!(std::fs::read_to_string(ok.unwrap().0).unwrap(), "hello");
        // Same name again gets a suffix instead of overwriting.
        assert!(f.put(2, &d, "a.txt", 1, None)["path"].as_str().unwrap().ends_with("a (1).txt"));
        drop(f); // removes the unfinished upload
        assert!(!dir.join("a (1).txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn folder_upload_creates_tree_and_never_overwrites() {
        let dir = scratch("tree");
        let d = dir.display().to_string();
        let mut f = Files::new();
        let root = f.mkdir(Some(1), &d, "Photos");
        assert_eq!(root["t"], "fs_ok");
        let root = root["path"].as_str().unwrap().to_string();
        assert!(root.ends_with("Photos"));
        // A second folder with the same name is not merged into the first.
        assert!(f.mkdir(Some(2), &d, "Photos")["path"].as_str().unwrap().ends_with("Photos (1)"));

        assert_eq!(f.put(3, &root, "", 3, Some("2024/jan/a.txt"))["t"], "put_ok");
        assert!(f.chunk(3, b"abc").is_none());
        assert_eq!(f.put_end(3).0["t"], "put_done");
        assert_eq!(std::fs::read(dir.join("Photos/2024/jan/a.txt")).unwrap(), b"abc");
        // Same relative path again: suffixed, original untouched.
        assert!(f.put(4, &root, "", 0, Some("2024/jan/a.txt"))["path"].as_str().unwrap().ends_with("a (1).txt"));
        assert_eq!(f.put_end(4).0["t"], "put_done");
        assert_eq!(std::fs::read(dir.join("Photos/2024/jan/a.txt")).unwrap(), b"abc");
        // Empty sub-folder.
        assert_eq!(f.mkdir(None, &root, "empty/inner")["t"], "fs_ok");
        assert!(dir.join("Photos/empty/inner").is_dir());

        // Escapes are refused.
        for rel in ["../x.txt", "/tmp/x.txt", "a/../../x.txt", "C:\\x.txt"] {
            assert_eq!(f.put(9, &root, "", 1, Some(rel))["t"], "fs_error", "{rel}");
        }
        assert_eq!(f.mkdir(None, &root, "../evil")["t"], "fs_error");
        assert!(!dir.join("x.txt").exists() && !dir.join("evil").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_redirect_writes() {
        let dir = scratch("symlink");
        let outside = scratch("symlink-outside");
        let root = dir.join("root");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let mut f = Files::new();
        let r = root.display().to_string();
        let reply = f.put(1, &r, "", 1, Some("link/pwned.txt"));
        assert_eq!(reply["t"], "fs_error");
        assert!(reply["message"].as_str().unwrap().contains("symbolic link"));
        assert_eq!(f.mkdir(None, &r, "link/sub")["t"], "fs_error");
        assert!(ensure_dirs(&root, &["link".into(), "x".into()]).is_err());
        // A file where a directory is expected.
        std::fs::write(root.join("file"), b"").unwrap();
        assert_eq!(f.put(2, &r, "", 1, Some("file/x.txt"))["t"], "fs_error");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);

        // Downloads skip symlinks instead of following them.
        std::fs::write(outside.join("secret"), b"s").unwrap();
        std::fs::create_dir(root.join("d")).unwrap();
        std::fs::write(root.join("d/ok.txt"), b"ok").unwrap();
        let tree = walk(&root).unwrap();
        assert_eq!(tree.dirs, ["d"]);
        assert_eq!(tree.files.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["d/ok.txt", "file"]);

        // Deleting a symlink removes the link, not the target.
        let (reply, ok) = f.rm(None, &root.join("link").display().to_string());
        assert!(ok, "{reply}");
        assert!(outside.join("secret").exists());
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn walk_reports_empty_dirs() {
        let dir = scratch("walk");
        std::fs::create_dir_all(dir.join("a/b/c")).unwrap();
        std::fs::create_dir_all(dir.join("e")).unwrap();
        std::fs::write(dir.join("a/f.txt"), b"12345").unwrap();
        let t = walk(&dir).unwrap();
        assert_eq!(t.dirs, ["a", "a/b", "a/b/c", "e"]);
        assert_eq!(t.size(), 5);
        assert_eq!(t.empty_dirs(), ["a/b/c", "e"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rename_and_delete() {
        let dir = scratch("ops");
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        std::fs::write(dir.join("b.txt"), b"b").unwrap();
        std::fs::create_dir_all(dir.join("sub/deep")).unwrap();
        std::fs::write(dir.join("sub/deep/x"), b"x").unwrap();
        let f = Files::new();
        let a = dir.join("a.txt").display().to_string();
        // Never replaces an existing entry; no path separators in the new name.
        assert_eq!(f.mv(None, &a, "b.txt").0["t"], "fs_error");
        assert_eq!(f.mv(None, &a, "../a.txt").0["t"], "fs_error");
        assert_eq!(f.mv(None, &a, "sub/a.txt").0["t"], "fs_error");
        let (reply, new) = f.mv(Some(7), &a, "c.txt");
        assert_eq!(reply["t"], "fs_ok");
        assert_eq!(reply["id"], 7);
        assert!(new.unwrap().ends_with("c.txt") && dir.join("c.txt").exists() && !dir.join("a.txt").exists());

        assert!(f.rm(None, &dir.join("sub").display().to_string()).1);
        assert!(!dir.join("sub").exists());
        assert!(f.rm(None, &dir.join("c.txt").display().to_string()).1);
        assert!(!f.rm(None, "relative/path").1);
        assert!(!f.rm(None, "/").1);
        if let Some(home) = dirs::home_dir() {
            assert!(!f.rm(None, &home.display().to_string()).1);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn folder_download_streams_every_file() {
        let dir = scratch("getdir");
        std::fs::create_dir_all(dir.join("src/empty")).unwrap();
        std::fs::write(dir.join("src/a.txt"), b"hello").unwrap();
        std::fs::create_dir_all(dir.join("src/n")).unwrap();
        std::fs::write(dir.join("src/n/b.bin"), vec![7u8; 150_000]).unwrap();
        let f = Files::new();
        let (tx, mut rx) = mpsc::channel(1000);
        f.get_dir(5, dir.join("src").display().to_string(), tx);
        let mut msgs = Vec::new();
        let mut bytes = 0usize;
        while let Some(m) = rx.recv().await {
            if m[0] == proto::MSG_FILE {
                assert_eq!(u32::from_be_bytes(m[1..5].try_into().unwrap()), 5);
                bytes += m.len() - 5;
                continue;
            }
            let v: Value = serde_json::from_slice(&m[1..]).unwrap();
            let end = v["t"] == "get_end";
            msgs.push(v);
            if end {
                break;
            }
        }
        assert_eq!(msgs[0]["t"], "dir_start");
        assert_eq!(msgs[0]["files"], 2);
        assert_eq!(msgs[0]["size"], 150_005);
        assert_eq!(msgs[0]["dirs"], json!(["empty", "n"]));
        assert_eq!(msgs[1]["rel"], "a.txt");
        assert_eq!(msgs[2]["rel"], "n/b.bin");
        assert_eq!(bytes, 150_005);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
