//! `urllib.request.urlopen` for http and https: the request it sends (GET,
//! HTTP/1.1, `Accept-Encoding: identity`, `Connection: close`, no
//! decompression), `http.client`'s reading of the response (lenient
//! Content-Length, chunked bodies), a timeout on every socket operation
//! rather than on the whole transfer, HTTPErrorProcessor (anything outside
//! 2xx is an error unless the redirect handler follows it), the redirect
//! rules, proxies from `<scheme>_proxy` as urllib picks them, and OpenSSL's
//! trust store (`SSL_CERT_FILE` / `SSL_CERT_DIR` included). Errors carry
//! Python's exception name; only that there is one matters.
//!
//! Stricter than urllib where a download's integrity depends on it: a body
//! shorter than its Content-Length, or a TLS connection closed without
//! close_notify, is an error rather than the end of the body, and a redirect
//! from https to http is not followed.
//!
//! Same code as `icons`' copy (private there), but for those three.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Python's `str.isspace()` for one character.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// `int(s, base)`: surrounding whitespace, a sign, `_` between digits, a
/// `0x` prefix for base 16. `ascii_ws` for bytes, where only ASCII
/// whitespace is stripped. Saturates instead of growing without bound.
fn py_int(s: &str, base: u32, ascii_ws: bool) -> Option<i128> {
    let t = if ascii_ws {
        s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'))
    } else {
        py_strip(s)
    };
    let (neg, mut t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut prefixed = false;
    if base == 16 && (t.starts_with("0x") || t.starts_with("0X")) {
        t = &t[2..];
        prefixed = true;
    }
    let (mut val, mut digits, mut last_us) = (0i128, 0usize, false);
    for (n, c) in t.chars().enumerate() {
        if c == '_' {
            if last_us || (n == 0 && !prefixed) {
                return None;
            }
            last_us = true;
            continue;
        }
        let d = c.to_digit(base)?;
        val = val.saturating_mul(i128::from(base)).saturating_add(i128::from(d)).min(1 << 100);
        digits += 1;
        last_us = false;
    }
    if digits == 0 || last_us || (base == 10 && digits > super::pyisms::INT_MAX_STR_DIGITS) {
        return None;
    }
    Some(if neg { -val } else { val })
}

/// `str.split(None, maxsplit)`.
fn py_split(s: &str, maxsplit: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s.trim_start_matches(py_isspace);
    while !rest.is_empty() {
        if out.len() == maxsplit {
            out.push(rest);
            break;
        }
        let end = rest.find(py_isspace).unwrap_or(rest.len());
        out.push(&rest[..end]);
        rest = rest[end..].trim_start_matches(py_isspace);
    }
    out
}

/// `str.capitalize()`.
fn py_capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

/// A Python dict of str, in insertion order.
fn dict_set<V>(d: &mut Vec<(String, V)>, k: &str, v: V) {
    match d.iter_mut().find(|(x, _)| x == k) {
        Some(e) => e.1 = v,
        None => d.push((k.to_string(), v)),
    }
}

fn dict_get<'a, V>(d: &'a [(String, V)], k: &str) -> Option<&'a V> {
    d.iter().find(|(x, _)| x == k).map(|(_, v)| v)
}

fn dict_pop<V>(d: &mut Vec<(String, V)>, k: &str) -> Option<V> {
    let i = d.iter().position(|(x, _)| x == k)?;
    Some(d.remove(i).1)
}

/// `%XX` decoded, other bytes as they are.
fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && s.get(i + 1).is_some_and(u8::is_ascii_hexdigit) && s.get(i + 2).is_some_and(u8::is_ascii_hexdigit) {
            let h = std::str::from_utf8(&s[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(h) = h {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `urllib.parse.unquote` (UTF-8, errors="replace").
fn unquote(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    String::from_utf8_lossy(&percent_decode(s.as_bytes())).into_owned()
}

/// `unwrap('<URL:type://host/path>')`.
fn unwrap_url(url: &str) -> String {
    let mut u = py_strip(url);
    if u.starts_with('<') && u.ends_with('>') && !u.is_empty() {
        u = py_strip(&u[1..u.len() - 1]);
        if u.is_empty() {
            return String::new();
        }
    }
    if let Some(r) = u.strip_prefix("URL:") {
        u = py_strip(r);
    }
    u.to_string()
}

/// `_splittype`: `([^/:]+):(.*)`.
fn _splittype(url: &str) -> (Option<String>, &str) {
    for (i, c) in url.char_indices() {
        if c == '/' {
            break;
        }
        if c == ':' {
            if i == 0 {
                break;
            }
            return (Some(url[..i].to_lowercase()), &url[i + 1..]);
        }
    }
    (None, url)
}

/// `_splithost`: `//([^/#?]*)(.*)`.
fn _splithost(url: &str) -> (Option<String>, String) {
    let Some(r) = url.strip_prefix("//") else {
        return (None, url.to_string());
    };
    let end = r.find(['/', '#', '?']).unwrap_or(r.len());
    let path = &r[end..];
    let path = if !path.is_empty() && !path.starts_with('/') { format!("/{path}") } else { path.to_string() };
    (Some(r[..end].to_string()), path)
}

/// `_splittag`: the last `#`.
fn _splittag(url: &str) -> (String, Option<String>) {
    match url.rfind('#') {
        Some(i) => (url[..i].to_string(), Some(url[i + 1..].to_string())),
        None => (url.to_string(), None),
    }
}

/// `_splitport`: `(.*):([0-9]*)`, the port only when non-empty.
fn _splitport(host: &str) -> (&str, Option<&str>) {
    if let Some(i) = host.rfind(':') {
        let port = &host[i + 1..];
        if port.bytes().all(|b| b.is_ascii_digit()) {
            return (&host[..i], (!port.is_empty()).then_some(port));
        }
    }
    (host, None)
}

#[derive(Debug, Clone, Default)]
struct Split {
    scheme: Option<String>,
    netloc: Option<String>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

const USES_RELATIVE: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "imap", "wais", "file", "https", "shttp", "mms", "prospero", "rtsp",
    "rtsps", "rtspu", "sftp", "svn", "svn+ssh", "ws", "wss",
];
const USES_NETLOC: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "telnet", "imap", "wais", "file", "mms", "https", "shttp", "snews",
    "prospero", "rtsp", "rtsps", "rtspu", "rsync", "svn", "svn+ssh", "sftp", "nfs", "git", "git+ssh", "ws", "wss",
    "itms-services",
];
const USES_PARAMS: &[&str] = &[
    "", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu", "sip", "sips", "mms",
    "sftp", "tel",
];

/// Characters whose NFKC form holds one of `/?#@:` (`_checknetloc`).
const NFKC_DELIMS: &[char] = &[
    '\u{2047}', '\u{2048}', '\u{2049}', '\u{2100}', '\u{2101}', '\u{2105}', '\u{2106}', '\u{2a74}', '\u{fe13}',
    '\u{fe16}', '\u{fe55}', '\u{fe56}', '\u{fe5f}', '\u{fe6b}', '\u{ff03}', '\u{ff0f}', '\u{ff1a}', '\u{ff1f}',
    '\u{ff20}',
];

fn _check_bracketed_host(hostname: &str) -> Result<(), String> {
    let bad = || Err("ValueError: Invalid IPv6 URL".to_string());
    if hostname.starts_with(['v', 'V']) {
        // \A[vV][a-fA-F0-9]+\..+\z
        let r = &hostname[1..];
        let hex = r.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(r.len());
        if hex == 0 || !r[hex..].starts_with('.') || r[hex + 1..].is_empty() || r[hex + 1..].contains('\n') {
            return bad();
        }
        return Ok(());
    }
    if hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        return bad();
    }
    let (addr, scope) = match hostname.split_once('%') {
        Some((a, s)) => (a, Some(s)),
        None => (hostname, None),
    };
    if scope.is_some_and(|s| s.is_empty() || s.contains('%')) || addr.parse::<std::net::Ipv6Addr>().is_err() {
        return bad();
    }
    Ok(())
}

/// `urllib.parse._urlsplit`.
fn _urlsplit(url: &str, allow_fragments: bool) -> Result<Split, String> {
    let url: String = url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut sp = Split::default();
    let mut rest = url.as_str();
    if let Some(i) = rest.find(':') {
        let first = rest.chars().next().unwrap_or(' ');
        if i > 0
            && first.is_ascii_alphabetic()
            && rest[..i].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            sp.scheme = Some(rest[..i].to_ascii_lowercase());
            rest = &rest[i + 1..];
        }
    }
    if let Some(r) = rest.strip_prefix("//") {
        let end = r.find(['/', '?', '#']).unwrap_or(r.len());
        let netloc = &r[..end];
        rest = &r[end..];
        if netloc.contains('[') != netloc.contains(']') {
            return Err("ValueError: Invalid IPv6 URL".into());
        }
        if netloc.contains('[') {
            let hp = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
            let hostname = match hp.split_once('[') {
                Some((before, bracketed)) => {
                    if !before.is_empty() {
                        return Err("ValueError: Invalid IPv6 URL".into());
                    }
                    let (h, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                    if !port.is_empty() && !port.starts_with(':') {
                        return Err("ValueError: Invalid IPv6 URL".into());
                    }
                    h
                }
                None => hp.split_once(':').map_or(hp, |(h, _)| h),
            };
            _check_bracketed_host(hostname)?;
        }
        sp.netloc = Some(netloc.to_string());
    }
    let mut path = rest.to_string();
    if allow_fragments {
        if let Some((p, f)) = path.clone().split_once('#') {
            path = p.to_string();
            sp.fragment = Some(f.to_string());
        }
    }
    if let Some((p, q)) = path.clone().split_once('?') {
        path = p.to_string();
        sp.query = Some(q.to_string());
    }
    sp.path = path;
    if let Some(n) = &sp.netloc {
        if !n.is_ascii() && n.chars().filter(|c| !matches!(c, '@' | ':' | '#' | '?')).any(|c| NFKC_DELIMS.contains(&c)) {
            return Err("ValueError: netloc contains invalid characters under NFKC normalization".into());
        }
    }
    Ok(sp)
}

/// `urllib.parse._urlunsplit`.
fn _urlunsplit(scheme: Option<&str>, netloc: Option<&str>, url: &str, query: Option<&str>, fragment: Option<&str>) -> String {
    let mut url = url.to_string();
    if let Some(n) = netloc {
        if !url.is_empty() && !url.starts_with('/') {
            url = format!("/{url}");
        }
        url = format!("//{n}{url}");
    } else if url.starts_with("//") {
        url = format!("//{url}");
    }
    if let Some(s) = scheme.filter(|s| !s.is_empty()) {
        url = format!("{s}:{url}");
    }
    if let Some(q) = query {
        url = format!("{url}?{q}");
    }
    if let Some(f) = fragment {
        url = format!("{url}#{f}");
    }
    url
}

/// `urlparse(url)`: the six parts, `None` turned into "".
fn urlparse(url: &str) -> Result<[String; 6], String> {
    let sp = _urlsplit(url, true)?;
    let scheme = sp.scheme.unwrap_or_default();
    let mut path = sp.path;
    let mut params = String::new();
    if USES_PARAMS.contains(&scheme.as_str()) && path.contains(';') {
        let i = match path.rfind('/') {
            Some(slash) => path[slash..].find(';').map(|j| slash + j),
            None => path.find(';'),
        };
        if let Some(i) = i {
            params = path[i + 1..].to_string();
            path.truncate(i);
        }
    }
    Ok([scheme, sp.netloc.unwrap_or_default(), path, params, sp.query.unwrap_or_default(), sp.fragment.unwrap_or_default()])
}

/// `urlunparse(parts)`.
fn urlunparse(p: &[String; 6]) -> String {
    let [scheme, netloc, url, params, query, fragment] = p;
    let netloc = if netloc.is_empty() {
        (!scheme.is_empty() && USES_NETLOC.contains(&scheme.as_str()) && (url.is_empty() || url.starts_with('/'))).then_some("")
    } else {
        Some(netloc.as_str())
    };
    let url = if params.is_empty() { url.clone() } else { format!("{url};{params}") };
    let opt = |s: &String| (!s.is_empty()).then(|| s.clone());
    _urlunsplit(opt(scheme).as_deref(), netloc, &url, opt(query).as_deref(), opt(fragment).as_deref())
}

/// `urllib.parse.urljoin(base, url)`.
fn urljoin(base: &str, url: &str) -> Result<String, String> {
    if base.is_empty() {
        return Ok(url.to_string());
    }
    if url.is_empty() {
        return Ok(base.to_string());
    }
    let b = _urlsplit(base, true)?;
    let u = _urlsplit(url, true)?;
    let scheme = u.scheme.clone().or(b.scheme.clone());
    let s = scheme.clone().unwrap_or_default();
    if scheme != b.scheme || (!s.is_empty() && !USES_RELATIVE.contains(&s.as_str())) {
        return Ok(url.to_string());
    }
    let mut netloc = u.netloc.clone();
    if s.is_empty() || USES_NETLOC.contains(&s.as_str()) {
        if netloc.as_deref().is_some_and(|n| !n.is_empty()) {
            return Ok(_urlunsplit(scheme.as_deref(), netloc.as_deref(), &u.path, u.query.as_deref(), u.fragment.as_deref()));
        }
        netloc = b.netloc.clone();
    }
    let (mut query, mut fragment) = (u.query.clone(), u.fragment.clone());
    if u.path.is_empty() {
        if query.is_none() {
            query = b.query.clone();
            if fragment.is_none() {
                fragment = b.fragment.clone();
            }
        }
        return Ok(_urlunsplit(scheme.as_deref(), netloc.as_deref(), &b.path, query.as_deref(), fragment.as_deref()));
    }
    let mut base_parts: Vec<&str> = b.path.split('/').collect();
    if base_parts.last() != Some(&"") {
        // the last item is not a directory, so will not be taken into account
        // in resolving the relative path
        base_parts.pop();
    }
    let segments: Vec<&str> = if u.path.starts_with('/') {
        u.path.split('/').collect()
    } else {
        let mut s: Vec<&str> = base_parts;
        s.extend(u.path.split('/'));
        // filter out elements that would cause redundant slashes on re-joining
        // the resolved_path
        if s.len() > 2 {
            let last = s.len() - 1;
            let mid: Vec<&str> = s[1..last].iter().copied().filter(|x| !x.is_empty()).collect();
            let mut t = vec![s[0]];
            t.extend(mid);
            t.push(s[last]);
            s = t;
        }
        s
    };
    let mut resolved: Vec<&str> = Vec::new();
    for seg in &segments {
        match *seg {
            ".." => {
                resolved.pop();
            }
            "." => {}
            other => resolved.push(other),
        }
    }
    if matches!(segments.last(), Some(&".") | Some(&"..")) {
        resolved.push("");
    }
    let joined = resolved.join("/");
    let path = if joined.is_empty() { "/".to_string() } else { joined };
    Ok(_urlunsplit(scheme.as_deref(), netloc.as_deref(), &path, query.as_deref(), fragment.as_deref()))
}

/// `quote(url, encoding="iso-8859-1", safe=string.punctuation)`.
fn quote_redirect(url: &str) -> Result<String, String> {
    let mut out = String::new();
    for c in url.chars() {
        let b = u32::from(c);
        if b > 0xff {
            return Err("UnicodeEncodeError: 'latin-1' codec can't encode character".into());
        }
        if (0x21..0x7f).contains(&b) {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(out)
}

/// The `idna` codec (IDNA 2003). Non-ASCII labels go through UTS 46, which
/// maps them the same way for ordinary names.
fn idna_encode(host: &str) -> Result<String, String> {
    let err = |r: &str| Err(format!("UnicodeEncodeError: 'idna' codec can't encode: {r}"));
    if host.is_empty() {
        return Ok(String::new());
    }
    if host.is_ascii() {
        let labels: Vec<&str> = host.split('.').collect();
        if labels[..labels.len() - 1].iter().any(|l| l.is_empty()) {
            return err("label empty");
        }
        if labels.iter().any(|l| l.len() >= 64) {
            return err("label too long");
        }
        return Ok(host.to_string());
    }
    let mut labels: Vec<&str> = host.split(['.', '\u{3002}', '\u{ff0e}', '\u{ff61}']).collect();
    let trailing = labels.last() == Some(&"");
    if trailing {
        labels.pop();
    }
    let mut out = Vec::new();
    for label in labels {
        let ascii = if label.is_ascii() { label.to_string() } else { url::quirks::domain_to_ascii(label) };
        if ascii.is_empty() {
            return err("label empty");
        }
        if ascii.len() >= 64 {
            return err("label too long");
        }
        out.push(ascii);
    }
    let mut s = out.join(".");
    if trailing {
        s.push('.');
    }
    Ok(s)
}

pub(super) fn b64encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |a, (i, &b)| a | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// -- proxies

/// `getproxies_environment()`: `<scheme>_proxy` variables, the all-lowercase
/// spelling winning; `HTTP_PROXY` ignored under CGI. Note there is no
/// fallback: `all_proxy` only ever serves an `all:` URL.
fn getproxies_environment() -> Vec<(String, String)> {
    let env: Vec<(String, String)> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .collect();
    let mut proxies: Vec<(String, String)> = Vec::new();
    let mut environment = Vec::new();
    for (name, value) in &env {
        let chars: Vec<char> = name.chars().collect();
        let n = chars.len();
        if n > 5 && chars[n - 6] == '_' && chars[n - 5..].iter().collect::<String>().to_lowercase() == "proxy" {
            let proxy_name = chars[..n - 6].iter().collect::<String>().to_lowercase();
            if !value.is_empty() {
                dict_set(&mut proxies, &proxy_name, value.clone());
            }
            environment.push((name, value, proxy_name));
        }
    }
    if env.iter().any(|(k, _)| k == "REQUEST_METHOD") {
        dict_pop(&mut proxies, "http");
    }
    for (name, value, proxy_name) in environment {
        if name.ends_with("_proxy") {
            if !value.is_empty() {
                dict_set(&mut proxies, &proxy_name, value.clone());
            } else {
                dict_pop(&mut proxies, &proxy_name);
            }
        }
    }
    proxies
}

/// The ProxyHandler's table, fixed when the first URL is opened (urllib
/// builds its opener once).
fn proxies() -> &'static [(String, String)] {
    static P: OnceLock<Vec<(String, String)>> = OnceLock::new();
    P.get_or_init(getproxies_environment)
}

/// `proxy_bypass_environment(host)`: `no_proxy`, read afresh each time.
fn proxy_bypass(host: &str) -> bool {
    let proxies = getproxies_environment();
    let Some(no_proxy) = dict_get(&proxies, "no") else {
        return false;
    };
    if no_proxy == "*" {
        return true;
    }
    let host = host.to_lowercase();
    let (hostonly, _) = _splitport(&host);
    for name in no_proxy.split(',') {
        let name = py_strip(name);
        if !name.is_empty() {
            let name = name.trim_start_matches('.').to_lowercase();
            if hostonly == name || host == name {
                return true;
            }
            let name = format!(".{name}");
            if hostonly.ends_with(&name) || host.ends_with(&name) {
                return true;
            }
        }
    }
    false
}

type ProxyParts = (Option<String>, Option<String>, Option<String>, String);

/// `_parse_proxy(proxy)`: scheme, user, password, host:port.
fn _parse_proxy(proxy: &str) -> Result<ProxyParts, String> {
    let (scheme, r_scheme) = _splittype(proxy);
    let (scheme, authority) = if !r_scheme.starts_with('/') {
        (None, proxy.to_string())
    } else {
        if !r_scheme.starts_with("//") {
            return Err(format!("ValueError: proxy URL with no authority: {proxy:?}"));
        }
        let end = match r_scheme.find('@') {
            Some(at) => r_scheme[at..].find('/').map(|j| at + j),
            None => r_scheme[2..].find('/').map(|j| 2 + j),
        };
        (scheme, r_scheme[2..end.unwrap_or(r_scheme.len()).max(2)].to_string())
    };
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(authority[..i].to_string()), authority[i + 1..].to_string()),
        None => (None, authority.clone()),
    };
    let (user, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((a, b)) => (Some(a.to_string()), Some(b.to_string())),
            None => (Some(u), None),
        },
        None => (None, None),
    };
    Ok((scheme, user, password, hostport))
}

// -- the request

/// `urllib.request.Request`, the parts used here.
#[derive(Clone)]
struct Req {
    full: String,
    fragment: Option<String>,
    typ: String,
    host: Option<String>,
    selector: String,
    tunnel_host: Option<String>,
    headers: Vec<(String, String)>,
    /// `None` is a Host header Python would fail to send.
    unredirected: Vec<(String, Option<String>)>,
    redirect_dict: Option<Rc<RefCell<HashMap<String, u32>>>>,
    timeout: Duration,
}

impl Req {
    /// `Request(url, headers=...)`; `origin` false where the caller passes
    /// `origin_req_host` (so the URL is not run through `urlparse`).
    fn new(url: &str, headers: &[(String, String)], origin: bool, timeout: Duration) -> Result<Req, String> {
        let (full, fragment) = _splittag(&unwrap_url(url));
        let (typ, rest) = _splittype(&full);
        let Some(typ) = typ else {
            return Err(format!("ValueError: unknown url type: {url:?}"));
        };
        let (host, selector) = _splithost(rest);
        let host = host.map(|h| if h.is_empty() { h } else { unquote(&h) });
        let mut req = Req {
            full: full.clone(),
            fragment,
            typ,
            host,
            selector,
            tunnel_host: None,
            headers: Vec::new(),
            unredirected: Vec::new(),
            redirect_dict: None,
            timeout,
        };
        for (k, v) in headers {
            dict_set(&mut req.headers, &py_capitalize(k), v.clone());
        }
        if origin {
            // request_host(): urlparse may raise.
            urlparse(&req.full_url())?;
        }
        Ok(req)
    }

    fn full_url(&self) -> String {
        match self.fragment.as_deref() {
            Some(f) if !f.is_empty() => format!("{}#{f}", self.full),
            _ => self.full.clone(),
        }
    }

    fn set_proxy(&mut self, host: String, typ: String) {
        if self.typ == "https" && self.tunnel_host.as_deref().is_none_or(str::is_empty) {
            self.tunnel_host = self.host.clone();
        } else {
            self.typ = typ;
            self.selector = self.full_url();
        }
        self.host = Some(host);
    }

    fn has_proxy(&self) -> bool {
        self.selector == self.full_url()
    }

    fn has_header(&self, name: &str) -> bool {
        dict_get(&self.headers, name).is_some() || dict_get(&self.unredirected, name).is_some()
    }
}

// -- the connection

const TLS_SUFFIX_EOF: std::io::ErrorKind = std::io::ErrorKind::UnexpectedEof;

enum Sock {
    Tcp(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Sock {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Sock::Tcp(s) => s.read(buf),
            // Python's suppress_ragged_eofs reads a close without
            // close_notify as the end of the body. It is how a truncation
            // looks, so it is an error here: a body with a length or chunks
            // never reads that far, and one without has no other end to go by.
            Sock::Tls(s) => match s.read(buf) {
                Err(e) if e.kind() == TLS_SUFFIX_EOF => Err(std::io::Error::new(
                    TLS_SUFFIX_EOF,
                    "the TLS connection closed without close_notify: the body may be truncated",
                )),
                r => r,
            },
        }
    }
}

impl Write for Sock {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Sock::Tcp(s) => s.write(buf),
            Sock::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Sock::Tcp(s) => s.flush(),
            Sock::Tls(s) => s.flush(),
        }
    }
}

/// `sock.makefile("rb")`.
struct Conn {
    sock: Sock,
    buf: Vec<u8>,
    pos: usize,
}

fn io_err(e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => "TimeoutError: timed out".into(),
        _ => format!("OSError: {e}"),
    }
}

impl Conn {
    fn new(sock: Sock) -> Conn {
        Conn { sock, buf: Vec::new(), pos: 0 }
    }

    /// One more read from the socket; false at EOF.
    fn fill(&mut self) -> Result<bool, String> {
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let mut tmp = [0u8; 8192];
        loop {
            match self.sock.read(&mut tmp) {
                Ok(n) => {
                    self.buf.extend_from_slice(&tmp[..n]);
                    return Ok(n > 0);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io_err(e)),
            }
        }
    }

    fn take(&mut self, n: usize) -> Vec<u8> {
        let n = n.min(self.buf.len() - self.pos);
        let out = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        out
    }

    /// `readline(limit)`.
    fn readline(&mut self, limit: usize) -> Result<Vec<u8>, String> {
        loop {
            let avail = &self.buf[self.pos..];
            if let Some(nl) = avail.iter().position(|&b| b == b'\n') {
                return Ok(self.take((nl + 1).min(limit)));
            }
            if avail.len() >= limit || !self.fill()? {
                return Ok(self.take(limit));
            }
        }
    }

    /// `read(n)`: up to n bytes, fewer only at EOF.
    fn read(&mut self, n: usize) -> Result<Vec<u8>, String> {
        while self.buf.len() - self.pos < n {
            if !self.fill()? {
                break;
            }
        }
        Ok(self.take(n))
    }

    /// `read()`: to EOF.
    fn read_to_end(&mut self) -> Result<Vec<u8>, String> {
        while self.fill()? {}
        Ok(self.take(usize::MAX))
    }

    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.sock.write_all(data).and_then(|_| self.sock.flush()).map_err(io_err)
    }
}

/// `socket.create_connection((host, port), timeout)`: every address in
/// turn, each with the full timeout, which then holds for every read and
/// write on the socket.
fn create_connection(host: &str, port: i128, timeout: Duration) -> Result<TcpStream, String> {
    if !(0..=65535).contains(&port) {
        return Err("OverflowError: getsockaddrarg: port must be 0-65535.".into());
    }
    let h = if host.is_ascii() { host.to_string() } else { idna_encode(host)? };
    let addrs: Vec<_> = (h.as_str(), port as u16).to_socket_addrs().map_err(|e| format!("socket.gaierror: {e}"))?.collect();
    let mut last = "OSError: getaddrinfo returns an empty list".to_string();
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                s.set_read_timeout(Some(timeout)).map_err(io_err)?;
                s.set_write_timeout(Some(timeout)).map_err(io_err)?;
                return Ok(s);
            }
            Err(e) => last = io_err(e),
        }
    }
    Err(last)
}

/// The default HTTPS context: the system's CA store (as OpenSSL finds it,
/// `SSL_CERT_FILE` / `SSL_CERT_DIR` included), TLS 1.2 and up, ALPN
/// http/1.1. Built once, like urllib's.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CFG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CFG.get_or_init(|| {
        let anchors = system_certs();
        let mut roots = rustls::RootCertStore::empty();
        roots.add_parsable_certificates(anchors.clone());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .map_err(|e| format!("ssl.SSLError: {e}"))?;
        let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("ssl.SSLError: {e}"))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TrustStore { inner, anchors }))
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(cfg))
    })
    .clone()
}

/// OpenSSL's verification as rustls' WebPKI does it, plus the one case
/// WebPKI refuses and OpenSSL accepts: a self-signed certificate that is
/// itself in the trust store (a locally trusted certificate, often marked
/// CA:TRUE), still checked for its validity period and the host name.
#[derive(Debug)]
struct TrustStore {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    anchors: Vec<rustls::pki_types::CertificateDer<'static>>,
}

impl rustls::client::danger::ServerCertVerifier for TrustStore {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let err = match self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now) {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        if !self.anchors.iter().any(|a| a.as_ref() == end_entity.as_ref()) {
            return Err(err);
        }
        let Some((issuer, subject, not_before, not_after)) = cert_names_and_validity(end_entity.as_ref()) else {
            return Err(err);
        };
        let t = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
        if issuer != subject || t < not_before || t > not_after {
            return Err(err);
        }
        let parsed = rustls::server::ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_name(&parsed, server_name)?;
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// One DER element: tag, the whole element, its contents, what follows.
fn der_next(b: &[u8]) -> Option<(u8, &[u8], &[u8], &[u8])> {
    let tag = *b.first()?;
    let first = *b.get(1)? as usize;
    let (len, head) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for k in 0..n {
            len = (len << 8) | *b.get(2 + k)? as usize;
        }
        (len, 2 + n)
    };
    let end = head.checked_add(len)?;
    if end > b.len() {
        return None;
    }
    Some((tag, &b[..end], &b[head..end], &b[end..]))
}

/// An X.509 Time (UTCTime or GeneralizedTime) as Unix seconds.
fn der_time(tag: u8, v: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(v).ok()?.strip_suffix('Z')?;
    let full = match tag {
        0x17 if s.len() == 12 => {
            let yy: i32 = s[..2].parse().ok()?;
            format!("{}{}", if yy < 50 { 2000 + yy } else { 1900 + yy }, &s[2..])
        }
        0x18 if s.len() == 14 => s.to_string(),
        _ => return None,
    };
    let dt = chrono::NaiveDateTime::parse_from_str(&full, "%Y%m%d%H%M%S").ok()?;
    Some(dt.and_utc().timestamp())
}

/// A certificate's issuer and subject (as DER) and its validity period.
fn cert_names_and_validity(der: &[u8]) -> Option<(&[u8], &[u8], i64, i64)> {
    let (_, _, cert, _) = der_next(der)?;
    let (_, _, tbs, _) = der_next(cert)?;
    let mut r = tbs;
    if r.first() == Some(&0xa0) {
        r = der_next(r)?.3; // version
    }
    r = der_next(r)?.3; // serialNumber
    r = der_next(r)?.3; // signature
    let (_, issuer, _, rest) = der_next(r)?;
    let (_, _, validity, rest) = der_next(rest)?;
    let (_, subject, _, _) = der_next(rest)?;
    let (t1, _, nb, more) = der_next(validity)?;
    let (t2, _, na, _) = der_next(more)?;
    Some((issuer, subject, der_time(t1, nb)?, der_time(t2, na)?))
}

fn system_certs() -> Vec<rustls::pki_types::CertificateDer<'static>> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::CertificateDer;
    let mut out = Vec::new();
    let file = std::env::var_os("SSL_CERT_FILE").unwrap_or_else(|| "/etc/ssl/cert.pem".into());
    if let Ok(it) = CertificateDer::pem_file_iter(&file) {
        out.extend(it.flatten());
    }
    // A capath is looked up by subject hash: <8 hex digits>.<n>.
    let dirs = std::env::var_os("SSL_CERT_DIR").unwrap_or_else(|| "/etc/ssl/certs".into());
    for dir in dirs.as_bytes().split(|&b| b == b':') {
        let Ok(rd) = std::fs::read_dir(Path::new(std::ffi::OsStr::from_bytes(dir))) else { continue };
        for e in rd.flatten() {
            let name = e.file_name();
            let n = name.as_bytes();
            let hashed = n.len() > 9
                && n[..8].iter().all(u8::is_ascii_hexdigit)
                && n[8] == b'.'
                && n[9..].iter().all(u8::is_ascii_digit);
            if hashed {
                if let Ok(it) = CertificateDer::pem_file_iter(e.path()) {
                    out.extend(it.flatten());
                }
            }
        }
    }
    out
}

fn wrap_tls(tcp: TcpStream, server_hostname: &str) -> Result<Sock, String> {
    let cfg = tls_config()?;
    let name = rustls::pki_types::ServerName::try_from(idna_encode(server_hostname)?)
        .map_err(|e| format!("ValueError: {e}"))?;
    let mut tls = rustls::ClientConnection::new(cfg, name).map_err(|e| format!("ssl.SSLError: {e}"))?;
    let mut tcp = tcp;
    while tls.is_handshaking() {
        tls.complete_io(&mut tcp).map_err(|e| format!("ssl.SSLError: {e}"))?;
    }
    Ok(Sock::Tls(Box::new(rustls::StreamOwned::new(tls, tcp))))
}

// -- the response

const MAXLINE: usize = 65536;
const MAXHEADERS: usize = 100;

/// `_read_status()`.
fn read_status(conn: &mut Conn) -> Result<(String, i128, String), String> {
    let raw = conn.readline(MAXLINE + 1)?;
    if raw.len() > MAXLINE {
        return Err("LineTooLong: status line".into());
    }
    if raw.is_empty() {
        return Err("RemoteDisconnected: Remote end closed connection without response".into());
    }
    let line: String = raw.iter().map(|&b| b as char).collect();
    let parts = py_split(&line, 2);
    let (version, status, reason) = match parts.len() {
        3 => (parts[0], parts[1], parts[2]),
        2 => (parts[0], parts[1], ""),
        _ => ("", "", ""),
    };
    if !version.starts_with("HTTP/") {
        return Err(format!("BadStatusLine: {line:?}"));
    }
    match py_int(status, 10, false) {
        Some(s) if (100..=999).contains(&s) => Ok((version.to_string(), s, reason.to_string())),
        _ => Err(format!("BadStatusLine: {line:?}")),
    }
}

/// `_read_headers()`: the raw lines, the blank one included.
fn read_header_lines(conn: &mut Conn) -> Result<Vec<u8>, String> {
    let mut all = Vec::new();
    let mut count = 0;
    loop {
        let line = conn.readline(MAXLINE + 1)?;
        if line.len() > MAXLINE {
            return Err("LineTooLong: header line".into());
        }
        count += 1;
        if count > MAXHEADERS {
            return Err(format!("HTTPException: got more than {MAXHEADERS} headers"));
        }
        let end = matches!(line.as_slice(), b"\r\n" | b"\n" | b"");
        all.extend_from_slice(&line);
        if end {
            return Ok(all);
        }
    }
}

/// The lines with their endings, split as the email package splits them.
fn split_keepends(s: &str) -> Vec<&str> {
    let b = s.as_bytes();
    let (mut out, mut start, mut i) = (Vec::new(), 0, 0);
    while i < b.len() {
        let end = match b[i] {
            b'\r' if b.get(i + 1) == Some(&b'\n') => i + 2,
            b'\r' | b'\n' => i + 1,
            _ => {
                i += 1;
                continue;
            }
        };
        out.push(&s[start..end]);
        start = end;
        i = end;
    }
    if start < b.len() {
        out.push(&s[start..]);
    }
    out
}

/// `email.parser.Parser().parsestr(...)` of a header block, compat32:
/// the header lines end at the first line that is not one.
fn parse_headers(raw: &[u8]) -> Vec<(String, String)> {
    let text: String = raw.iter().map(|&b| b as char).collect();
    let is_header = |line: &str| {
        if line.starts_with("From ") || line.starts_with([' ', '\t']) {
            return true;
        }
        let name_end = line.find(|c: char| !matches!(c, '\x21'..='\x39' | '\x3b'..='\x7e')).unwrap_or(line.len());
        line[name_end..].starts_with(':')
    };
    let lines: Vec<&str> = split_keepends(&text).into_iter().take_while(|l| is_header(l)).collect();
    let source_parse = |lines: &[&str]| {
        let (name, value) = lines[0].split_once(':').unwrap_or((lines[0], ""));
        let mut v = value.to_string();
        v.extend(lines[1..].iter().copied());
        (name.to_string(), v.trim_start_matches([' ', '\t', '\r', '\n']).trim_end_matches(['\r', '\n']).to_string())
    };
    let mut out = Vec::new();
    let mut last: Vec<&str> = Vec::new();
    for (lineno, &line) in lines.iter().enumerate() {
        if line.starts_with([' ', '\t']) {
            if !last.is_empty() {
                last.push(line);
            }
            continue;
        }
        if !last.is_empty() {
            out.push(source_parse(&last));
            last.clear();
        }
        if line.starts_with("From ") {
            if lineno == lines.len() - 1 && lineno != 0 {
                return out;
            }
            continue;
        }
        match line.find(':') {
            Some(0) | None => continue,
            Some(_) => last.push(line),
        }
    }
    if !last.is_empty() {
        out.push(source_parse(&last));
    }
    out
}

/// `msg.get(name)`: the first, by name in any case.
fn header_get(h: &[(String, String)], name: &str) -> Option<String> {
    h.iter().find(|(k, _)| k.to_lowercase() == name).map(|(_, v)| v.clone())
}

/// `http.client.HTTPResponse`.
struct HttpResp {
    conn: Conn,
    status: i128,
    headers: Vec<(String, String)>,
    chunked: bool,
    chunk_left: Option<i128>,
    length: Option<i128>,
    /// `fp is None`: the body is done, every read gives b"".
    closed: bool,
}

impl HttpResp {
    /// `begin()`.
    fn begin(mut conn: Conn) -> Result<HttpResp, String> {
        let mut first = None;
        for _ in 0..100 {
            let (version, status, _) = read_status(&mut conn)?;
            if status != 100 {
                first = Some((version, status));
                break;
            }
            read_header_lines(&mut conn)?;
        }
        let Some((version, status)) = first else {
            return Err("HTTPException: got more than 100 interim responses".into());
        };
        if !(version == "HTTP/1.0" || version == "HTTP/0.9" || version.starts_with("HTTP/1.")) {
            return Err(format!("UnknownProtocol: {version}"));
        }
        let headers = parse_headers(&read_header_lines(&mut conn)?);
        let chunked = header_get(&headers, "transfer-encoding").is_some_and(|t| t.to_lowercase() == "chunked");
        let mut length = None;
        if let Some(l) = header_get(&headers, "content-length").filter(|l| !l.is_empty()) {
            if !chunked {
                length = py_int(&l, 10, false).filter(|&n| n >= 0);
            }
        }
        if status == 204 || status == 304 || (100..200).contains(&status) {
            length = Some(0);
        }
        Ok(HttpResp { conn, status, headers, chunked, chunk_left: None, length, closed: false })
    }

    /// `_safe_read(n)`: exactly n bytes, or IncompleteRead. A negative n
    /// (a negative chunk size) reads to EOF, as `fp.read(-n)` does.
    fn safe_read(&mut self, n: i128) -> Result<Vec<u8>, String> {
        if n < 0 {
            return self.conn.read_to_end();
        }
        let want = usize::try_from(n).unwrap_or(usize::MAX);
        let data = self.conn.read(want)?;
        if data.len() < want {
            return Err("IncompleteRead".into());
        }
        Ok(data)
    }

    /// `_get_chunk_left()`.
    fn get_chunk_left(&mut self) -> Result<Option<i128>, String> {
        let chunk_left = self.chunk_left;
        if chunk_left.is_none() || chunk_left == Some(0) {
            if chunk_left.is_some() {
                self.safe_read(2)?; // toss the CRLF at the end of the chunk
            }
            let line = self.conn.readline(MAXLINE + 1)?;
            if line.len() > MAXLINE {
                return Err("LineTooLong: chunk size".into());
            }
            let line = match line.iter().position(|&b| b == b';') {
                Some(i) => &line[..i], // strip chunk-extensions
                None => &line[..],
            };
            let text: String = line.iter().map(|&b| b as char).collect();
            let Some(n) = py_int(&text, 16, true) else {
                return Err("IncompleteRead".into());
            };
            if n == 0 {
                // _read_and_discard_trailer()
                let mut trailers = 0;
                loop {
                    let l = self.conn.readline(MAXLINE + 1)?;
                    if l.len() > MAXLINE {
                        return Err("LineTooLong: trailer line".into());
                    }
                    if matches!(l.as_slice(), b"" | b"\r\n" | b"\n") {
                        break;
                    }
                    trailers += 1;
                    if trailers > MAXHEADERS {
                        return Err(format!("HTTPException: got more than {MAXHEADERS} trailers"));
                    }
                }
                // we read everything; close the "file"
                self.chunk_left = None;
                self.closed = true;
                return Ok(None);
            }
            self.chunk_left = Some(n);
            return Ok(Some(n));
        }
        Ok(chunk_left)
    }

    /// `_read_chunked(amt)`.
    fn read_chunked(&mut self, mut amt: Option<i128>) -> Result<Vec<u8>, String> {
        let mut value = Vec::new();
        while let Some(chunk_left) = self.get_chunk_left()? {
            if let Some(a) = amt {
                if a <= chunk_left {
                    value.extend(self.safe_read(a)?);
                    self.chunk_left = Some(chunk_left - a);
                    break;
                }
            }
            value.extend(self.safe_read(chunk_left)?);
            amt = amt.map(|a| a - chunk_left);
            self.chunk_left = Some(0);
        }
        Ok(value)
    }

    /// `read(amt)`. A body cut short of its Content-Length is an
    /// IncompleteRead (http.client returns it as it is).
    fn read(&mut self, amt: usize) -> Result<Vec<u8>, String> {
        if self.closed {
            return Ok(Vec::new());
        }
        if self.chunked {
            return self.read_chunked(Some(amt as i128));
        }
        // clip the read to the "end of response"
        let amt = match self.length {
            Some(l) if (amt as i128) > l => usize::try_from(l).unwrap_or(0),
            _ => amt,
        };
        let s = self.conn.read(amt)?;
        if s.is_empty() && amt > 0 {
            if let Some(left) = self.length.filter(|&l| l > 0) {
                return Err(format!("IncompleteRead: {left} more bytes expected"));
            }
            self.closed = true;
        } else if let Some(l) = self.length {
            let left = l - s.len() as i128;
            self.length = Some(left);
            if left == 0 {
                self.closed = true;
            }
        }
        Ok(s)
    }

    /// `read()`.
    fn read_all(&mut self) -> Result<Vec<u8>, String> {
        if self.closed {
            return Ok(Vec::new());
        }
        if self.chunked {
            return self.read_chunked(None);
        }
        match self.length {
            None => self.conn.read_to_end(),
            Some(l) => self.safe_read(l),
        }
    }
}

enum Resp {
    Http(Box<HttpResp>),
    /// `addinfourl` from a `file:` or `data:` URL: no status.
    Local,
}

// -- the handlers

/// `HTTPConnection._get_hostport(host, None)`.
fn get_hostport(host: &str, default_port: i128) -> Result<(String, i128), String> {
    let i = host.rfind(':');
    let j = host.rfind(']');
    let (h, port) = match i {
        Some(i) if j.is_none_or(|j| i > j) => {
            let p = &host[i + 1..];
            let port = match py_int(p, 10, false) {
                Some(n) => n,
                None if p.is_empty() => default_port,
                None => return Err(format!("InvalidURL: nonnumeric port: '{p}'")),
            };
            (&host[..i], port)
        }
        _ => (host, default_port),
    };
    let h = if h.starts_with('[') && h.ends_with(']') && h.len() >= 2 { &h[1..h.len() - 1] } else { h };
    Ok((h.to_string(), port))
}

fn check_url_chars(s: &str) -> Result<(), String> {
    if s.chars().any(|c| c <= ' ' || c == '\x7f') {
        return Err(format!("InvalidURL: URL can't contain control characters. {s:?}"));
    }
    Ok(())
}

/// `putheader(name, value)`.
fn putheader(out: &mut Vec<u8>, name: &str, value: Option<&str>) -> Result<(), String> {
    let Some(value) = value else {
        return Err("TypeError: header value is None".into());
    };
    let legal_name = name.is_ascii()
        && name.chars().next().is_some_and(|c| c != ':' && !c.is_ascii_whitespace() && c != '\x0b')
        && name.chars().skip(1).all(|c| !matches!(c, ':' | '\r' | '\n'));
    if !legal_name {
        return Err(format!("ValueError: Invalid header name {name:?}"));
    }
    let mut v = Vec::new();
    for c in value.chars() {
        let b = u8::try_from(u32::from(c)).map_err(|_| "UnicodeEncodeError: 'latin-1' codec can't encode".to_string())?;
        v.push(b);
    }
    // \n(?![ \t])|\r(?![ \t\n])
    let illegal = v.iter().enumerate().any(|(i, &b)| match b {
        b'\n' => !matches!(v.get(i + 1), Some(b' ' | b'\t')),
        b'\r' => !matches!(v.get(i + 1), Some(b' ' | b'\t' | b'\n')),
        _ => false,
    });
    if illegal {
        return Err("ValueError: Invalid header value".into());
    }
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(&v);
    out.extend_from_slice(b"\r\n");
    Ok(())
}

/// `AbstractHTTPHandler.do_request_`.
fn do_request_(req: &mut Req) -> Result<(), String> {
    let Some(host) = req.host.clone().filter(|h| !h.is_empty()) else {
        return Err("URLError: no host given".into());
    };
    let mut sel_host = Some(host);
    if req.has_proxy() {
        let (_, sel) = _splittype(&req.selector);
        sel_host = _splithost(sel).0;
    }
    if !req.has_header("Host") {
        dict_set(&mut req.unredirected, "Host", sel_host);
    }
    if !req.has_header("User-agent") {
        dict_set(&mut req.unredirected, "User-agent", Some("Python-urllib/3.14".to_string()));
    }
    Ok(())
}

/// `AbstractHTTPHandler.do_open`, with HTTPConnection / HTTPSConnection.
fn do_open(req: &mut Req, https: bool) -> Result<Resp, String> {
    let Some(host) = req.host.clone().filter(|h| !h.is_empty()) else {
        return Err("URLError: no host given".into());
    };
    let default_port = if https { 443 } else { 80 };
    let (h_host, h_port) = get_hostport(&host, default_port)?;
    check_url_chars(&h_host)?;

    let mut merged: Vec<(String, Option<String>)> = req.unredirected.clone();
    for (k, v) in &req.headers {
        dict_set(&mut merged, k, Some(v.clone()));
    }
    dict_set(&mut merged, "Connection", Some("close".to_string()));
    let mut headers: Vec<(String, Option<String>)> = Vec::new();
    for (k, v) in merged {
        dict_set(&mut headers, &crate::util::py_title(&k), v);
    }
    let mut tunnel = None;
    if let Some(th) = req.tunnel_host.clone().filter(|t| !t.is_empty()) {
        let mut tunnel_headers: Vec<(String, String)> = Vec::new();
        if let Some(v) = dict_pop(&mut headers, "Proxy-Authorization") {
            tunnel_headers.push(("Proxy-Authorization".into(), v.unwrap_or_default()));
        }
        let (t_host, t_port) = get_hostport(&th, default_port)?;
        if !tunnel_headers.iter().any(|(k, _)| k.to_lowercase() == "host") {
            tunnel_headers.push(("Host".into(), format!("{}:{t_port}", idna_encode(&t_host)?)));
        }
        tunnel = Some((t_host, t_port, tunnel_headers));
    }

    // putrequest + putheader: everything is checked before connecting.
    let url = if req.selector.is_empty() { "/" } else { req.selector.as_str() };
    check_url_chars(url)?;
    if !url.is_ascii() {
        return Err("UnicodeEncodeError: 'ascii' codec can't encode the request line".into());
    }
    let mut out = format!("GET {url} HTTP/1.1\r\n").into_bytes();
    if !headers.iter().any(|(k, _)| k.to_lowercase() == "accept-encoding") {
        putheader(&mut out, "Accept-Encoding", Some("identity"))?;
    }
    for (k, v) in &headers {
        putheader(&mut out, k, v.as_deref())?;
    }
    out.extend_from_slice(b"\r\n");

    let tcp = create_connection(&h_host, h_port, req.timeout)?;
    let mut sni = h_host.clone();
    let tcp = match &tunnel {
        Some((t_host, t_port, tunnel_headers)) => {
            check_url_chars(t_host).map_err(|_| "ValueError: Tunnel host can't contain control characters".to_string())?;
            let mut target = idna_encode(t_host)?;
            if target.contains(':') && !target.starts_with('[') {
                target = format!("[{target}]");
            }
            let mut connect = format!("CONNECT {target}:{t_port} HTTP/1.1\r\n").into_bytes();
            for (k, v) in tunnel_headers {
                putheader(&mut connect, k, Some(v))?;
            }
            connect.extend_from_slice(b"\r\n");
            let mut c = Conn::new(Sock::Tcp(tcp));
            c.send(&connect)?;
            let (_, code, message) = read_status(&mut c)?;
            read_header_lines(&mut c)?;
            if code != 200 {
                return Err(format!("OSError: Tunnel connection failed: {code} {}", py_strip(&message)));
            }
            sni = t_host.clone();
            match c.sock {
                Sock::Tcp(t) => t,
                Sock::Tls(_) => return Err("OSError: tunnel".into()),
            }
        }
        None => tcp,
    };
    let sock = if https { wrap_tls(tcp, &sni)? } else { Sock::Tcp(tcp) };
    let mut conn = Conn::new(sock);
    conn.send(&out)?;
    Ok(Resp::Http(Box::new(HttpResp::begin(conn)?)))
}

/// `ProxyHandler.proxy_open`: `None` lets the next handler take it.
fn proxy_open(req: &mut Req, proxy: &str, depth: usize) -> Result<Option<Resp>, String> {
    let orig_type = req.typ.clone();
    let (ptype, user, password, hostport) = _parse_proxy(proxy)?;
    let ptype = ptype.unwrap_or_else(|| orig_type.clone());
    if req.host.as_deref().is_some_and(|h| !h.is_empty() && proxy_bypass(h)) {
        return Ok(None);
    }
    if let (Some(u), Some(p)) = (user.filter(|u| !u.is_empty()), password.filter(|p| !p.is_empty())) {
        let creds = b64encode(format!("{}:{}", unquote(&u), unquote(&p)).as_bytes());
        dict_set(&mut req.headers, "Proxy-authorization", format!("Basic {creds}"));
    }
    req.set_proxy(unquote(&hostport), ptype.clone());
    if orig_type == ptype || orig_type == "https" {
        return Ok(None);
    }
    // need to start over, because the other handlers don't grok the proxy's
    // URL type
    Ok(Some(open(req.clone(), depth + 1)?))
}

fn _open(req: &mut Req, depth: usize) -> Result<Resp, String> {
    let t = req.typ.clone();
    if let Some(proxy) = dict_get(proxies(), &t).cloned() {
        if let Some(r) = proxy_open(req, &proxy, depth)? {
            return Ok(r);
        }
    }
    match t.as_str() {
        "http" => do_open(req, false),
        "https" => do_open(req, true),
        // Reached only through a proxy whose URL names another scheme. Such a
        // response has no status, and HTTPErrorProcessor then raises on it.
        "file" | "data" => Ok(Resp::Local),
        // urllib would try an FTP download (a redirect may lead there).
        "ftp" => Err("URLError: ftp is not supported here".into()),
        other => Err(format!("URLError: unknown url type: {other}")),
    }
}

/// `HTTPRedirectHandler.http_error_302` (and the default handler's
/// HTTPError for every other status).
fn http_error(req: Req, resp: Resp, code: i128, depth: usize) -> Result<Resp, String> {
    let http_error = || Err(format!("HTTPError: HTTP Error {code}"));
    let Resp::Http(mut h) = resp else {
        return http_error();
    };
    if !matches!(code, 301 | 302 | 303 | 307 | 308) {
        return http_error();
    }
    let Some(newurl) = header_get(&h.headers, "location").or_else(|| header_get(&h.headers, "uri")) else {
        return http_error();
    };
    let mut parts = urlparse(&newurl)?;
    if !["http", "https", "ftp", ""].contains(&parts[0].as_str()) {
        return http_error();
    }
    if parts[2].is_empty() && !parts[1].is_empty() {
        parts[2] = "/".into();
    }
    let newurl = quote_redirect(&urlunparse(&parts))?;
    let newurl = urljoin(&req.full_url(), &newurl)?;
    if !redirect_allowed(&req.typ, &newurl)? {
        return Err(format!("HTTPError: HTTP Error {code}: refusing a redirect from https to {newurl}"));
    }
    let headers: Vec<(String, String)> = req
        .headers
        .iter()
        .filter(|(k, _)| !matches!(k.to_lowercase().as_str(), "content-length" | "content-type"))
        .cloned()
        .collect();
    let mut new = Req::new(&newurl.replace(' ', "%20"), &headers, false, req.timeout)?;
    let visited = match &req.redirect_dict {
        Some(v) => {
            let d = v.borrow();
            if d.get(&newurl).copied().unwrap_or(0) >= 4 || d.len() >= 10 {
                return http_error();
            }
            v.clone()
        }
        None => Rc::new(RefCell::new(HashMap::new())),
    };
    *visited.borrow_mut().entry(newurl).or_insert(0) += 1;
    new.redirect_dict = Some(visited);
    h.read_all()?;
    open(new, depth + 1)
}

/// Whether a redirect from a `from` URL to `to` may be followed. urllib
/// follows https to http; that would hand the download to anyone on the
/// path, so it is refused.
pub(super) fn redirect_allowed(from: &str, to: &str) -> Result<bool, String> {
    Ok(!from.eq_ignore_ascii_case("https") || urlparse(to)?[0].eq_ignore_ascii_case("https"))
}

/// `OpenerDirector.open(req)`.
fn open(mut req: Req, depth: usize) -> Result<Resp, String> {
    if depth > 100 {
        return Err("RecursionError: maximum recursion depth exceeded".into());
    }
    let protocol = req.typ.clone();
    let is_http = protocol == "http" || protocol == "https";
    if is_http {
        do_request_(&mut req)?;
    }
    let resp = _open(&mut req, depth)?;
    if is_http {
        // HTTPErrorProcessor
        let code = match &resp {
            Resp::Http(h) => h.status,
            Resp::Local => return Err("TypeError: '<=' not supported between instances of 'int' and 'NoneType'".into()),
        };
        if !(200..300).contains(&code) {
            return http_error(req, resp, code, depth);
        }
    }
    Ok(resp)
}


/// What urllib fixes when it builds its opener, on the first `urlopen` of
/// the process: the proxy table and the HTTPS context.
pub(super) fn build_opener() {
    let _ = proxies();
    let _ = tls_config();
}

/// `Request(url, headers=UA)`'s checks that come before any handler:
/// `request_host` runs `urlparse`, which may raise.
pub(super) fn check_request(full_url: &str) -> Result<(), String> {
    urlparse(full_url).map(|_| ())
}

/// `urlopen(Request(url, headers=UA), timeout=...)` for an http(s) URL:
/// the response's body and `Content-Length` header.
pub(super) fn open_http(url: &str, ua: (&str, &str), timeout: Duration) -> Result<(Box<dyn Read + Send>, Option<String>), String> {
    let headers = [(ua.0.to_string(), ua.1.to_string())];
    let req = Req::new(url, &headers, true, timeout)?;
    match open(req, 0)? {
        Resp::Http(r) => {
            let length = header_get(&r.headers, "content-length");
            Ok((Box::new(Body(r)), length))
        }
        Resp::Local => Err("TypeError: '<=' not supported between instances of 'int' and 'NoneType'".into()),
    }
}

/// `HTTPResponse.read(n)` as a reader.
struct Body(Box<HttpResp>);

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let data = self.0.read(buf.len()).map_err(std::io::Error::other)?;
        buf[..data.len()].copy_from_slice(&data);
        Ok(data.len())
    }
}
