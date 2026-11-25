//! The Chromecast protocol (CASTV2): protobuf-framed JSON messages over TLS
//! on port 8009. The device's "Default Media Receiver" is started with the
//! first file; it fetches the media from our server by itself.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::discovery::Device;
use super::session::{Cmd, Media, Phase, Shared, update};

/// Google's Default Media Receiver: plays plain links, on every device.
const APP: &str = "CC1AD845";
const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
const ME: &str = "sender-smp";
const RECEIVER: &str = "receiver-0";
/// Larger messages than this are not part of the protocol.
const MAX_MESSAGE: usize = 256 * 1024;

// ---- framing ----

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Message {
    pub source: String,
    pub dest: String,
    pub ns: String,
    pub payload: String,
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn take_varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let x = *b.get(*i)?;
        *i += 1;
        v |= u64::from(x & 0x7F) << shift;
        if x & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

impl Message {
    /// With its length in front, as sent.
    pub fn encode(&self) -> Vec<u8> {
        // protocol_version = CASTV2_1_0 (0), payload_type = STRING (0).
        let mut b = vec![0x08, 0];
        let string = |b: &mut Vec<u8>, field: u8, s: &str| {
            b.push(field << 3 | 2);
            varint(b, s.len() as u64);
            b.extend_from_slice(s.as_bytes());
        };
        string(&mut b, 2, &self.source);
        string(&mut b, 3, &self.dest);
        string(&mut b, 4, &self.ns);
        b.extend([0x28, 0]);
        string(&mut b, 6, &self.payload);
        let mut out = (b.len() as u32).to_be_bytes().to_vec();
        out.extend(b);
        out
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut m = Self::default();
        let mut i = 0;
        while i < b.len() {
            let key = take_varint(b, &mut i)?;
            match key & 7 {
                0 => {
                    take_varint(b, &mut i)?;
                }
                2 => {
                    let n = usize::try_from(take_varint(b, &mut i)?).ok()?;
                    let bytes = b.get(i..i.checked_add(n)?)?;
                    i += n;
                    let s = || String::from_utf8_lossy(bytes).into_owned();
                    match key >> 3 {
                        2 => m.source = s(),
                        3 => m.dest = s(),
                        4 => m.ns = s(),
                        6 => m.payload = s(),
                        _ => {}
                    }
                }
                5 => i += 4,
                1 => i += 8,
                _ => return None,
            }
        }
        Some(m)
    }
}

/// The complete messages at the start of `buf`, taken out of it.
fn take_messages(buf: &mut Vec<u8>) -> Result<Vec<Message>, String> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(len) = buf.get(at..at + 4) {
        let n = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        if n > MAX_MESSAGE {
            return Err("the answer isn't a Chromecast's".into());
        }
        let Some(body) = buf.get(at + 4..at + 4 + n) else { break };
        out.extend(Message::decode(body));
        at += 4 + n;
    }
    buf.drain(..at);
    Ok(out)
}

// ---- TLS ----

/// Chromecasts have self-signed certificates: there is no name to check, as
/// with every sender app. The connection stays inside the home network.
#[derive(Debug)]
struct AnyCertificate(rustls::crypto::CryptoProvider);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

fn connect(d: &Device) -> Result<Tls, String> {
    let tcp = TcpStream::connect_timeout(&d.addr, Duration::from_secs(5))
        .map_err(|e| format!("{} didn't answer ({e})", d.name))?;
    let _ = tcp.set_nodelay(true);
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(5)));
    let provider = rustls::crypto::ring::default_provider();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(provider.clone()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::IpAddress(d.addr.ip().into());
    let conn = rustls::ClientConnection::new(Arc::new(cfg), name).map_err(|e| e.to_string())?;
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    // The handshake, with the long timeout; then short reads.
    while tls.conn.is_handshaking() {
        tls.conn.complete_io(&mut tls.sock).map_err(|e| format!("couldn't connect securely to {} ({e})", d.name))?;
    }
    let _ = tls.sock.set_read_timeout(Some(Duration::from_millis(100)));
    Ok(tls)
}

// ---- the session ----

struct Cast<'a> {
    tls: Tls,
    status: &'a Shared,
    request: u64,
    /// The receiver app's address and session, once it runs.
    transport: Option<String>,
    app_session: Option<String>,
    launching: bool,
    /// Waiting for the receiver app.
    pending: Option<Box<Media>>,
    media_session: Option<i64>,
    load_request: u64,
    load_id: u64,
}

pub fn run(d: &Device, rx: &Receiver<Cmd>, status: &Shared) -> Result<(), String> {
    let tls = connect(d)?;
    let mut c = Cast {
        tls,
        status,
        request: 1,
        transport: None,
        app_session: None,
        launching: false,
        pending: None,
        media_session: None,
        load_request: 0,
        load_id: 0,
    };
    c.send(RECEIVER, NS_CONNECTION, json!({"type": "CONNECT", "origin": {}}))?;
    c.ask(RECEIVER, NS_RECEIVER, json!({"type": "GET_STATUS"}))?;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    let (mut heard, mut pinged, mut polled) = (Instant::now(), Instant::now(), Instant::now());
    loop {
        match c.tls.read(&mut chunk) {
            Ok(0) => return Err(format!("{} closed the connection", d.name)),
            Ok(n) => {
                heard = Instant::now();
                buf.extend_from_slice(&chunk[..n]);
                for m in take_messages(&mut buf)? {
                    c.handle(&m)?;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            Err(e) => return Err(format!("the connection to {} was lost ({e})", d.name)),
        }
        loop {
            match rx.try_recv() {
                Ok(Cmd::Quit) | Err(TryRecvError::Disconnected) => {
                    c.quit();
                    return Ok(());
                }
                Ok(cmd) => c.command(cmd)?,
                Err(TryRecvError::Empty) => break,
            }
        }
        if heard.elapsed() > Duration::from_secs(20) {
            return Err(format!("{} stopped answering", d.name));
        }
        if pinged.elapsed() > Duration::from_secs(5) {
            pinged = Instant::now();
            c.send(RECEIVER, NS_HEARTBEAT, json!({"type": "PING"}))?;
        }
        if polled.elapsed() > Duration::from_secs(1) {
            polled = Instant::now();
            if c.media_session.is_some() && let Some(t) = c.transport.clone() {
                c.ask(&t, NS_MEDIA, json!({"type": "GET_STATUS"}))?;
            }
        }
    }
}

impl Cast<'_> {
    fn send(&mut self, dest: &str, ns: &str, body: Value) -> Result<(), String> {
        let m = Message { source: ME.into(), dest: dest.into(), ns: ns.into(), payload: body.to_string() };
        self.tls
            .write_all(&m.encode())
            .and_then(|()| self.tls.flush())
            .map_err(|e| format!("the connection was lost ({e})"))
    }

    /// Send a request (it gets a number); returns the number.
    fn ask(&mut self, dest: &str, ns: &str, mut body: Value) -> Result<u64, String> {
        self.request += 1;
        body["requestId"] = json!(self.request);
        self.send(dest, ns, body)?;
        Ok(self.request)
    }

    /// A command for what is playing.
    fn media(&mut self, mut body: Value) -> Result<(), String> {
        let (Some(t), Some(id)) = (self.transport.clone(), self.media_session) else { return Ok(()) };
        body["mediaSessionId"] = json!(id);
        self.ask(&t, NS_MEDIA, body).map(|_| ())
    }

    fn command(&mut self, cmd: Cmd) -> Result<(), String> {
        match cmd {
            Cmd::Load(m) => {
                update(self.status, |s| {
                    s.phase = Phase::Loading;
                    s.id = m.id;
                    s.position = m.start;
                    s.at = Instant::now();
                });
                if self.transport.is_some() {
                    return self.load(&m);
                }
                self.pending = Some(m);
                if !self.launching {
                    self.launching = true;
                    self.ask(RECEIVER, NS_RECEIVER, json!({"type": "LAUNCH", "appId": APP}))?;
                }
                Ok(())
            }
            Cmd::Play => self.media(json!({"type": "PLAY"})),
            Cmd::Pause => self.media(json!({"type": "PAUSE"})),
            Cmd::Seek(t) => self.media(json!({"type": "SEEK", "currentTime": t})),
            Cmd::Volume(v) => {
                self.ask(RECEIVER, NS_RECEIVER, json!({"type": "SET_VOLUME", "volume": {"level": v}})).map(|_| ())
            }
            Cmd::Mute(m) => {
                self.ask(RECEIVER, NS_RECEIVER, json!({"type": "SET_VOLUME", "volume": {"muted": m}})).map(|_| ())
            }
            Cmd::Quit => Ok(()),
        }
    }

    fn load(&mut self, m: &Media) -> Result<(), String> {
        let Some(t) = self.transport.clone() else { return Ok(()) };
        self.media_session = None;
        self.load_id = m.id;
        self.load_request = self.ask(&t, NS_MEDIA, load_body(m))?;
        Ok(())
    }

    fn quit(&mut self) {
        if let Some(id) = self.app_session.take() {
            let _ = self.ask(RECEIVER, NS_RECEIVER, json!({"type": "STOP", "sessionId": id}));
        }
        if let Some(t) = self.transport.take() {
            let _ = self.send(&t, NS_CONNECTION, json!({"type": "CLOSE"}));
        }
        let _ = self.send(RECEIVER, NS_CONNECTION, json!({"type": "CLOSE"}));
        self.tls.conn.send_close_notify();
        let _ = self.tls.flush();
    }

    fn failed(&self, why: String) {
        let id = self.load_id;
        update(self.status, |s| {
            if s.id <= id {
                s.phase = Phase::Failed(why);
            }
        });
    }

    fn handle(&mut self, m: &Message) -> Result<(), String> {
        let Ok(v) = serde_json::from_str::<Value>(&m.payload) else { return Ok(()) };
        let kind = v["type"].as_str().unwrap_or("");
        match (m.ns.as_str(), kind) {
            (NS_HEARTBEAT, "PING") => self.send(&m.source, NS_HEARTBEAT, json!({"type": "PONG"}))?,
            (NS_CONNECTION, "CLOSE") => {
                if Some(&m.source) == self.transport.as_ref() {
                    self.gone();
                }
            }
            (NS_RECEIVER, "RECEIVER_STATUS") => self.receiver_status(&v["status"])?,
            (NS_RECEIVER, "LAUNCH_ERROR") => {
                self.launching = false;
                if let Some(m) = self.pending.take() {
                    self.load_id = m.id;
                }
                self.failed("it couldn't start its player".into());
            }
            (NS_MEDIA, "MEDIA_STATUS") => {
                let asked = v["requestId"].as_u64().unwrap_or(0);
                if let Some(st) = v["status"].as_array().and_then(|a| a.first()) {
                    self.media_status(asked, st);
                }
            }
            (NS_MEDIA, "LOAD_FAILED" | "LOAD_CANCELLED" | "INVALID_REQUEST")
                if v["requestId"].as_u64() == Some(self.load_request) =>
            {
                self.failed("it can't play this format".into());
            }
            _ => {}
        }
        Ok(())
    }

    /// The receiver app went away: another app took over, or the TV's remote
    /// stopped it.
    fn gone(&mut self) {
        self.transport = None;
        self.app_session = None;
        self.media_session = None;
        update(self.status, |s| s.phase = Phase::Stopped);
    }

    fn receiver_status(&mut self, st: &Value) -> Result<(), String> {
        update(self.status, |s| {
            s.volume = st["volume"]["level"].as_f64().or(s.volume);
            s.muted = st["volume"]["muted"].as_bool().or(s.muted);
            if s.phase == Phase::Connecting {
                s.phase = Phase::Ready;
            }
        });
        let ours = st["applications"].as_array().and_then(|a| a.iter().find(|a| a["appId"] == APP));
        match ours.and_then(|a| Some((a["transportId"].as_str()?, a["sessionId"].as_str()?))) {
            // Before our own launch, someone else may be using the same app.
            Some((t, s)) if self.launching => {
                self.launching = false;
                self.transport = Some(t.to_owned());
                self.app_session = Some(s.to_owned());
                self.send(t, NS_CONNECTION, json!({"type": "CONNECT", "origin": {}}))?;
                if let Some(m) = self.pending.take() {
                    self.load(&m)?;
                }
            }
            Some((_, s)) if self.app_session.as_deref().is_some_and(|o| o != s) => self.gone(),
            None if self.transport.is_some() => self.gone(),
            _ => {}
        }
        Ok(())
    }

    fn media_status(&mut self, asked: u64, st: &Value) {
        let session = st["mediaSessionId"].as_i64();
        let state = st["playerState"].as_str().unwrap_or("");
        if asked == self.load_request && session.is_some() {
            self.media_session = session;
        }
        // About what played before this load.
        if self.media_session.is_none() || session != self.media_session {
            return;
        }
        let phase = match (state, st["idleReason"].as_str()) {
            ("PLAYING", _) => Phase::Playing,
            ("PAUSED", _) => Phase::Paused,
            ("BUFFERING", _) => Phase::Buffering,
            ("LOADING", _) => Phase::Loading,
            ("IDLE", Some("FINISHED")) => Phase::Ended,
            ("IDLE", Some("ERROR")) => Phase::Failed("it couldn't play this".into()),
            ("IDLE", Some("CANCELLED")) => Phase::Stopped,
            _ => return,
        };
        if matches!(phase, Phase::Ended | Phase::Failed(_) | Phase::Stopped) {
            self.media_session = None;
        }
        let id = self.load_id;
        update(self.status, |s| {
            // Another load was asked for meanwhile.
            if s.id != id {
                return;
            }
            s.phase = phase;
            if let Some(t) = st["currentTime"].as_f64() {
                s.position = t;
                s.at = Instant::now();
            }
            // The stream's own volume is always 1; the device's is in its
            // receiver status.
        });
    }
}

fn load_body(m: &Media) -> Value {
    let mut meta = if m.video {
        json!({"metadataType": 0, "title": m.title})
    } else {
        json!({"metadataType": 3, "title": m.title, "artist": m.artist, "albumName": m.album})
    };
    if let Some(art) = &m.art {
        meta["images"] = json!([{"url": art}]);
    }
    let mut media = json!({
        "contentId": m.url,
        "contentUrl": m.url,
        "contentType": m.mime,
        "streamType": if m.duration > 0.0 { "BUFFERED" } else { "LIVE" },
        "metadata": meta,
    });
    if m.duration > 0.0 {
        media["duration"] = json!(m.duration);
    }
    let mut body = json!({
        "type": "LOAD",
        "autoplay": !m.paused,
        "currentTime": m.start,
    });
    if let Some(vtt) = &m.vtt {
        media["tracks"] = json!([{
            "trackId": 1, "type": "TEXT", "subtype": "SUBTITLES", "name": "Subtitles",
            "language": "en", "trackContentId": vtt, "trackContentType": "text/vtt",
        }]);
        media["textTrackStyle"] = json!({
            "backgroundColor": "#00000000", "foregroundColor": "#FFFFFFFF",
            "edgeType": "OUTLINE", "edgeColor": "#000000FF", "fontScale": 1.1,
        });
        body["activeTrackIds"] = json!([1]);
    }
    body["media"] = media;
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let m = Message {
            source: ME.into(),
            dest: RECEIVER.into(),
            ns: NS_MEDIA.into(),
            payload: format!("{{\"x\":\"{}\"}}", "é".repeat(300)),
        };
        let mut wire = m.encode();
        wire.extend(m.encode());
        // Arrives in pieces.
        let rest = wire.split_off(wire.len() - 10);
        let mut buf = wire;
        assert_eq!(take_messages(&mut buf).unwrap(), vec![m.clone()]);
        buf.extend(rest);
        assert_eq!(take_messages(&mut buf).unwrap(), vec![m.clone()]);
        assert!(buf.is_empty());
        // Bad data: refused or skipped, never a crash.
        assert!(take_messages(&mut vec![0xFF; 8]).is_err());
        let body = &m.encode()[4..];
        for n in 0..body.len() {
            let _ = Message::decode(&body[..n]);
        }
        assert_eq!(Message::decode(&[0x32, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]), None);
    }

    #[test]
    fn load_request() {
        let m = Media {
            url: "http://h/x.mp4".into(),
            mime: "video/mp4".into(),
            title: "Film".into(),
            vtt: Some("http://h/x.vtt".into()),
            start: 12.5,
            duration: 100.0,
            video: true,
            ..Default::default()
        };
        let b = load_body(&m);
        assert_eq!(b["media"]["streamType"], "BUFFERED");
        assert_eq!(b["currentTime"], 12.5);
        assert_eq!(b["autoplay"], true);
        assert_eq!(b["activeTrackIds"], json!([1]));
        assert_eq!(b["media"]["tracks"][0]["trackContentId"], "http://h/x.vtt");
        let song = load_body(&Media { title: "Song".into(), artist: "Band".into(), ..Default::default() });
        assert_eq!(song["media"]["streamType"], "LIVE");
        assert_eq!(song["media"]["metadata"]["artist"], "Band");
        assert_eq!(song.get("activeTrackIds"), None);
    }
}
