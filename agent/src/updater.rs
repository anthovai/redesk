//! Agent updates distributed by the ReDesk server (not GitHub): an admin uploads a vetted
//! build, the app asks whether a newer one exists and downloads it only when the person at
//! the screen agrees. Nothing is installed automatically. A download is kept only after its
//! size and SHA-256 match what the server announced, and TLS is always verified.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::check_server_url;

/// Largest build the server accepts.
pub const MAX_SIZE: u64 = 300 << 20;

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
    /// Safe file name to save as.
    pub filename: String,
    /// Absolute download URL on the same server.
    pub url: String,
    insecure: bool,
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(timeout)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .user_agent(concat!("ReDesk/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Name of this platform as the server knows it.
pub fn current_os() -> &'static str {
    std::env::consts::OS
}

pub fn current_arch() -> &'static str {
    std::env::consts::ARCH
}

#[derive(Deserialize)]
struct Reply {
    available: bool,
    #[serde(default)]
    version: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    filename: String,
    #[serde(default)]
    url: String,
}

/// Asks the server for a build newer than this one. `Ok(None)` = up to date.
pub fn check(server: &str, insecure: bool) -> Result<Option<Release>> {
    let server = server.trim().trim_end_matches('/');
    check_server_url(server, insecure)?;
    let current = env!("CARGO_PKG_VERSION");
    let mut res = agent(Some(Duration::from_secs(15)))
        .get(&format!("{server}/api/agent/update"))
        .query("os", current_os())
        .query("arch", current_arch())
        .query("version", current)
        .call()
        .map_err(|e| anyhow!("เชื่อมต่อเซิร์ฟเวอร์ไม่ได้: {e}"))?;
    match res.status().as_u16() {
        200 => {}
        404 => bail!("เซิร์ฟเวอร์นี้ยังไม่รองรับการอัปเดตโปรแกรม"),
        429 => bail!("ตรวจสอบอัปเดตบ่อยเกินไป กรุณารอสักครู่"),
        s => bail!("เซิร์ฟเวอร์ตอบกลับ {s}"),
    }
    let r: Reply = res.body_mut().read_json().map_err(|e| anyhow!("คำตอบจากเซิร์ฟเวอร์ไม่ถูกต้อง: {e}"))?;
    if !r.available {
        return Ok(None);
    }
    validate(server, insecure, current, r).map(Some)
}

fn validate(server: &str, insecure: bool, current: &str, r: Reply) -> Result<Release> {
    let bad = |what: &str| anyhow!("ข้อมูลอัปเดตจากเซิร์ฟเวอร์ไม่ถูกต้อง ({what})");
    // Never downgrade, even if the server says so.
    match (Version::parse(&r.version), Version::parse(current)) {
        (Some(new), Some(cur)) if new > cur => {}
        (Some(_), Some(_)) => return Err(bad("version is not newer")),
        _ => return Err(bad("version")),
    }
    if r.size == 0 || r.size > MAX_SIZE {
        return Err(bad("size"));
    }
    if r.sha256.len() != 64 || !r.sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(bad("sha256"));
    }
    // Only a download path on this same server; never a foreign URL.
    let path_ok = r.url.starts_with("/api/agent/releases/")
        && r.url.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.'))
        && !r.url.contains("..");
    if !path_ok {
        return Err(bad("url"));
    }
    Ok(Release {
        filename: safe_filename(&r.filename, &r.version),
        version: r.version,
        notes: r.notes.chars().take(4000).collect(),
        size: r.size,
        sha256: r.sha256,
        url: format!("{server}{}", r.url),
        insecure,
    })
}

/// Keeps a plain file name (no directories, no hidden files).
fn safe_filename(name: &str, version: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+') { c } else { '_' })
        .collect();
    let clean = clean.trim_start_matches('.').chars().take(128).collect::<String>();
    if clean.is_empty() || clean == "_" { format!("redesk-{version}") } else { clean }
}

/// Downloads the build into `dest_dir`, streaming to a temporary file and renaming it only
/// after the size and SHA-256 match. Returns the final path; the caller decides what to do
/// with it (show it to the user, open the installer after confirmation).
pub fn download(release: &Release, dest_dir: &Path) -> Result<PathBuf> {
    check_server_url(&release.url, release.insecure)?;
    fs::create_dir_all(dest_dir).with_context(|| format!("สร้างโฟลเดอร์ {} ไม่ได้", dest_dir.display()))?;
    let name = safe_filename(&release.filename, &release.version);
    let final_path = dest_dir.join(&name);
    let tmp = dest_dir.join(format!(".{name}.part"));
    let _ = fs::remove_file(&tmp);
    if let Err(e) = fetch_verified(release, &tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if final_path.exists() {
        fs::remove_file(&final_path).with_context(|| format!("แทนที่ไฟล์ {} ไม่ได้", final_path.display()))?;
    }
    fs::rename(&tmp, &final_path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(final_path)
}

fn fetch_verified(release: &Release, tmp: &Path) -> Result<()> {
    let res = agent(None).get(&release.url).call().map_err(|e| anyhow!("ดาวน์โหลดไม่สำเร็จ: {e}"))?;
    if res.status().as_u16() != 200 {
        bail!("ดาวน์โหลดไม่สำเร็จ: เซิร์ฟเวอร์ตอบกลับ {}", res.status().as_u16());
    }
    let mut reader = res.into_body().into_with_config().limit(release.size + 1).reader();
    let mut file = OpenOptions::new().write(true).create_new(true).open(tmp).with_context(|| format!("สร้างไฟล์ {} ไม่ได้", tmp.display()))?;
    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            // includes the body limit (size + 1) being exceeded
            Err(e) => bail!("ดาวน์โหลดไม่สำเร็จ: {e}"),
        };
        total += n as u64;
        if total > release.size {
            bail!("ไฟล์ที่ได้รับใหญ่กว่าที่เซิร์ฟเวอร์แจ้งไว้");
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
    }
    file.sync_all()?;
    if total != release.size {
        bail!("ไฟล์ไม่ครบ (ได้ {total} จาก {} ไบต์)", release.size);
    }
    let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got != release.sha256 {
        bail!("ค่า SHA-256 ของไฟล์ไม่ตรงกับที่เซิร์ฟเวอร์แจ้ง ไฟล์อาจถูกแก้ไขระหว่างทาง");
    }
    Ok(())
}

/// Semantic version (semver.org) for comparison only; build metadata is ignored.
#[derive(Debug, PartialEq, Eq)]
struct Version {
    core: [u64; 3],
    pre: Vec<String>,
}

impl Version {
    fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let s = s.split_once('+').map_or(s, |(v, _)| v);
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let nums: Vec<u64> = core
            .split('.')
            .map(|p| if p.is_empty() || (p.len() > 1 && p.starts_with('0')) { None } else { p.parse().ok() })
            .collect::<Option<_>>()?;
        let core: [u64; 3] = nums.try_into().ok()?;
        let pre = match pre {
            None => vec![],
            Some(p) => p
                .split('.')
                .map(|id| (!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')).then(|| id.to_string()))
                .collect::<Option<_>>()?,
        };
        Some(Version { core, pre })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        self.core.cmp(&other.core).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Equal,
            (true, false) => Greater,
            (false, true) => Less,
            _ => {
                for (a, b) in self.pre.iter().zip(&other.pre) {
                    let o = match (a.parse::<u64>(), b.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        (Ok(_), Err(_)) => Less,
                        (Err(_), Ok(_)) => Greater,
                        _ => a.cmp(b),
                    };
                    if o != Equal {
                        return o;
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn version_order() {
        let order = ["0.3.0", "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.10.0"];
        for w in order.windows(2) {
            assert!(Version::parse(w[0]).unwrap() < Version::parse(w[1]).unwrap(), "{} < {}", w[0], w[1]);
        }
        assert_eq!(Version::parse("v1.2.3+b.1"), Version::parse("1.2.3"));
        for bad in ["1.2", "01.2.3", "1.2.3-", "1.2.3-a..b", "x.y.z"] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn filenames() {
        assert_eq!(safe_filename("../../evil.exe", "1.0.0"), "evil.exe");
        assert_eq!(safe_filename("C:\\x\\a b.msi", "1.0.0"), "a_b.msi");
        assert_eq!(safe_filename("..", "1.0.0"), "redesk-1.0.0");
        assert_eq!(safe_filename(".hidden", "1.0.0"), "hidden");
    }

    fn reply(version: &str, url: &str) -> Reply {
        Reply { available: true, version: version.into(), notes: String::new(), size: 3, sha256: "a".repeat(64), filename: "x.exe".into(), url: url.into() }
    }

    #[test]
    fn rejects_bad_replies() {
        let s = "https://redesk.example.com";
        assert!(validate(s, false, "0.3.0", reply("0.4.0", "/api/agent/releases/1/download")).is_ok());
        assert!(validate(s, false, "0.3.0", reply("0.2.0", "/api/agent/releases/1/download")).is_err(), "downgrade");
        assert!(validate(s, false, "0.3.0", reply("0.3.0", "/api/agent/releases/1/download")).is_err(), "same version");
        assert!(validate(s, false, "0.3.0", reply("0.4.0", "https://evil.example/x")).is_err(), "foreign url");
        assert!(validate(s, false, "0.3.0", reply("0.4.0", "/api/agent/releases/../../x")).is_err(), "traversal");
        let mut r = reply("0.4.0", "/api/agent/releases/1/download");
        r.sha256 = "zz".into();
        assert!(validate(s, false, "0.3.0", r).is_err(), "sha");
    }

    /// Serves `body` once over plain HTTP on loopback.
    fn serve(body: Vec<u8>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut req = [0u8; 4096];
                let _ = s.read(&mut req);
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(&body);
            }
        });
        format!("http://{addr}")
    }

    fn release(server: &str, data: &[u8]) -> Release {
        Release {
            version: "9.9.9".into(),
            notes: String::new(),
            size: data.len() as u64,
            sha256: Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect(),
            filename: "redesk-9.9.9.exe".into(),
            url: format!("{server}/api/agent/releases/1/download"),
            insecure: false,
        }
    }

    #[test]
    fn download_verifies() {
        let dir = std::env::temp_dir().join(format!("redesk-updater-test-{}", std::process::id()));
        let data = b"vetted build".to_vec();

        let rel = release(&serve(data.clone()), &data);
        let p = download(&rel, &dir).unwrap();
        assert_eq!(fs::read(&p).unwrap(), data);
        assert_eq!(p.file_name().unwrap(), "redesk-9.9.9.exe");

        // Tampered content: same length, different bytes.
        let mut rel = release(&serve(b"evil build!!".to_vec()), &data);
        rel.filename = "tampered.exe".into();
        assert!(download(&rel, &dir).unwrap_err().to_string().contains("SHA-256"));
        assert!(!dir.join("tampered.exe").exists() && !dir.join(".tampered.exe.part").exists());

        // Longer than announced.
        let mut rel = release(&serve(b"vetted build plus extra".to_vec()), &data);
        rel.filename = "long.exe".into();
        assert!(download(&rel, &dir).is_err());
        assert!(!dir.join("long.exe").exists() && !dir.join(".long.exe.part").exists());

        // Plain http to a non-loopback host is refused before connecting.
        let mut rel = release("http://redesk.example.com", &data);
        rel.filename = "x.exe".into();
        assert!(download(&rel, &dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
