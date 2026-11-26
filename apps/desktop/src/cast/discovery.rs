//! Finding the TVs and speakers on the local network: Chromecasts answer
//! mDNS (`_googlecast._tcp.local`), DLNA renderers answer SSDP and describe
//! themselves in an XML file.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use crate::network;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Chromecast,
    Dlna,
}

/// Where a DLNA renderer takes its commands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DlnaUrls {
    pub transport: String,
    /// The AVTransport service's type ("…:1"), as it must be named in calls.
    pub transport_type: String,
    pub rendering: Option<String>,
    pub rendering_type: String,
    /// Describes RenderingControl (its volume range).
    pub rendering_scpd: Option<String>,
    pub connection: Option<String>,
    pub connection_type: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub kind: Kind,
    pub name: String,
    pub model: String,
    /// Identifies it across searches.
    pub id: String,
    pub addr: SocketAddr,
    /// Has a screen (a speaker only gets the sound).
    pub video: bool,
    pub dlna: DlnaUrls,
}

impl Device {
    /// "Chromecast · Living-room model".
    pub fn detail(&self) -> String {
        let kind = match self.kind {
            Kind::Chromecast if self.video => "Chromecast",
            Kind::Chromecast => "Chromecast, sound only",
            Kind::Dlna => "DLNA",
        };
        if self.model.is_empty() || self.model == kind {
            kind.to_owned()
        } else {
            format!("{kind} · {}", self.model)
        }
    }
}

/// Look for devices for `wait`, reporting each one as it is found (from a
/// worker thread). Blocks.
pub fn search(wait: Duration, found: impl Fn(Device) + Send + Sync) {
    std::thread::scope(|s| {
        s.spawn(|| chromecasts(wait, &found));
        s.spawn(|| renderers(wait, None, &found));
    });
}

/// The device at an address typed by the user: "192.168.1.30",
/// "192.168.1.30:8009" or the address of a DLNA description file. Blocks.
pub fn at_address(text: &str) -> Result<Device, String> {
    let text = text.trim();
    if text.starts_with("http://") {
        return describe(text);
    }
    let (host, port) = match text.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (
            h,
            p.parse::<u16>().map_err(|_| "That isn't a valid address")?,
        ),
        _ => (text, 8009),
    };
    let addr = std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
        .ok()
        .and_then(|mut a| a.find(SocketAddr::is_ipv4))
        .ok_or("That isn't a valid address")?;
    if TcpStream::connect_timeout(&addr, Duration::from_millis(2000)).is_ok() {
        return Ok(Device {
            kind: Kind::Chromecast,
            name: format!("Chromecast at {}", addr.ip()),
            model: String::new(),
            id: addr.to_string(),
            addr,
            video: true,
            dlna: DlnaUrls::default(),
        });
    }
    let hit = std::sync::Mutex::new(None);
    if let IpAddr::V4(ip) = addr.ip() {
        renderers(Duration::from_millis(2500), Some(ip), &|d| {
            if let Ok(mut h) = hit.lock() {
                h.get_or_insert(d);
            }
        });
    }
    hit.into_inner()
        .ok()
        .flatten()
        .ok_or_else(|| format!("Nothing that can play was found at {host}"))
}

// ---- DLNA ----

fn renderers(wait: Duration, only: Option<Ipv4Addr>, found: &(impl Fn(Device) + Sync)) {
    let seen = std::sync::Mutex::new(Vec::<String>::new());
    std::thread::scope(|s| {
        network::ssdp(
            "urn:schemas-upnp-org:device:MediaRenderer:1",
            wait,
            only,
            |loc| {
                let seen = &seen;
                s.spawn(move || match describe(&loc) {
                    Ok(d) => {
                        let new = seen.lock().is_ok_and(|mut s| {
                            let new = !s.contains(&d.id);
                            s.push(d.id.clone());
                            new
                        });
                        if new {
                            found(d);
                        }
                    }
                    Err(e) => log::info!("renderer {loc}: {e}"),
                });
            },
        );
    });
}

fn describe(location: &str) -> Result<Device, String> {
    let xml = network::get_text(location)?;
    parse_renderer(&xml, location)
        .ok_or_else(|| "it can't be told to play (no AVTransport service)".into())
}

/// The host and port of an http link.
pub fn host_of(url: &str) -> Option<SocketAddr> {
    let host = url.split_once("://")?.1.split('/').next()?;
    let with_port = if host.contains(':') {
        host.to_owned()
    } else {
        format!("{host}:80")
    };
    std::net::ToSocketAddrs::to_socket_addrs(&with_port)
        .ok()?
        .find(SocketAddr::is_ipv4)
}

pub fn parse_renderer(xml: &str, location: &str) -> Option<Device> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let text = |n: roxmltree::Node, name: &str| {
        n.descendants()
            .find(|c| c.tag_name().name() == name)
            .and_then(|c| c.text())
            .map(|t| t.trim().to_owned())
    };
    let root = doc.root_element();
    let base = text(root, "URLBase")
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| location.to_owned());
    // The renderer may be one of several devices in the file.
    let device = root
        .descendants()
        .filter(|n| n.has_tag_name("device") || n.tag_name().name() == "device")
        .find(|n| {
            n.children().any(|c| {
                c.tag_name().name() == "deviceType"
                    && c.text().is_some_and(|t| t.contains("MediaRenderer"))
            })
        })
        .unwrap_or(root);
    let service = |kind: &str| {
        let n = device.descendants().find(|n| {
            n.tag_name().name() == "service"
                && n.children().any(|c| {
                    c.tag_name().name() == "serviceType"
                        && c.text().is_some_and(|t| t.contains(kind))
                })
        })?;
        Some((
            network::join_url(&base, &text(n, "controlURL")?),
            text(n, "serviceType")?,
            text(n, "SCPDURL").map(|u| network::join_url(&base, &u)),
        ))
    };
    let (transport, transport_type, _) = service(":AVTransport:")?;
    let rendering = service(":RenderingControl:");
    let connection = service(":ConnectionManager:");
    let name = text(device, "friendlyName").unwrap_or_else(|| "Media renderer".into());
    let maker = text(device, "manufacturer").unwrap_or_default();
    let model = text(device, "modelName").unwrap_or_default();
    let model = if maker.is_empty() || model.to_lowercase().starts_with(&maker.to_lowercase()) {
        model
    } else {
        format!("{maker} {model}")
    };
    let addr = host_of(&transport)?;
    Some(Device {
        kind: Kind::Dlna,
        name,
        model: model.trim().to_owned(),
        id: text(device, "UDN").unwrap_or_else(|| transport.clone()),
        addr,
        video: true,
        dlna: DlnaUrls {
            transport,
            transport_type,
            rendering_scpd: rendering.as_ref().and_then(|r| r.2.clone()),
            rendering_type: rendering.as_ref().map(|r| r.1.clone()).unwrap_or_default(),
            rendering: rendering.map(|r| r.0),
            connection_type: connection.as_ref().map(|c| c.1.clone()).unwrap_or_default(),
            connection: connection.map(|c| c.0),
        },
    })
}

// ---- Chromecast (mDNS) ----

const SERVICE: &str = "_googlecast._tcp.local";
const MDNS: &str = "224.0.0.251:5353";
const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;

/// "Who offers `SERVICE`?", asking for the answer straight back to us.
fn query() -> Vec<u8> {
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in SERVICE.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&TYPE_PTR.to_be_bytes());
    q.extend_from_slice(&0x8001u16.to_be_bytes());
    q
}

/// What mDNS answers have said so far.
#[derive(Default)]
pub struct Answers {
    instances: Vec<String>,
    srv: HashMap<String, (u16, String)>,
    txt: HashMap<String, HashMap<String, String>>,
    a: HashMap<String, Ipv4Addr>,
    /// Who sent an instance's records (when there is no A record).
    from: HashMap<String, Ipv4Addr>,
}

fn u16_at(p: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*p.get(i)?, *p.get(i + 1)?]))
}

/// The name at `i` (which may point back into the packet) and what follows.
fn name_at(p: &[u8], mut i: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut after = None;
    // Each pointer must go back, so this ends; the count is for safety.
    for _ in 0..64 {
        let n = *p.get(i)? as usize;
        if n == 0 {
            return Some((labels.join("."), after.unwrap_or(i + 1)));
        }
        if n & 0xC0 == 0xC0 {
            let to = (n & 0x3F) << 8 | *p.get(i + 1)? as usize;
            after.get_or_insert(i + 2);
            if to >= i {
                return None;
            }
            i = to;
            continue;
        }
        labels.push(String::from_utf8_lossy(p.get(i + 1..i + 1 + n)?).into_owned());
        i += 1 + n;
    }
    None
}

impl Answers {
    /// Take in one packet. `None`: not a valid one.
    pub fn add(&mut self, p: &[u8], sender: Ipv4Addr) -> Option<()> {
        let questions = u16_at(p, 4)?;
        let records =
            u32::from(u16_at(p, 6)?) + u32::from(u16_at(p, 8)?) + u32::from(u16_at(p, 10)?);
        let mut i = 12;
        for _ in 0..questions {
            i = name_at(p, i)?.1 + 4;
        }
        for _ in 0..records {
            let (name, at) = name_at(p, i)?;
            let key = name.to_lowercase();
            let kind = u16_at(p, at)?;
            let len = u16_at(p, at + 8)? as usize;
            let data = at + 10;
            let bytes = p.get(data..data + len)?;
            match kind {
                TYPE_PTR if key == SERVICE => {
                    let inst = name_at(p, data)?.0.to_lowercase();
                    if !self.instances.contains(&inst) {
                        self.instances.push(inst);
                    }
                }
                TYPE_SRV => {
                    let port = u16_at(p, data + 4)?;
                    let target = name_at(p, data + 6)?.0.to_lowercase();
                    self.srv.insert(key.clone(), (port, target));
                    self.from.insert(key, sender);
                }
                TYPE_TXT => {
                    let mut map = HashMap::new();
                    let mut j = 0;
                    while j < bytes.len() {
                        let n = bytes[j] as usize;
                        let Some(s) = bytes.get(j + 1..j + 1 + n) else {
                            break;
                        };
                        if let Some((k, v)) = String::from_utf8_lossy(s).split_once('=') {
                            map.insert(k.to_lowercase(), v.to_owned());
                        }
                        j += 1 + n;
                    }
                    self.txt.insert(key, map);
                }
                TYPE_A if len == 4 => {
                    self.a
                        .insert(key, Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]));
                }
                _ => {}
            }
            i = data + len;
        }
        Some(())
    }

    /// The devices known in full.
    pub fn devices(&self) -> Vec<Device> {
        self.instances
            .iter()
            .filter_map(|inst| {
                let (port, target) = self.srv.get(inst)?;
                let txt = self.txt.get(inst)?;
                let ip = self.a.get(target).or_else(|| self.from.get(inst))?;
                // "ca" bit 0: shows video.
                let caps: u32 = txt.get("ca").and_then(|c| c.parse().ok()).unwrap_or(1);
                let addr = SocketAddr::new((*ip).into(), *port);
                Some(Device {
                    kind: Kind::Chromecast,
                    name: txt
                        .get("fn")
                        .cloned()
                        .unwrap_or_else(|| format!("Chromecast at {ip}")),
                    model: txt.get("md").cloned().unwrap_or_default(),
                    id: txt.get("id").cloned().unwrap_or_else(|| addr.to_string()),
                    addr,
                    video: caps & 1 == 1,
                    dlna: DlnaUrls::default(),
                })
            })
            .collect()
    }
}

fn chromecasts(wait: Duration, found: &(impl Fn(Device) + Sync)) {
    // Bound to our own address, the packet leaves through that network
    // (and not through a VPN or a container bridge).
    let local = crate::remote::lan_ip()
        .filter(IpAddr::is_ipv4)
        .unwrap_or(Ipv4Addr::UNSPECIFIED.into());
    let Ok(sock) = UdpSocket::bind((local, 0)).or_else(|_| UdpSocket::bind("0.0.0.0:0")) else {
        return;
    };
    let _ = sock.set_multicast_ttl_v4(255);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
    let mut answers = Answers::default();
    let mut told: Vec<String> = Vec::new();
    let start = Instant::now();
    let mut asked = 0;
    let mut buf = [0u8; 9000];
    while start.elapsed() < wait {
        // UDP can drop a packet, and devices that sleep answer late.
        if asked < 3 && start.elapsed() >= Duration::from_millis(900) * asked {
            if let Err(e) = sock.send_to(&query(), MDNS) {
                log::info!("mDNS: {e}");
            }
            asked += 1;
        }
        let Ok((n, from)) = sock.recv_from(&mut buf) else {
            continue;
        };
        let IpAddr::V4(ip) = from.ip() else { continue };
        answers.add(&buf[..n], ip);
        for d in answers.devices() {
            if !told.contains(&d.id) {
                told.push(d.id.clone());
                found(d);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On this network: cargo test -- --ignored live_cast_search
    #[test]
    #[ignore]
    fn live_cast_search() {
        search(Duration::from_secs(4), |d| println!("{d:?}"));
    }

    fn label(s: &str) -> Vec<u8> {
        let mut v = vec![s.len() as u8];
        v.extend_from_slice(s.as_bytes());
        v
    }

    #[test]
    fn chromecast_answer() {
        // One PTR, with SRV, TXT and A as additional records, names
        // compressed the way devices do it.
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 3];
        for l in SERVICE.split('.') {
            p.extend(label(l));
        }
        p.push(0); // service name at 12, 24 bytes: "local" label at 30
        let record = |p: &mut Vec<u8>, kind: u16, data: &[u8]| {
            p.extend(kind.to_be_bytes());
            p.extend([0x80, 1, 0, 0, 0, 120]);
            p.extend((data.len() as u16).to_be_bytes());
            p.extend(data);
        };
        let mut inst = label("Chromecast-abc");
        inst.extend([0xC0, 12]);
        let inst_at = p.len() + 10;
        record(&mut p, TYPE_PTR, &inst);
        let inst_ptr = [0xC0, inst_at as u8];
        p.extend(inst_ptr);
        let mut srv = vec![0, 0, 0, 0, 0x1F, 0x49];
        let host_at = p.len() + 10 + 6;
        srv.extend(label("abc-host"));
        srv.push(0);
        record(&mut p, TYPE_SRV, &srv);
        p.extend(inst_ptr);
        let mut txt = label("id=abc");
        txt.extend(label("fn=Salon – TV"));
        txt.extend(label("md=Chromecast Ultra"));
        txt.extend(label("ca=4101"));
        record(&mut p, TYPE_TXT, &txt);
        p.extend([0xC0, host_at as u8]);
        record(&mut p, TYPE_A, &[192, 168, 1, 30]);

        let mut a = Answers::default();
        a.add(&p, Ipv4Addr::new(192, 168, 1, 99)).unwrap();
        let d = a.devices();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "Salon – TV");
        assert_eq!(d[0].detail(), "Chromecast · Chromecast Ultra");
        assert_eq!(d[0].addr.to_string(), "192.168.1.30:8009");
        assert!(d[0].video);
        // Cut short anywhere, it must not crash.
        for n in 0..p.len() {
            let _ = Answers::default().add(&p[..n], Ipv4Addr::LOCALHOST);
        }
        // A name that points at itself.
        let mut bad = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xC0, 12, 0, 0, 0, 0];
        assert_eq!(Answers::default().add(&bad, Ipv4Addr::LOCALHOST), None);
        bad[13] = 14;
        assert_eq!(Answers::default().add(&bad, Ipv4Addr::LOCALHOST), None);
    }

    #[test]
    fn renderer_description() {
        let xml = r#"<?xml version="1.0"?><root xmlns="urn:schemas-upnp-org:device-1-0"><device>
            <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
            <friendlyName>[TV] Living room</friendlyName><manufacturer>Samsung</manufacturer><modelName>UE55</modelName>
            <UDN>uuid:1234</UDN><serviceList>
            <service><serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType><controlURL>/rc/control</controlURL><SCPDURL>/rc.xml</SCPDURL></service>
            <service><serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType><controlURL>/cm/control</controlURL></service>
            <service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>/av/control</controlURL></service>
            </serviceList></device></root>"#;
        let d = parse_renderer(xml, "http://192.168.1.40:9197/dmr").unwrap();
        assert_eq!(d.name, "[TV] Living room");
        assert_eq!(d.detail(), "DLNA · Samsung UE55");
        assert_eq!(d.dlna.transport, "http://192.168.1.40:9197/av/control");
        assert_eq!(
            d.dlna.rendering.as_deref(),
            Some("http://192.168.1.40:9197/rc/control")
        );
        assert_eq!(d.addr.to_string(), "192.168.1.40:9197");
        // A media server isn't a renderer.
        assert_eq!(
            parse_renderer(
                &xml.replace("AVTransport", "ContentDirectory"),
                "http://h/x"
            ),
            None
        );
    }
}
