//! The phone remote: a small web page served on the local network.
//!
//! Turning it on starts an HTTP server on this computer. Its address holds
//! a random secret (128 bits); every request must carry it, and anything
//! else gets "not found", so only a phone that scanned the QR code can
//! control the player. The page needs no app: it polls the player's state
//! once a second and sends button presses back.
//!
//! The server is plain HTTP inside the home network (browsers can't trust a
//! certificate for a LAN address); the secret is the protection. It is off
//! by default and stops with the player.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Port tried first (then any free one).
const PORT: u16 = 47_801;
/// Most connections served at once.
const MAX_CLIENTS: usize = 16;

/// What the page shows, updated by the UI thread.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub title: String,
    pub artist: String,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub idle: bool,
    pub volume: f64,
    pub muted: bool,
    pub skip: String,
}

/// A button press from the phone.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    TogglePause,
    Next,
    Prev,
    Stop,
    SeekRelative(f64),
    Seek(f64),
    Volume(f64),
    Mute,
    Fullscreen,
    Skip,
}

pub struct Server {
    pub url: String,
    pub state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 128 random bits as hex, from the system's random source.
pub fn secret() -> String {
    let mut b = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_ok();
    if !ok {
        // No /dev/urandom (Windows): the std hasher's random keys.
        use std::hash::{BuildHasher, Hasher};
        for chunk in b.chunks_mut(8) {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
            );
            chunk.copy_from_slice(&h.finish().to_le_bytes());
        }
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// This computer's address on the local network (no packet is sent: a UDP
/// "connect" only picks the route).
pub fn lan_ip() -> Option<std::net::IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:80").ok()?;
    Some(s.local_addr().ok()?.ip()).filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
}

/// Start serving. `on_command` runs on a server thread.
pub fn start(on_command: impl Fn(Command) + Send + Sync + 'static) -> Result<Server, String> {
    let ip = lan_ip().ok_or("this computer is not on a network")?;
    let listener = TcpListener::bind(SocketAddr::new([0, 0, 0, 0].into(), PORT))
        .or_else(|_| TcpListener::bind("0.0.0.0:0"))
        .map_err(|e| format!("couldn't open a port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let token = secret();
    let url = format!("http://{ip}:{port}/{token}/");
    let state = Arc::new(Mutex::new(State::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let (st, stop2) = (state.clone(), stop.clone());
    let on_command = Arc::new(on_command);
    let clients = Arc::new(AtomicUsize::new(0));
    std::thread::Builder::new()
        .name("phone-remote".into())
        .spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if clients.load(Ordering::Relaxed) >= MAX_CLIENTS {
                            continue;
                        }
                        clients.fetch_add(1, Ordering::Relaxed);
                        let (st, cmd, token, clients) = (
                            st.clone(),
                            on_command.clone(),
                            token.clone(),
                            clients.clone(),
                        );
                        std::thread::spawn(move || {
                            let _ = serve(stream, &token, &st, &*cmd);
                            clients.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        })
        .map_err(|e| e.to_string())?;
    log::info!("phone remote on port {port}");
    Ok(Server { url, state, stop })
}

/// One request (the page closes the connection after each).
fn serve(
    stream: TcpStream,
    token: &str,
    state: &Mutex<State>,
    on_command: &dyn Fn(Command),
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?.take(8192));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    // The headers are not needed; read them so the client isn't cut off.
    let mut h = String::new();
    while reader.read_line(&mut h)? > 2 {
        h.clear();
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (status, ctype, body) = route(method, target, token, state, on_command);
    let mut out = stream;
    write!(
        out,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         Referrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    out.write_all(body.as_bytes())
}

fn route(
    method: &str,
    target: &str,
    token: &str,
    state: &Mutex<State>,
    on_command: &dyn Fn(Command),
) -> (&'static str, &'static str, String) {
    const NOT_FOUND: (&str, &str, &str) = ("404 Not Found", "text/plain", "not found");
    let not_found = || (NOT_FOUND.0, NOT_FOUND.1, NOT_FOUND.2.to_owned());
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let Some(rest) = path.strip_prefix('/').and_then(|p| p.strip_prefix(token)) else {
        return not_found();
    };
    match (method, rest) {
        ("GET", "/" | "") => ("200 OK", "text/html; charset=utf-8", PAGE.to_owned()),
        ("GET", "/state") => {
            let s = state.lock().map(|s| s.clone()).unwrap_or_default();
            ("200 OK", "application/json", state_json(&s))
        }
        ("POST", cmd) => match cmd
            .strip_prefix("/cmd/")
            .and_then(|c| parse_command(c, query))
        {
            Some(c) => {
                on_command(c);
                ("200 OK", "text/plain", "ok".to_owned())
            }
            None => not_found(),
        },
        _ => not_found(),
    }
}

fn parse_command(name: &str, query: &str) -> Option<Command> {
    let v = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("v="))
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite());
    Some(match name {
        "toggle" => Command::TogglePause,
        "next" => Command::Next,
        "prev" => Command::Prev,
        "stop" => Command::Stop,
        "mute" => Command::Mute,
        "fullscreen" => Command::Fullscreen,
        "skip" => Command::Skip,
        "seek-rel" => Command::SeekRelative(v?.clamp(-600.0, 600.0)),
        "seek" => Command::Seek(v?.max(0.0)),
        "volume" => Command::Volume(v?.clamp(0.0, 150.0)),
        _ => return None,
    })
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out += "\\\"",
            '\\' => out += "\\\\",
            '<' => out += "\\u003c",
            c if (c as u32) < 0x20 => out += &format!("\\u{:04x}", c as u32),
            c => out.push(c),
        }
    }
    out + "\""
}

fn state_json(s: &State) -> String {
    format!(
        "{{\"title\":{},\"artist\":{},\"position\":{:.1},\"duration\":{:.1},\"paused\":{},\"idle\":{},\"volume\":{:.0},\"muted\":{},\"skip\":{}}}",
        json_str(&s.title),
        json_str(&s.artist),
        s.position,
        s.duration,
        s.paused,
        s.idle,
        s.volume,
        s.muted,
        json_str(&s.skip)
    )
}

/// The QR code for `text`, as a picture `scale` pixels per module (with a
/// white border).
pub fn qr_image(text: &str, scale: u32) -> Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let w = code.width() as u32;
    let colors = code.to_colors();
    let border = 2;
    let size = (w + 2 * border) * scale;
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(size, size);
    let px = buf.make_mut_slice();
    for y in 0..size {
        for x in 0..size {
            let (mx, my) = (
                (x / scale) as i64 - border as i64,
                (y / scale) as i64 - border as i64,
            );
            let dark = mx >= 0
                && my >= 0
                && (mx as u32) < w
                && (my as u32) < w
                && colors[(my as u32 * w + mx as u32) as usize] == qrcode::Color::Dark;
            let v = if dark { 0 } else { 255 };
            px[(y * size + x) as usize] = slint::Rgba8Pixel {
                r: v,
                g: v,
                b: v,
                a: 255,
            };
        }
    }
    Some(buf)
}

const PAGE: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<meta name="theme-color" content="#07080d">
<title>SMP remote</title>
<style>
:root{--bg:#07080d;--card:#141826;--text:#eef1fa;--dim:#9aa3bd;--accent:#39d0ff;--accent2:#a46bff}
*{box-sizing:border-box;-webkit-tap-highlight-color:transparent}
body{margin:0;min-height:100vh;background:radial-gradient(circle at 50% 0,#1a1440,var(--bg) 70%);color:var(--text);
 font:16px system-ui,-apple-system,sans-serif;display:flex;flex-direction:column;align-items:center;
 padding:max(24px,env(safe-area-inset-top)) 20px max(24px,env(safe-area-inset-bottom))}
.title{font-size:22px;font-weight:800;text-align:center;margin-top:12px;word-break:break-word}
.artist{color:var(--dim);text-align:center;margin:6px 0 22px;min-height:1.2em}
.time{display:flex;justify-content:space-between;width:100%;max-width:440px;color:var(--dim);font-size:13px}
input[type=range]{width:100%;max-width:440px;accent-color:var(--accent);height:32px}
.row{display:flex;gap:14px;justify-content:center;align-items:center;margin:16px 0;flex-wrap:wrap}
button{border:0;border-radius:50%;width:64px;height:64px;background:var(--card);color:var(--text);font-size:22px}
button.big{width:88px;height:88px;font-size:34px;background:linear-gradient(135deg,var(--accent),var(--accent2));color:#07080d}
button.pill{border-radius:14px;width:auto;height:44px;padding:0 16px;font-size:15px}
button:active{transform:scale(.94)}
#skip{display:none;background:#fff;color:#10131c;font-weight:800}
.vol{display:flex;gap:10px;align-items:center;width:100%;max-width:440px}
.err{color:#ff7a8e;font-size:13px;min-height:1.2em;margin-top:10px}
</style></head><body>
<div class="title" id="title">Super Media Player</div>
<div class="artist" id="artist"></div>
<div class="time"><span id="pos">0:00</span><span id="dur">0:00</span></div>
<input type="range" id="seek" min="0" max="1" step="1" value="0">
<div class="row">
 <button onclick="cmd('prev')" aria-label="Previous">⏮</button>
 <button onclick="cmd('seek-rel',-10)" aria-label="Back 10 seconds">↺</button>
 <button class="big" id="play" onclick="cmd('toggle')" aria-label="Play or pause">▶</button>
 <button onclick="cmd('seek-rel',10)" aria-label="Forward 10 seconds">↻</button>
 <button onclick="cmd('next')" aria-label="Next">⏭</button>
</div>
<div class="row"><button class="pill" id="skip" onclick="cmd('skip')"></button></div>
<div class="vol"><button class="pill" id="mute" onclick="cmd('mute')">🔊</button>
 <input type="range" id="vol" min="0" max="150" step="1" value="100"></div>
<div class="row"><button class="pill" onclick="cmd('stop')">■ Stop</button>
 <button class="pill" onclick="cmd('fullscreen')">⛶ Fullscreen</button></div>
<div class="err" id="err"></div>
<script>
const $=id=>document.getElementById(id);let dragging=false,volDrag=false;
const fmt=t=>{t=Math.max(0,Math.floor(t));const h=Math.floor(t/3600),m=Math.floor(t/60)%60,s=t%60;
 return (h?h+':'+String(m).padStart(2,'0'):m)+':'+String(s).padStart(2,'0')};
const cmd=(c,v)=>fetch('cmd/'+c+(v!==undefined?'?v='+v:''),{method:'POST'}).then(()=>setTimeout(poll,150)).catch(()=>{});
function poll(){fetch('state').then(r=>r.json()).then(s=>{$('err').textContent='';
 $('title').textContent=s.idle?'Nothing playing':s.title;$('artist').textContent=s.artist;
 $('play').textContent=(s.paused||s.idle)?'▶':'⏸';$('dur').textContent=fmt(s.duration);
 if(!dragging){$('seek').max=Math.max(1,Math.floor(s.duration));$('seek').value=Math.floor(s.position);$('pos').textContent=fmt(s.position)}
 if(!volDrag)$('vol').value=s.volume;$('mute').textContent=s.muted?'🔇':'🔊';
 $('skip').style.display=s.skip?'block':'none';$('skip').textContent=s.skip+'  ›';
}).catch(()=>{$('err').textContent='Not connected to the player'})}
$('seek').oninput=e=>{dragging=true;$('pos').textContent=fmt(e.target.value)};
$('seek').onchange=e=>{dragging=false;cmd('seek',e.target.value)};
$('vol').oninput=e=>{volDrag=true};$('vol').onchange=e=>{volDrag=false;cmd('volume',e.target.value)};
poll();setInterval(poll,1000);
</script></body></html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_the_secret() {
        let state = Mutex::new(State {
            title: "A \"film\" <x>".into(),
            ..Default::default()
        });
        let got = Mutex::new(Vec::new());
        let cmd = |c| got.lock().unwrap().push(c);
        let r = |m, t| route(m, t, "abc123", &state, &cmd);
        assert_eq!(r("GET", "/abc123/").0, "200 OK");
        assert_eq!(r("GET", "/").0, "404 Not Found");
        assert_eq!(r("GET", "/abc12/state").0, "404 Not Found");
        assert_eq!(r("POST", "/wrong/cmd/toggle").0, "404 Not Found");
        let s = r("GET", "/abc123/state").2;
        assert!(s.contains(r#""title":"A \"film\" \u003cx>""#), "{s}");
        assert_eq!(r("POST", "/abc123/cmd/seek-rel?v=-10").0, "200 OK");
        assert_eq!(r("POST", "/abc123/cmd/volume?v=999").0, "200 OK");
        assert_eq!(r("POST", "/abc123/cmd/seek?v=NaN").0, "404 Not Found");
        assert_eq!(r("POST", "/abc123/cmd/rm-rf").0, "404 Not Found");
        assert_eq!(
            *got.lock().unwrap(),
            vec![Command::SeekRelative(-10.0), Command::Volume(150.0)]
        );
        assert_eq!(secret().len(), 32);
        assert!(
            qr_image(
                "http://192.168.1.2:47801/0123456789abcdef0123456789abcdef/",
                4
            )
            .is_some()
        );
    }

    /// Over a real socket: the page and a command.
    #[test]
    fn serves_over_http() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        let Ok(server) = start(move |c| g.lock().unwrap().push(c)) else {
            eprintln!("no network; skipping");
            return;
        };
        let path = server.url.split_once("://").unwrap().1;
        let (host, rest) = path.split_once('/').unwrap();
        let req = |method: &str, p: &str| {
            let mut s = TcpStream::connect(host).unwrap();
            write!(s, "{method} /{rest}{p} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        };
        assert!(req("GET", "").contains("SMP remote"));
        assert!(req("POST", "cmd/next").starts_with("HTTP/1.1 200"));
        assert_eq!(*got.lock().unwrap(), vec![Command::Next]);
    }
}
