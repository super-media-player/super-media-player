//! Watching together: play, pause and seeking stay in step between several
//! players, each playing its own copy of the same file.
//!
//! One player hosts: it listens on a TCP port and sends its state (file
//! name, position, paused) to everyone about once a second and whenever it
//! changes. The others join with the invite `address:port/code`; when they
//! pause or seek, the host does the same, and so everyone follows. Only the
//! file's name is sent, never the file or its path.
//!
//! The protocol is lines of text:
//!   guest → host   `HELLO <code> <name>`, then `CTRL pause|play|seek <secs>`
//!   host → guest   `WELCOME`, `STATE <0|1 paused> <secs> <file name>`,
//!                  `PEOPLE <name>|<name>…`

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const PORT: u16 = 47_800;
const MAX_GUESTS: usize = 32;

/// A change asked for by someone in the party.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Pause,
    Play,
    Seek(f64),
}

/// The host's state, as guests hear it.
#[derive(Clone, Debug, PartialEq)]
pub struct Shared {
    pub paused: bool,
    pub position: f64,
    pub file: String,
}

/// News for the UI thread.
#[derive(Debug, PartialEq)]
pub enum Event {
    /// (Host) a guest asks for this.
    Control(Control),
    /// (Guest) the host's state.
    State(Shared),
    People(Vec<String>),
    /// The connection ended (guest), with why.
    Closed(String),
}

pub struct Party {
    pub hosting: bool,
    pub invite: String,
    pub events: Receiver<Event>,
    out: Arc<Mutex<Vec<(TcpStream, String)>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Party {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut peers) = self.out.lock() {
            for (s, _) in peers.drain(..) {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// Names travel inside lines and lists: keep them short and plain.
pub fn clean_name(name: &str) -> String {
    let n: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '|')
        .take(32)
        .collect();
    let n = n.trim().to_owned();
    if n.is_empty() { "Someone".into() } else { n }
}

fn code() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    format!("{:08x}", h.finish() as u32)
}

pub fn encode_state(s: &Shared) -> String {
    let file: String = s.file.chars().filter(|c| !c.is_control()).collect();
    format!("STATE {} {:.2} {file}\n", u8::from(s.paused), s.position)
}

pub fn parse_line(line: &str) -> Option<Event> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
    match kind {
        "STATE" => {
            let mut p = rest.splitn(3, ' ');
            let paused = p.next()? == "1";
            let position: f64 = p
                .next()?
                .parse()
                .ok()
                .filter(|x: &f64| x.is_finite() && *x >= 0.0)?;
            Some(Event::State(Shared {
                paused,
                position,
                file: p.next().unwrap_or("").to_owned(),
            }))
        }
        "PEOPLE" => Some(Event::People(
            rest.split('|')
                .filter(|s| !s.is_empty())
                .map(clean_name)
                .collect(),
        )),
        "CTRL" => {
            let mut p = rest.split(' ');
            Some(Event::Control(match p.next()? {
                "pause" => Control::Pause,
                "play" => Control::Play,
                "seek" => Control::Seek(
                    p.next()?
                        .parse()
                        .ok()
                        .filter(|x: &f64| x.is_finite() && *x >= 0.0)?,
                ),
                _ => return None,
            }))
        }
        _ => None,
    }
}

fn control_line(c: &Control) -> String {
    match c {
        Control::Pause => "CTRL pause\n".into(),
        Control::Play => "CTRL play\n".into(),
        Control::Seek(t) => format!("CTRL seek {t:.2}\n"),
    }
}

impl Party {
    /// Host a party. `name` is the host's own name in the list.
    pub fn host(name: &str) -> Result<Party, String> {
        let ip = crate::remote::lan_ip().ok_or("this computer is not on a network")?;
        let listener = TcpListener::bind(SocketAddr::new([0, 0, 0, 0].into(), PORT))
            .or_else(|_| TcpListener::bind("0.0.0.0:0"))
            .map_err(|e| format!("couldn't open a port: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let code = code();
        let (tx, events) = channel();
        let out: Arc<Mutex<Vec<(TcpStream, String)>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let party = Party {
            hosting: true,
            invite: format!("{ip}:{port}/{code}"),
            events,
            out: out.clone(),
            stop: stop.clone(),
        };
        let host_name = clean_name(name);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((s, _)) => {
                        let (tx, out, code, stop, host_name) = (
                            tx.clone(),
                            out.clone(),
                            code.clone(),
                            stop.clone(),
                            host_name.clone(),
                        );
                        std::thread::spawn(move || {
                            guest_conn(s, &code, &host_name, &out, &tx, &stop)
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        });
        let _ = &party.out;
        Ok(party)
    }

    /// Join with an invite `address:port/code`.
    pub fn join(invite: &str, name: &str) -> Result<Party, String> {
        let invite = invite.trim().trim_start_matches("smp://");
        let (addr, code) = invite
            .rsplit_once('/')
            .ok_or("the invite looks like 192.168.1.20:47800/ab12cd34")?;
        let addr = if addr.contains(':') {
            addr.to_owned()
        } else {
            format!("{addr}:{PORT}")
        };
        let sock: SocketAddr = std::net::ToSocketAddrs::to_socket_addrs(&addr)
            .map_err(|e| format!("{addr}: {e}"))?
            .next()
            .ok_or("bad address")?;
        let mut s = TcpStream::connect_timeout(&sock, Duration::from_secs(5))
            .map_err(|e| format!("couldn't reach the host: {e}"))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        writeln!(s, "HELLO {} {}", code.trim(), clean_name(name)).map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(s.try_clone().map_err(|e| e.to_string())?);
        let mut first = String::new();
        reader
            .read_line(&mut first)
            .map_err(|_| "the host didn't answer".to_owned())?;
        if first.trim() != "WELCOME" {
            return Err("the host turned the invite down (wrong code?)".into());
        }
        s.set_read_timeout(None).map_err(|e| e.to_string())?;
        let (tx, events) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let out = Arc::new(Mutex::new(vec![(s, String::new())]));
        let stop2 = stop.clone();
        std::thread::spawn(move || {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        if !stop2.load(Ordering::Relaxed) {
                            let _ = tx.send(Event::Closed("The host ended the party".into()));
                        }
                        return;
                    }
                    Ok(_) => {
                        if let Some(e) = parse_line(&line) {
                            let _ = tx.send(e);
                        }
                    }
                }
            }
        });
        Ok(Party {
            hosting: false,
            invite: invite.to_owned(),
            events,
            out,
            stop,
        })
    }

    /// (Host) tell everyone the state.
    pub fn broadcast(&self, s: &Shared) {
        self.send_all(&encode_state(s));
    }

    /// (Guest) ask the host for a change.
    pub fn control(&self, c: &Control) {
        self.send_all(&control_line(c));
    }

    /// Names of the guests (host side).
    pub fn guests(&self) -> Vec<String> {
        self.out
            .lock()
            .map(|o| o.iter().map(|(_, n)| n.clone()).collect())
            .unwrap_or_default()
    }

    /// (Host) tell everyone who is watching.
    pub fn send_people(&self, names: &[String]) {
        self.send_all(&format!("PEOPLE {}\n", names.join("|")));
    }

    fn send_all(&self, line: &str) {
        if let Ok(mut peers) = self.out.lock() {
            peers.retain_mut(|(s, _)| s.write_all(line.as_bytes()).is_ok());
        }
    }
}

/// One guest's connection, on the host.
fn guest_conn(
    s: TcpStream,
    code: &str,
    _host: &str,
    out: &Mutex<Vec<(TcpStream, String)>>,
    tx: &Sender<Event>,
    stop: &AtomicBool,
) {
    let _ = s.set_nonblocking(false);
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(clone) = s.try_clone() else { return };
    let mut reader = BufReader::new(clone);
    let mut hello = String::new();
    if reader.read_line(&mut hello).is_err() {
        return;
    }
    let mut parts = hello.trim().splitn(3, ' ');
    let ok = parts.next() == Some("HELLO") && parts.next() == Some(code);
    let name = clean_name(parts.next().unwrap_or(""));
    let full = out.lock().map(|o| o.len() >= MAX_GUESTS).unwrap_or(true);
    let mut w = &s;
    if !ok || full || w.write_all(b"WELCOME\n").is_err() {
        return;
    }
    let _ = s.set_read_timeout(None);
    if let (Ok(mut o), Ok(c)) = (out.lock(), s.try_clone()) {
        o.push((c, name.clone()));
    }
    let _ = tx.send(Event::People(Vec::new()));
    let mut line = String::new();
    while !stop.load(Ordering::Relaxed) {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Some(Event::Control(c)) = parse_line(&line) {
                    let _ = tx.send(Event::Control(c));
                }
            }
        }
    }
    if let Ok(mut o) = out.lock() {
        o.retain(|(c, _)| c.peer_addr().ok() != s.peer_addr().ok());
    }
    let _ = tx.send(Event::People(Vec::new()));
}

/// What a guest should do to match the host: a pause change and/or a seek.
/// `local` is this player's position; `latency` how old the host's report is.
pub fn follow(
    host: &Shared,
    local_paused: bool,
    local: f64,
    latency: f64,
) -> (Option<bool>, Option<f64>) {
    let expected = host.position + if host.paused { 0.0 } else { latency };
    let pause = (host.paused != local_paused).then_some(host.paused);
    let seek = ((expected - local).abs() > 1.5).then_some(expected);
    (pause, seek)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        let s = Shared {
            paused: true,
            position: 61.5,
            file: "Show S01E02.mkv".into(),
        };
        assert_eq!(parse_line(&encode_state(&s)), Some(Event::State(s)));
        assert_eq!(
            parse_line("CTRL seek 12.5\n"),
            Some(Event::Control(Control::Seek(12.5)))
        );
        assert_eq!(parse_line("CTRL seek NaN"), None);
        assert_eq!(parse_line("CTRL format c:"), None);
        assert_eq!(
            parse_line("PEOPLE Ann|Bob"),
            Some(Event::People(vec!["Ann".into(), "Bob".into()]))
        );
        assert_eq!(clean_name("Evil\n|Name"), "EvilName");
        assert_eq!(
            follow(
                &Shared {
                    paused: false,
                    position: 100.0,
                    file: String::new()
                },
                false,
                100.2,
                0.1
            ),
            (None, None)
        );
        assert_eq!(
            follow(
                &Shared {
                    paused: true,
                    position: 50.0,
                    file: String::new()
                },
                false,
                100.0,
                0.1
            ),
            (Some(true), Some(50.0))
        );
    }

    /// A host and a guest on this computer: the guest hears the state, the
    /// host hears the guest's seek, a wrong code is turned down.
    #[test]
    fn host_and_guest() {
        let Ok(host) = Party::host("Ann") else {
            eprintln!("no network; skipping");
            return;
        };
        let (addr, code) = host.invite.rsplit_once('/').unwrap();
        assert!(Party::join(&format!("{addr}/wrongcode"), "Eve").is_err());
        let guest = Party::join(&host.invite, "Bob").unwrap();
        // Wait for the guest to be listed.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while host.guests().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(host.guests(), vec!["Bob".to_owned()]);
        let s = Shared {
            paused: false,
            position: 10.0,
            file: "a.mkv".into(),
        };
        host.broadcast(&s);
        assert_eq!(
            guest.events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::State(s)
        );
        guest.control(&Control::Seek(42.0));
        let got = loop {
            if let Event::Control(c) = host.events.recv_timeout(Duration::from_secs(3)).unwrap() {
                break c;
            }
        };
        assert_eq!(got, Control::Seek(42.0));
        let _ = code;
    }
}
