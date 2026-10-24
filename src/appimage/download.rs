//! Downloading an AppImage: `urlopen` for every scheme, the checks before
//! the swap (length, ELF header, digest), the swap that keeps a rollback copy,
//! and the lock that keeps two downloads apart.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::UA;
use super::pyisms::{py_int, py_norm, py_str_repr, py_strip, py_with_suffix};
use super::urllib;

/// urllib's `timeout=120`: it bounds every socket operation, so a body that
/// keeps arriving may take as long as it takes, but one that stalls for this
/// long fails.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// `HTTPResponse.read(262144)`: the size of each block written (and of each
/// progress step).
const BLOCK: usize = 262144;

/// What `urllib.request.urlopen` hands back: the body and its
/// `Content-Length` header (None when absent).
struct Opened {
    body: Box<dyn Read + Send>,
    content_length: Option<String>,
}

/// `Request(url)`'s clean-up of the URL, `unwrap()`: surrounding
/// whitespace, `<...>` and a `URL:` prefix go. The fragment stays part of
/// `full_url` (the data: handler reads it as data).
fn request_url(url: &str) -> String {
    let mut u = py_strip(url);
    if u.starts_with('<') && u.ends_with('>') && u.len() >= 2 {
        u = py_strip(&u[1..u.len() - 1]);
    }
    if let Some(rest) = u.strip_prefix("URL:") {
        u = py_strip(rest);
    }
    u.to_string()
}

/// `_splittag`: the URL without its fragment (split at the last '#').
fn without_fragment(url: &str) -> &str {
    match url.rfind('#') {
        Some(i) => &url[..i],
        None => url,
    }
}

/// `_splittype`: the scheme (lowercased) before the first ':', if the text
/// up to it has no '/'.
fn url_scheme(url: &str) -> Option<String> {
    let i = url.find(':')?;
    let scheme = &url[..i];
    if scheme.is_empty() || scheme.contains('/') {
        return None;
    }
    Some(scheme.to_lowercase())
}

/// `urllib.parse.unquote_to_bytes`: %XX becomes the byte, anything else
/// (including a '%' not followed by two hex digits) stays as it is.
fn unquote_to_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("00");
            out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// `base64.decodebytes` (`binascii.a2b_base64`, not strict): characters
/// outside the alphabet are skipped, a complete pad sequence ends the data.
fn a2b_base64(data: &[u8]) -> Result<Vec<u8>, String> {
    let val = |c: u8| -> Option<u8> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::new();
    let (mut quad_pos, mut leftchar, mut pads) = (0u8, 0u8, 0u8);
    for &c in data {
        if c == b'=' {
            if quad_pos >= 2 {
                pads += 1;
                if quad_pos + pads >= 4 {
                    return Ok(out);
                }
            }
            continue;
        }
        let Some(v) = val(c) else { continue };
        pads = 0;
        match quad_pos {
            0 => {
                quad_pos = 1;
                leftchar = v;
            }
            1 => {
                quad_pos = 2;
                out.push((leftchar << 2) | (v >> 4));
                leftchar = v & 0x0f;
            }
            2 => {
                quad_pos = 3;
                out.push((leftchar << 4) | (v >> 2));
                leftchar = v & 0x03;
            }
            _ => {
                quad_pos = 0;
                out.push((leftchar << 6) | v);
                leftchar = 0;
            }
        }
    }
    match quad_pos {
        0 => Ok(out),
        1 => {
            let n = data.iter().filter(|&&c| val(c).is_some()).count();
            Err(format!(
                "binascii.Error: Invalid base64-encoded string: number of data characters ({n}) cannot be 1 more than a multiple of 4"
            ))
        }
        _ => Err("binascii.Error: Incorrect padding".into()),
    }
}

/// `DataHandler.data_open`: RFC 2397 `data:[<mediatype>][;base64],<data>`.
fn open_data(url: &str) -> Result<Opened, String> {
    let rest = url.split_once(':').map(|x| x.1).unwrap_or("");
    let Some((mediatype, data)) = rest.split_once(',') else {
        return Err("ValueError: not enough values to unpack (expected 2, got 1)".into());
    };
    // Disallow control characters within mediatype.
    if mediatype.chars().any(|c| (c as u32) < 0x20 || c == '\x7f') {
        return Err("ValueError: Control characters not allowed in data: mediatype".into());
    }
    // even base64 encoded data URLs might be quoted so unquote in any case:
    let mut bytes = unquote_to_bytes(data);
    if mediatype.ends_with(";base64") {
        bytes = a2b_base64(&bytes)?;
    }
    let len = bytes.len();
    Ok(Opened { body: Box::new(std::io::Cursor::new(bytes)), content_length: Some(len.to_string()) })
}

/// `socket.gethostbyname(name)`: its first IPv4 address.
fn host_ipv4(name: &str) -> Option<std::net::Ipv4Addr> {
    use std::net::ToSocketAddrs;
    (name, 0).to_socket_addrs().ok()?.find_map(|a| match a.ip() {
        std::net::IpAddr::V4(ip) => Some(ip),
        std::net::IpAddr::V6(_) => None,
    })
}

/// `_is_local_authority(authority, resolve=True)`.
fn is_local_authority(authority: &str) -> bool {
    if authority.is_empty() || authority == "localhost" {
        return true;
    }
    let hostname = rustix::system::uname().nodename().to_string_lossy().into_owned();
    if authority == hostname {
        return true;
    }
    let Some(addr) = host_ipv4(authority) else { return false };
    // FileHandler.get_names(): every IPv4 address of localhost and of us.
    use std::net::ToSocketAddrs;
    let names: Vec<std::net::Ipv4Addr> = ["localhost", hostname.as_str()]
        .iter()
        .filter_map(|h| (*h, 0).to_socket_addrs().ok())
        .flatten()
        .filter_map(|a| match a.ip() {
            std::net::IpAddr::V4(ip) => Some(ip),
            std::net::IpAddr::V6(_) => None,
        })
        .collect();
    names.contains(&addr)
}

/// `FileHandler.open_local_file`: `url2pathname` (localhost only, query and
/// fragment dropped, %XX decoded to bytes), then the file itself, with its
/// size as the Content-Length.
fn open_file(url: &str) -> Result<Opened, String> {
    use std::os::unix::ffi::OsStrExt;
    // urlsplit: C0 controls and spaces are stripped from the front, tabs and
    // newlines removed anywhere.
    let u: String = url
        .trim_start_matches(|c: char| (c as u32) <= 0x20)
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let rest = u.split_once(':').map(|x| x.1).unwrap_or("");
    let (authority, path) = match rest.strip_prefix("//") {
        Some(r) => {
            let end = r.find(['/', '?', '#']).unwrap_or(r.len());
            (&r[..end], &r[end..])
        }
        None => ("", rest),
    };
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    if !is_local_authority(authority) {
        return Err("URLError: <urlopen error file:// scheme is supported only on localhost>".into());
    }
    let local = PathBuf::from(std::ffi::OsStr::from_bytes(&unquote_to_bytes(path)));
    let err = |e: std::io::Error| format!("URLError: <urlopen error {e}>");
    let size = std::fs::metadata(&local).map_err(err)?.len();
    // open() of a directory is IsADirectoryError in Python; on Linux the
    // open itself succeeds, so refuse it here, before anything is written.
    if std::fs::metadata(&local).map_err(err)?.is_dir() {
        return Err(err(std::io::Error::from_raw_os_error(libc::EISDIR)));
    }
    let f = std::fs::File::open(&local).map_err(err)?;
    Ok(Opened { body: Box::new(f), content_length: Some(size.to_string()) })
}

/// `urllib.request.urlopen`: http and https over the network, file and
/// data URLs locally; any other scheme is the URLError urllib raises
/// (ftp, which urllib also speaks, has no client here).
fn urlopen(url: &str) -> Result<Opened, String> {
    let clean = request_url(url);
    // `Request.full_url`: the fragment comes back only when it is not empty.
    let base = without_fragment(&clean);
    let full_url = match clean.rfind('#').map(|i| &clean[i + 1..]) {
        Some(f) if !f.is_empty() => format!("{base}#{f}"),
        _ => base.to_string(),
    };
    let Some(scheme) = url_scheme(base) else {
        return Err(format!("ValueError: unknown url type: {}", py_str_repr(&clean)));
    };
    urllib::check_request(&full_url)?;
    urllib::build_opener();
    match scheme.as_str() {
        "http" | "https" => {
            let (body, content_length) = urllib::open_http(url, UA, DOWNLOAD_TIMEOUT)?;
            Ok(Opened { body, content_length })
        }
        "file" => open_file(&clean),
        "data" => open_data(&full_url),
        other => Err(format!("URLError: <urlopen error unknown url type: {other}>")),
    }
}

/// Read `body` in `BLOCK`-sized pieces (the last may be shorter), each read
/// bounded by `DOWNLOAD_TIMEOUT` the way urllib's socket timeout bounds it.
/// The reads run on a helper thread so a stalled connection cannot hang the
/// caller; on a timeout that thread is left to die with the connection.
fn read_blocks(mut body: Box<dyn Read + Send>, mut each: impl FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Vec<u8>, String>>(16);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let msg = match body.read(&mut buf) {
                Ok(n) => Ok(buf[..n].to_vec()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => Err(format!("OSError: {e}")),
            };
            let done = !matches!(&msg, Ok(c) if !c.is_empty());
            if tx.send(msg).is_err() || done {
                return;
            }
        }
    });
    let mut block: Vec<u8> = Vec::with_capacity(BLOCK);
    loop {
        let chunk = match rx.recv_timeout(DOWNLOAD_TIMEOUT) {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => return Err(e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Err("TimeoutError: The read operation timed out".into()),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Vec::new(),
        };
        if chunk.is_empty() {
            if !block.is_empty() {
                each(&block)?;
            }
            return Ok(());
        }
        let mut rest = chunk.as_slice();
        while !rest.is_empty() {
            let take = (BLOCK - block.len()).min(rest.len());
            block.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if block.len() == BLOCK {
                each(&block)?;
                block.clear();
            }
        }
    }
}

/// Fetch to a temporary file beside dest, then swap it in.
///
/// Downloading over the live file would leave a half-written AppImage if the
/// connection dropped; the swap is the last step and is atomic.
///
/// `progress` receives the fraction done when the source gives a
/// Content-Length. Errors (network, I/O) are returned, as the Python raises.
/// See [`download_verified`] for what the download must be before it is
/// swapped in.
pub fn download(url: &str, dest: &Path, progress: Option<&dyn Fn(f64)>) -> Result<PathBuf, String> {
    download_verified(url, dest, progress, "").map(|d| d.path)
}

/// What every AppImage (type 1 and type 2 alike) starts with: it is an ELF
/// executable, the runtime, with the filesystem image appended.
const ELF_MAGIC: &[u8] = b"\x7fELF";

/// How much of the file [`elf_check`] reads: the ELF identification and
/// header up to and including `e_machine` (offset 18, two bytes).
const ELF_HEAD: usize = 20;

/// The ELF `e_machine` this program runs on, `None` on an architecture
/// with no mapping here. x86_64 and aarch64 are what AppImages ship for;
/// elsewhere the check is skipped rather than refusing every download.
fn host_machine() -> Option<u16> {
    match std::env::consts::ARCH {
        "x86_64" => Some(0x3E),
        "aarch64" => Some(0xB7),
        _ => None,
    }
}

/// Whether `head` (the file's first bytes) is an ELF executable for
/// `machine`: an HTML error page or a truncated file is not an AppImage,
/// and a build for another architecture would install and then never
/// start. `machine` is `None` when the host's is not known: only the
/// magic is checked then.
fn elf_check(head: &[u8], machine: Option<u16>) -> Result<(), String> {
    if head.len() < ELF_HEAD || &head[..ELF_MAGIC.len()] != ELF_MAGIC {
        return Err("the download is not an AppImage (it does not start with an ELF header), \
                    so it was deleted"
            .into());
    }
    let Some(want) = machine else { return Ok(()) };
    // EI_DATA (byte 5): 2 is big-endian, anything else is read as little.
    let raw = [head[18], head[19]];
    let got = if head[5] == 2 { u16::from_be_bytes(raw) } else { u16::from_le_bytes(raw) };
    if got != want {
        return Err(format!(
            "the download is built for another processor (ELF machine {got:#x}; this one is \
             {}), so it was deleted",
            std::env::consts::ARCH
        ));
    }
    Ok(())
}

/// What [`download_verified`] installed: where, and whether it was checked
/// against a published sha256. A release with no digest is only as good as
/// TLS and the ELF check, and callers say so.
#[derive(Debug, Clone, PartialEq)]
pub struct Downloaded {
    pub path: PathBuf,
    pub checksum_verified: bool,
}

/// [`download`], checked before anything is replaced: the body must be as
/// long as its Content-Length said, start with an ELF header for this
/// machine's architecture (see [`elf_check`]), and, when `sha256` is not
/// empty, hash to it (the hex digest GitHub publishes for a release asset).
/// A download that fails any of these, or fails at all, is deleted and
/// leaves `dest` and its `.previous` copy as they were.
///
/// Two downloads to the same place (the window and the scheduled update,
/// say) would write the same temporary file: the second one finds it locked
/// and fails at once, touching nothing.
pub fn download_verified(
    url: &str,
    dest: &Path,
    progress: Option<&dyn Fn(f64)>,
    sha256: &str,
) -> Result<Downloaded, String> {
    use sha2::{Digest, Sha256};
    let dest = py_norm(dest);
    let tmp = py_with_suffix(&dest, ".download")?;
    let _lock = lock_download(&tmp)?;
    let fetched = (|| -> Result<(), String> {
        let r = urlopen(url)?;
        // `int(r.headers.get("Content-Length") or 0)`
        let total: f64 = match r.content_length.as_deref() {
            None | Some("") => 0.0,
            Some(v) => py_int(v)?.to_string().parse::<f64>().unwrap_or(0.0),
        };
        let mut done: u64 = 0;
        let mut head: Vec<u8> = Vec::with_capacity(ELF_HEAD);
        let mut hash = Sha256::new();
        {
            let mut f = std::fs::File::create(&tmp).map_err(|e| format!("OSError: {e}"))?;
            read_blocks(r.body, |chunk| {
                f.write_all(chunk).map_err(|e| format!("OSError: {e}"))?;
                hash.update(chunk);
                let want = ELF_HEAD.saturating_sub(head.len());
                head.extend_from_slice(&chunk[..want.min(chunk.len())]);
                done += chunk.len() as u64;
                if let Some(cb) = progress {
                    if total != 0.0 {
                        cb(done as f64 / total);
                    }
                }
                Ok(())
            })?;
            f.sync_all().map_err(|e| format!("OSError: {e}"))?;
        }
        // http.client hands a short body over as if it were whole; a
        // truncated AppImage must not replace a working one.
        if total > 0.0 && (done as f64) < total {
            return Err(format!("IncompleteRead: the download ended after {done} of {total} bytes"));
        }
        elf_check(&head, host_machine())?;
        if !sha256.is_empty() && !hex::encode(hash.finalize()).eq_ignore_ascii_case(sha256.trim()) {
            return Err("checksum mismatch: the download is not the file the project published, \
                        so it was deleted"
                .into());
        }
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&tmp).map_err(|e| format!("OSError: {e}"))?.permissions().mode();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode | 0o111))
            .map_err(|e| format!("OSError: {e}"))
    })();
    if let Err(e) = fetched {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Keep the previous build: a bad update must be revertible. It is linked
    // aside first and the new one renamed over `dest`, so `dest` is never
    // missing, and the old `.previous` is replaced only once the new build
    // is in place.
    let prev = py_with_suffix(&dest, ".previous")?;
    let staged = {
        let mut s = prev.clone().into_os_string();
        s.push(".new");
        PathBuf::from(s)
    };
    let kept = dest.exists() && {
        let _ = std::fs::remove_file(&staged);
        std::fs::hard_link(&dest, &staged).or_else(|_| std::fs::copy(&dest, &staged).map(|_| ())).is_ok()
    };
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        if kept {
            let _ = std::fs::remove_file(&staged);
        }
        return Err(format!("OSError: {e}"));
    }
    // The new build is in place from here on: failing to keep the old one
    // costs only the rollback copy, and reporting the update as failed would
    // leave the registry naming a version that is no longer the one installed.
    if kept {
        if let Err(e) = std::fs::rename(&staged, &prev) {
            eprintln!("almanac: {}: the previous build was not kept for rollback: {e}", dest.display());
            let _ = std::fs::remove_file(&staged);
        }
    }
    Ok(Downloaded { path: dest, checksum_verified: !sha256.is_empty() })
}

/// The lock on one download's temporary file (see [`lock_download`]).
struct DownloadLock {
    path: PathBuf,
    _file: std::fs::File,
}

impl Drop for DownloadLock {
    /// The lock file goes while it is still held, so none is left beside
    /// the AppImage; [`lock_download`] checks that the file it locked is the
    /// one the name still points at, so a new download never shares a lock
    /// with one that is just finishing.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Lock `<tmp>.lock` for one download to `tmp`, or fail at once when another
/// download to it (in this process or another) holds it. The lock goes when
/// the guard is dropped, and with the process if it dies.
fn lock_download(tmp: &Path) -> Result<DownloadLock, String> {
    use rustix::fs::{flock, FlockOperation};
    use std::os::unix::fs::MetadataExt;
    let mut name = tmp.as_os_str().to_os_string();
    name.push(".lock");
    let path = PathBuf::from(name);
    let busy = || {
        format!(
            "busy: {} is already being downloaded (by Almanac or its scheduled update); \
             try again when that is done",
            tmp.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        )
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("OSError: {e}"))?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(rustix::io::Errno::WOULDBLOCK) => return Err(busy()),
        Err(e) => return Err(format!("OSError: {e}")),
    }
    // Locked; but the download that held it may have removed the file since
    // it was opened here, and another may hold a new one by the same name.
    let same = match (file.metadata(), std::fs::metadata(&path)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    };
    if !same {
        return Err(busy());
    }
    Ok(DownloadLock { path, _file: file })
}

/// An ELF header for this machine (the smallest file [`elf_check`] takes as
/// an AppImage) followed by `tail`.
#[cfg(test)]
pub(crate) fn test_elf(tail: &[u8]) -> Vec<u8> {
    let mut v = ELF_MAGIC.to_vec();
    v.extend_from_slice(&[2, 1, 1, 0]); // ELFCLASS64, little-endian, version 1
    v.resize(18, 0);
    v.extend_from_slice(&host_machine().unwrap_or(0).to_le_bytes());
    v.extend_from_slice(tail);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    // AppImageDownloadSafety: the replacement must never leave the user
    // without a working binary.

    /// A one-shot HTTP server on localhost answering `body`.
    fn serve(body: &[u8]) -> String {
        let body = body.to_vec();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&body);
            }
        });
        format!("http://{addr}/x")
    }

    /// A body `download` takes for an AppImage: an ELF header for this machine.
    static NEW: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| test_elf(b"NEW"));

    /// Every byte of `b` %-escaped, for a `data:` URL.
    fn pct(b: &[u8]) -> String {
        b.iter().map(|c| format!("%{c:02X}")).collect()
    }

    #[test]
    fn test_previous_build_is_kept_for_rollback() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"OLD").unwrap();
        download(&serve(&NEW), &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), *NEW);
        assert_eq!(
            std::fs::read(dest.with_extension("previous")).unwrap(),
            b"OLD",
            "the old build must survive so a bad update is revertible"
        );
    }

    #[test]
    fn test_failed_download_leaves_the_original_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"OLD").unwrap();
        // A port nothing listens on: the connection dies.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(download(&format!("http://127.0.0.1:{port}/x"), &dest, None).is_err());
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"OLD",
            "a dropped connection must not destroy the installed build"
        );
    }

    // Review round 2: behaviour the parity cases pinned down.

    /// A one-shot server answering a raw HTTP response.
    fn serve_raw(resp: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = s.write_all(&resp);
            }
        });
        format!("http://{addr}/x")
    }

    /// A body shorter than its Content-Length is a failed download, not a
    /// new build: the installed one and its rollback copy stay as they were.
    #[test]
    fn test_short_body_fails_and_touches_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"\x7fELFOLD").unwrap();
        std::fs::write(tmp.path().join("app.previous"), b"\x7fELFOLDER").unwrap();
        let url = serve_raw(b"HTTP/1.0 200 OK\r\nContent-Length: 100\r\n\r\n\x7fELFNEW".to_vec());
        let err = download(&url, &dest, None).unwrap_err();
        assert!(err.contains("IncompleteRead"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"\x7fELFOLD");
        assert_eq!(std::fs::read(tmp.path().join("app.previous")).unwrap(), b"\x7fELFOLDER");
        assert!(!tmp.path().join("app.download").exists(), "the partial file is left behind");
    }

    /// Only an ELF file (every AppImage is one) replaces the installed
    /// build: an HTML error page served with 200 does not.
    #[test]
    fn test_a_download_that_is_not_an_appimage_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"\x7fELFOLD").unwrap();
        let err = download(&serve(b"<html>rate limited</html>"), &dest, None).unwrap_err();
        assert!(err.contains("not an AppImage"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"\x7fELFOLD");
        assert!(!tmp.path().join("app.previous").exists());
        assert!(!tmp.path().join("app.download").exists());
    }

    /// A published sha256 is checked before the swap.
    #[test]
    fn test_checksum_is_verified_before_the_swap() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"\x7fELFOLD").unwrap();
        let wrong = "0".repeat(64);
        let err = download_verified(&serve(&NEW), &dest, None, &wrong).unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"\x7fELFOLD");
        assert!(!tmp.path().join("app.previous").exists());
        let right = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(&*NEW)).to_uppercase()
        };
        download_verified(&serve(&NEW), &dest, None, &right).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), *NEW);
        assert_eq!(std::fs::read(tmp.path().join("app.previous")).unwrap(), b"\x7fELFOLD");
    }

    /// Whether a download was checked against a checksum is reported, so
    /// the caller can say when it was not.
    #[test]
    fn test_the_result_says_whether_a_checksum_was_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        let got = download_verified(&serve(&NEW), &dest, None, "").unwrap();
        assert_eq!(got, Downloaded { path: dest.clone(), checksum_verified: false });
        let sum = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(&*NEW))
        };
        assert!(download_verified(&serve(&NEW), &dest, None, &sum).unwrap().checksum_verified);
    }

    /// An ELF file for another processor is refused like any non-AppImage,
    /// whichever byte order its header is in; one too short to say is too.
    #[test]
    fn test_elf_check_matches_the_machine() {
        let header = |data: u8, machine: [u8; 2]| {
            let mut h = b"\x7fELF".to_vec();
            h.extend_from_slice(&[2, data, 1]);
            h.resize(18, 0);
            h.extend_from_slice(&machine);
            h
        };
        const X86_64: u16 = 0x3E;
        const AARCH64: u16 = 0xB7;
        assert_eq!(elf_check(&header(1, [0x3E, 0]), Some(X86_64)), Ok(()));
        assert_eq!(elf_check(&header(1, [0xB7, 0]), Some(AARCH64)), Ok(()));
        assert_eq!(elf_check(&header(2, [0, 0xB7]), Some(AARCH64)), Ok(()), "big-endian header");
        let err = elf_check(&header(1, [0xB7, 0]), Some(X86_64)).unwrap_err();
        assert!(err.contains("another processor") && err.contains("0xb7"), "{err}");
        assert!(elf_check(&header(1, [0x03, 0]), Some(X86_64)).is_err(), "i386");
        assert!(elf_check(&header(1, [0x28, 0]), Some(AARCH64)).is_err(), "32-bit ARM");
        assert!(elf_check(&header(2, [0x3E, 0]), Some(X86_64)).is_err(), "byte order honoured");
        // An unknown host checks the magic only.
        assert_eq!(elf_check(&header(1, [0x28, 0]), None), Ok(()));
        assert!(elf_check(b"\x7fELF", Some(X86_64)).unwrap_err().contains("not an AppImage"));
        assert!(elf_check(b"\x7fELF", None).is_err());
        assert!(elf_check(&[b'<'; 20], None).unwrap_err().contains("not an AppImage"));
        assert_eq!(host_machine(), match std::env::consts::ARCH {
            "x86_64" => Some(X86_64),
            "aarch64" => Some(AARCH64),
            _ => None,
        });
    }

    /// A build for another processor never replaces the installed one.
    #[test]
    fn test_a_wrong_architecture_download_is_refused() {
        let Some(machine) = host_machine() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"OLD").unwrap();
        let mut other = test_elf(b"NEW");
        other[18..20].copy_from_slice(&(if machine == 0x3E { 0xB7u16 } else { 0x3E }).to_le_bytes());
        let err = download(&serve(&other), &dest, None).unwrap_err();
        assert!(err.contains("another processor"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"OLD");
        assert!(!tmp.path().join("app.previous").exists());
        assert!(!tmp.path().join("app.download").exists());
    }

    /// Once the new build is in place, failing to keep the old one as
    /// `.previous` does not fail the update (the caller would not record
    /// the version now installed), and leaves no staged copy behind.
    #[test]
    fn test_an_update_succeeds_when_the_rollback_copy_cannot_be_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"OLD").unwrap();
        // A non-empty directory where `.previous` goes: the rename onto it fails.
        let prev = tmp.path().join("app.previous");
        std::fs::create_dir(&prev).unwrap();
        std::fs::write(prev.join("x"), b"").unwrap();
        let got = download(&serve(&NEW), &dest, None).unwrap();
        assert_eq!(got, dest);
        assert_eq!(std::fs::read(&dest).unwrap(), *NEW);
        assert!(prev.is_dir());
        assert!(!tmp.path().join("app.previous.new").exists(), "the staged copy is left behind");
        assert!(!tmp.path().join("app.download").exists());
    }

    /// The window and the scheduled update cannot download the same app at
    /// once: the second finds the download locked and fails without
    /// touching anything.
    #[test]
    fn test_a_download_in_progress_is_not_joined() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"\x7fELFOLD").unwrap();
        let held = lock_download(&tmp.path().join("app.download")).unwrap();
        let err = download(&serve(&NEW), &dest, None).unwrap_err();
        assert!(err.starts_with("busy:"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"\x7fELFOLD");
        drop(held);
        download(&serve(&NEW), &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), *NEW);
    }

    /// An https download is not followed down to http.
    #[test]
    fn test_no_redirect_from_https_to_http() {
        assert_eq!(urllib::redirect_allowed("https", "http://example.invalid/b"), Ok(false));
        assert_eq!(urllib::redirect_allowed("https", "HTTP://example.invalid/b"), Ok(false));
        assert_eq!(urllib::redirect_allowed("https", "https://cdn.example.invalid/b"), Ok(true));
        assert_eq!(urllib::redirect_allowed("http", "http://example.invalid/b"), Ok(true));
    }

    #[test]
    fn test_encoded_body_is_written_as_sent() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        // An ELF header, gzipped, sent although identity was asked for: it
        // is written as it came (and so is not an AppImage).
        let mut body = vec![0x7f, b'E', b'L', b'F', 0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 0xab, 0, 0, 0x83];
        body.extend_from_slice(&host_machine().unwrap_or(0).to_le_bytes());
        let mut resp = format!("HTTP/1.0 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        resp.extend_from_slice(&body);
        download(&serve_raw(resp), &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
    }

    #[test]
    fn test_file_and_data_urls_download() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("s rc");
        std::fs::write(&src, test_elf(b"FILE")).unwrap();
        let dest = tmp.path().join("app.AppImage");
        download(&format!("file://{}", tmp.path().join("s%20rc").display()), &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), test_elf(b"FILE"));
        download(&format!("data:,{}NEW%41#frag", pct(&test_elf(b""))), &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), test_elf(b"NEWA#frag"));
        download(&format!("data:text/plain;base64,{}", urllib::b64encode(&NEW)), &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), *NEW);
        assert!(download("data:,NEW", &dest, None).is_err(), "not an ELF file");
        assert!(download("data:no-comma", &dest, None).is_err());
        assert!(download("gopher://x/y", &dest, None).is_err());
    }

    #[test]
    fn test_non_2xx_and_lenient_content_length() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"OLD").unwrap();
        let url = serve_raw(b"HTTP/1.0 304 Not Modified\r\nContent-Length: 3\r\n\r\nNEW".to_vec());
        assert!(download(&url, &dest, None).is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"OLD");
        // The body is exactly a 20-byte ELF header.
        for len in ["+20", "2_0", "-5"] {
            let mut resp = format!("HTTP/1.0 200 OK\r\nContent-Length: {len}\r\n\r\n").into_bytes();
            resp.extend_from_slice(&test_elf(b""));
            download(&serve_raw(resp), &dest, None).unwrap();
            assert_eq!(std::fs::read(&dest).unwrap(), test_elf(b""));
        }
        download(&format!("data:,{}abc#", pct(&test_elf(b""))), &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), test_elf(b"abc"));
    }
}
