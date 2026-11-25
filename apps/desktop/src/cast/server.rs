//! The web server the TV fetches the media from. Like the phone remote's,
//! every address holds a random secret, so nothing else on the network can
//! list or read anything; and it only serves what is being cast. It runs
//! only while casting.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::convert::{Encoder, Job};

/// Port tried first, so one firewall rule is enough (then any free one).
const PORT: u16 = 47_811;
const MAX_CLIENTS: usize = 32;
/// Encoders at once for one file: TVs often open a link twice.
const MAX_ENCODERS: usize = 3;

/// Text made when first asked for (subtitles read out of a film).
pub type Lazy = Arc<OnceLock<Vec<u8>>>;

#[derive(Clone)]
pub enum Content {
    File(PathBuf),
    /// Passed on from another server.
    Remote(String),
    /// Converted while it is sent.
    Live(Arc<Job>),
    /// Filled in by another thread.
    Text(Lazy),
}

#[derive(Clone)]
pub struct Item {
    pub content: Content,
    pub mime: String,
    pub dlna: String,
    /// The subtitles, for Samsung TVs, which ask for them in a header.
    pub srt: Option<String>,
    cancel: Arc<AtomicBool>,
    encoders: Arc<AtomicUsize>,
}

impl Item {
    pub fn new(content: Content, mime: &str) -> Self {
        Self {
            content,
            mime: mime.to_owned(),
            dlna: String::new(),
            srt: None,
            cancel: Arc::default(),
            encoders: Arc::default(),
        }
    }
}

struct Shared {
    token: String,
    items: Mutex<HashMap<String, Item>>,
    stop: AtomicBool,
    /// Requests for something we serve.
    hits: AtomicU64,
    clients: AtomicUsize,
}

pub struct Server {
    shared: Arc<Shared>,
    pub port: u16,
    ip: IpAddr,
    listener: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.clear();
        // Moments: then the port is free for the next device.
        if let Some(t) = self.listener.take() {
            let _ = t.join();
        }
    }
}

impl Server {
    /// Serve to `device` (which picks the network address it can reach).
    pub fn start(device: SocketAddr) -> Result<Self, String> {
        let ip = UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| {
                s.connect(device)?;
                s.local_addr()
            })
            .map_err(|e| format!("this computer has no way to the device ({e})"))?
            .ip();
        // Only on the network the device is on.
        let listener = TcpListener::bind((ip, PORT))
            .or_else(|_| TcpListener::bind((ip, 0)))
            .map_err(|e| format!("couldn't open a port ({e})"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let shared = Arc::new(Shared {
            token: crate::remote::secret(),
            items: Mutex::default(),
            stop: AtomicBool::new(false),
            hits: AtomicU64::new(0),
            clients: AtomicUsize::new(0),
        });
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("cast-server".into())
            .spawn(move || {
                while !sh.stop.load(Ordering::Relaxed) {
                    let Ok((stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    };
                    if sh.clients.load(Ordering::Relaxed) >= MAX_CLIENTS {
                        continue;
                    }
                    sh.clients.fetch_add(1, Ordering::Relaxed);
                    let sh = sh.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = serve(stream, &sh) {
                            log::debug!("cast server: {e}");
                        }
                        sh.clients.fetch_sub(1, Ordering::Relaxed);
                    });
                }
            })
            .map_err(|e| e.to_string())?;
        log::info!("cast server on port {port}");
        Ok(Self { shared, port, ip, listener: Some(thread) })
    }

    /// Serve `item` as `name`; returns its link.
    pub fn add(&self, name: &str, item: Item) -> String {
        if let Ok(mut m) = self.shared.items.lock() {
            m.insert(name.to_owned(), item);
        }
        format!("http://{}/{}/{name}", SocketAddr::new(self.ip, self.port), self.shared.token)
    }

    /// Stop serving everything; transfers under way end.
    pub fn clear(&self) {
        if let Ok(mut m) = self.shared.items.lock() {
            for (_, i) in m.drain() {
                i.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn hits(&self) -> u64 {
        self.shared.hits.load(Ordering::Relaxed)
    }
}

struct Request {
    method: String,
    target: String,
    range: Option<String>,
    wants_captions: bool,
    /// HTTP/1.1: knows chunks.
    chunks: bool,
}

fn read_request(stream: &TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream.try_clone()?.take(16 * 1024));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let mut r = Request {
        method: parts.next().unwrap_or("").to_owned(),
        target: parts.next().unwrap_or("").to_owned(),
        range: None,
        wants_captions: false,
        chunks: parts.next() == Some("HTTP/1.1"),
    };
    loop {
        line.clear();
        if reader.read_line(&mut line)? <= 2 {
            return Ok(r);
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        match k.trim().to_ascii_lowercase().as_str() {
            "range" => r.range = Some(v.trim().to_owned()),
            "getcaptioninfo.sec" => r.wants_captions = true,
            _ => {}
        }
    }
}

/// The bytes asked for by a Range header: first and last. `Err`: none that
/// exist. Several ranges at once count as no range.
fn parse_range(h: &str, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = h.strip_prefix("bytes=") else { return Ok(None) };
    let Some((a, b)) = spec.trim().split_once('-').filter(|_| !spec.contains(',')) else { return Ok(None) };
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() {
        // The last n bytes.
        let n: u64 = b.parse().map_err(|_| ())?;
        return if n == 0 || size == 0 { Err(()) } else { Ok(Some((size.saturating_sub(n), size - 1))) };
    }
    let first: u64 = a.parse().map_err(|_| ())?;
    let last = if b.is_empty() { u64::MAX } else { b.parse().map_err(|_| ())? };
    if first >= size || last < first {
        return Err(());
    }
    Ok(Some((first, last.min(size - 1))))
}

fn serve(stream: TcpStream, sh: &Shared) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    // Short, so a transfer to a paused TV notices when it is called off.
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let req = read_request(&stream)?;
    let path = req.target.split('?').next().unwrap_or("");
    let name = path.strip_prefix('/').and_then(|p| p.strip_prefix(sh.token.as_str())).and_then(|p| p.strip_prefix('/'));
    let item = name.and_then(|n| sh.items.lock().ok()?.get(n).cloned());
    let mut out = Out { stream, cancel: Arc::default(), chunked: false };
    let Some(item) = item else { return out.head("404 Not Found", "text/plain", Some(0), "") };
    log::debug!("cast server: {} {} {:?}", req.method, name.unwrap_or(""), req.range);
    sh.hits.fetch_add(1, Ordering::Relaxed);
    out.cancel = item.cancel.clone();
    let head_only = req.method == "HEAD";
    if req.method == "OPTIONS" {
        return out.head("200 OK", &item.mime, Some(0), "");
    }
    if req.method != "GET" && !head_only {
        return out.head("405 Method Not Allowed", "text/plain", Some(0), "");
    }
    let mut extra = String::from("transferMode.dlna.org: Streaming\r\n");
    if !item.dlna.is_empty() {
        extra += &format!("contentFeatures.dlna.org: {}\r\n", item.dlna);
    }
    if let Some(srt) = item.srt.as_ref().filter(|_| req.wants_captions) {
        extra += &format!("CaptionInfo.sec: {srt}\r\n");
    }
    match &item.content {
        Content::File(path) => {
            let mut f = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(e) => {
                    log::warn!("cast: {}: {e}", path.display());
                    return out.head("404 Not Found", "text/plain", Some(0), "");
                }
            };
            let size = f.metadata()?.len();
            extra += "Accept-Ranges: bytes\r\n";
            let (status, first, len) = match req.range.as_deref().map(|r| parse_range(r, size)) {
                Some(Ok(Some((a, b)))) => {
                    extra += &format!("Content-Range: bytes {a}-{b}/{size}\r\n");
                    ("206 Partial Content", a, b - a + 1)
                }
                Some(Err(())) => {
                    extra += &format!("Content-Range: bytes */{size}\r\n");
                    return out.head("416 Range Not Satisfiable", "text/plain", Some(0), &extra);
                }
                _ => ("200 OK", 0, size),
            };
            out.head(status, &item.mime, Some(len), &extra)?;
            if !head_only {
                f.seek(SeekFrom::Start(first))?;
                out.copy(&mut f.take(len))?;
            }
            Ok(())
        }
        Content::Text(lazy) => {
            // Still being read out of the file: give it a while.
            for _ in 0..600 {
                if lazy.get().is_some() || item.cancel.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let data = lazy.get().map(Vec::as_slice).unwrap_or_default();
            out.head("200 OK", &item.mime, Some(data.len() as u64), &extra)?;
            if !head_only {
                out.copy(&mut &data[..])?;
            }
            Ok(())
        }
        Content::Remote(url) => relay(&mut out, url, &item, &req, head_only, &extra),
        Content::Live(job) => {
            // It has no bytes to jump to: only its start.
            let later = req.range.as_deref().and_then(|r| r.strip_prefix("bytes=")).is_some_and(|r| !r.starts_with("0-"));
            if later {
                return out.head("416 Range Not Satisfiable", "text/plain", Some(0), &extra);
            }
            if head_only {
                return out.head("200 OK", &item.mime, None, &extra);
            }
            if item.encoders.fetch_add(1, Ordering::Relaxed) >= MAX_ENCODERS {
                item.encoders.fetch_sub(1, Ordering::Relaxed);
                return out.head("503 Service Unavailable", "text/plain", Some(0), "");
            }
            let result = live(&mut out, job, &item, req.chunks, &extra);
            item.encoders.fetch_sub(1, Ordering::Relaxed);
            result
        }
    }
}

fn live(out: &mut Out, job: &Job, item: &Item, chunks: bool, extra: &str) -> std::io::Result<()> {
    let mut enc = match Encoder::start(job) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("cast: couldn't convert: {e}");
            return out.head("500 Internal Server Error", "text/plain", Some(0), "");
        }
    };
    // In chunks its end is certain; otherwise it ends with the connection.
    if !chunks {
        out.head("200 OK", &item.mime, None, extra)?;
        return out.copy(&mut enc);
    }
    out.head("200 OK", &item.mime, None, &format!("{extra}Transfer-Encoding: chunked\r\n"))?;
    out.chunked = true;
    out.copy(&mut enc)?;
    out.chunked = false;
    // Only reached when all of it was sent.
    out.write(b"0\r\n\r\n")
}

/// Pass another server's file on (radio, podcasts, a NAS), with the parts
/// asked for.
fn relay(out: &mut Out, url: &str, item: &Item, req: &Request, head_only: bool, extra: &str) -> std::io::Result<()> {
    let mut call = ureq::get(url)
        .header("User-Agent", concat!("SuperMediaPlayer/", env!("CARGO_PKG_VERSION")))
        .header("Accept-Encoding", "identity");
    if let Some(r) = &req.range {
        call = call.header("Range", r);
    }
    let resp = call
        .config()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(20)))
        .build()
        .call();
    let mut resp = match resp {
        Ok(r) => r,
        Err(e) => {
            log::warn!("cast: {url}: {e}");
            return out.head("502 Bad Gateway", "text/plain", Some(0), "");
        }
    };
    let code = resp.status().as_u16();
    if !resp.status().is_success() {
        log::warn!("cast: {url}: {code}");
        return out.head("502 Bad Gateway", "text/plain", Some(0), "");
    }
    let header = |n: &str| resp.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_owned);
    // A header with a line break can't get here: the HTTP library refuses it.
    let len = header("content-length").and_then(|l| l.parse().ok());
    let mut extra = extra.to_owned();
    if let Some(r) = header("content-range").filter(|_| code == 206) {
        extra += &format!("Content-Range: {r}\r\n");
    }
    if code == 206 || header("accept-ranges").is_some_and(|a| a == "bytes") {
        extra += "Accept-Ranges: bytes\r\n";
    }
    out.head(if code == 206 { "206 Partial Content" } else { "200 OK" }, &item.mime, len, &extra)?;
    if head_only {
        return Ok(());
    }
    out.copy(&mut resp.body_mut().as_reader())
}

struct Out {
    stream: TcpStream,
    cancel: Arc<AtomicBool>,
    chunked: bool,
}

impl Out {
    fn head(&mut self, status: &str, mime: &str, len: Option<u64>, extra: &str) -> std::io::Result<()> {
        let len = len.map(|l| format!("Content-Length: {l}\r\n")).unwrap_or_default();
        // Chromecasts play cross-origin: they need the CORS headers.
        let h = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\n{len}{extra}Access-Control-Allow-Origin: *\r\n\
             Access-Control-Allow-Headers: *\r\nAccess-Control-Expose-Headers: *\r\n\
             Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n"
        );
        self.write(h.as_bytes())
    }

    /// Write it all, however slowly the TV reads (a paused one doesn't at
    /// all), until called off.
    fn write(&mut self, mut data: &[u8]) -> std::io::Result<()> {
        while !data.is_empty() {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(ErrorKind::Interrupted.into());
            }
            match self.stream.write(data) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(n) => data = &data[n..],
                Err(e) if waiting(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn copy(&mut self, from: &mut impl Read) -> std::io::Result<()> {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(ErrorKind::Interrupted.into());
            }
            match from.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) if self.chunked => {
                    self.write(format!("{n:x}\r\n").as_bytes())?;
                    self.write(&buf[..n])?;
                    self.write(b"\r\n")?;
                }
                Ok(n) => self.write(&buf[..n])?,
                Err(e) if waiting(&e) => {}
                Err(e) => return Err(e),
            }
        }
    }
}

fn waiting(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Ok(Some((0, 99))));
        assert_eq!(parse_range("bytes=10-19", 100), Ok(Some((10, 19))));
        assert_eq!(parse_range("bytes=90-500", 100), Ok(Some((90, 99))));
        assert_eq!(parse_range("bytes=-5", 100), Ok(Some((95, 99))));
        assert_eq!(parse_range("bytes=-500", 100), Ok(Some((0, 99))));
        assert_eq!(parse_range("bytes=100-", 100), Err(()));
        assert_eq!(parse_range("bytes=20-10", 100), Err(()));
        assert_eq!(parse_range("bytes=x-", 100), Err(()));
        assert_eq!(parse_range("bytes=0-1,5-6", 100), Ok(None));
        assert_eq!(parse_range("items=1", 100), Ok(None));
        assert_eq!(parse_range("bytes=0-", 0), Err(()));
    }

    fn get(url: &str, head: &str) -> (String, Vec<u8>) {
        let rest = url.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let mut s = TcpStream::connect(host).unwrap();
        write!(s, "GET /{path} HTTP/1.1\r\nHost: x\r\n{head}\r\n").unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).unwrap();
        let cut = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (String::from_utf8_lossy(&all[..cut]).into_owned(), all[cut + 4..].to_vec())
    }

    #[test]
    fn serves_only_what_is_cast() {
        let dir = std::env::temp_dir().join(format!("smp-cast-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.bin");
        std::fs::write(&file, (0..=255u8).collect::<Vec<_>>()).unwrap();
        let server = Server::start("127.0.0.1:9".parse().unwrap()).unwrap();
        let mut item = Item::new(Content::File(file), "video/mp4");
        item.dlna = "DLNA.ORG_OP=01".into();
        let url = server.add("1/a.mp4", item);
        let (h, b) = get(&url, "");
        assert!(h.starts_with("HTTP/1.1 200"), "{h}");
        assert!(h.contains("contentFeatures.dlna.org: DLNA.ORG_OP=01") && h.contains("Content-Type: video/mp4"));
        assert_eq!(b.len(), 256);
        let (h, b) = get(&url, "Range: bytes=250-\r\n");
        assert!(h.starts_with("HTTP/1.1 206") && h.contains("Content-Range: bytes 250-255/256"), "{h}");
        assert_eq!(b, [250, 251, 252, 253, 254, 255]);
        assert!(get(&url, "Range: bytes=256-\r\n").0.starts_with("HTTP/1.1 416"));
        // Passed on from another server (here: ours), parts included.
        let relay = server.add("2/b.mp4", Item::new(Content::Remote(url.clone()), "audio/mpeg"));
        let (h, b) = get(&relay, "Range: bytes=1-3\r\n");
        assert!(h.starts_with("HTTP/1.1 206") && h.contains("Content-Type: audio/mpeg"), "{h}");
        assert_eq!(b, [1, 2, 3]);
        // Filled in late.
        let lazy = Lazy::default();
        let text = server.add("1/s.vtt", Item::new(Content::Text(lazy.clone()), "text/vtt"));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let _ = lazy.set(b"WEBVTT".to_vec());
        });
        let (h, b) = get(&text, "");
        assert!(h.contains("Access-Control-Allow-Origin: *"));
        assert_eq!(b, b"WEBVTT");
        // Nothing without the secret, outside the list, or after it ends.
        let host = url.split('/').nth(2).unwrap().to_owned();
        for bad in [format!("http://{host}/1/a.mp4"), format!("http://{host}/"), url.replace("a.mp4", "../../etc/passwd")] {
            assert!(get(&bad, "").0.starts_with("HTTP/1.1 404"), "{bad}");
        }
        server.clear();
        assert!(get(&url, "").0.starts_with("HTTP/1.1 404"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
