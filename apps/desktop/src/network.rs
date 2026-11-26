//! Network sources: DLNA / UPnP media servers on the local network, and
//! SMB / SFTP / WebDAV shares through GNOME's gvfs (`gio mount`).
//!
//! DLNA: an SSDP search finds the media servers; each server's description
//! names its ContentDirectory service, whose Browse action lists folders
//! and files (DIDL-Lite) with plain HTTP links mpv plays directly.
//!
//! Shares: `gio mount` connects (answering its user / password prompts),
//! and gvfs shows the share as a local folder under /run/user/<uid>/gvfs,
//! which the player browses and plays like any folder.

use std::io::{Read, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const AGENT: &str = concat!(
    "SuperMediaPlayer/",
    env!("CARGO_PKG_VERSION"),
    " UPnP/1.0 DLNADOC/1.50"
);

#[derive(Clone, Debug, PartialEq)]
pub struct Server {
    pub name: String,
    /// The ContentDirectory control URL.
    pub control: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// A DLNA folder: its object id.
    Folder {
        title: String,
        id: String,
        count: Option<u32>,
    },
    /// A DLNA file: its HTTP link.
    Item {
        title: String,
        url: String,
        detail: String,
    },
}

// ---- DLNA ----

/// Media servers that answer within `wait`.
pub fn discover(wait: Duration) -> Vec<Server> {
    let mut locations = Vec::new();
    ssdp(
        "urn:schemas-upnp-org:device:MediaServer:1",
        wait,
        None,
        |loc| locations.push(loc),
    );
    let mut out: Vec<Server> = Vec::new();
    for loc in locations {
        match describe(&loc) {
            Ok(s) if !out.iter().any(|o| o.control == s.control) => out.push(s),
            Ok(_) => {}
            Err(e) => log::info!("DLNA {loc}: {e}"),
        }
    }
    out
}

/// Ask for UPnP devices of type `st` on the network (or only the computer
/// `only`) and report each one's description link, once, as it answers.
pub fn ssdp(
    st: &str,
    wait: Duration,
    only: Option<std::net::Ipv4Addr>,
    mut found: impl FnMut(String),
) {
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
        return;
    };
    let _ = sock.set_multicast_ttl_v4(2);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(300)));
    let msg = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n"
    );
    let to = std::net::SocketAddr::new(
        only.unwrap_or(std::net::Ipv4Addr::new(239, 255, 255, 250))
            .into(),
        1900,
    );
    // UDP can drop a packet: ask twice.
    for _ in 0..2 {
        let _ = sock.send_to(msg.as_bytes(), to);
    }
    let mut seen: Vec<String> = Vec::new();
    let end = Instant::now() + wait;
    let mut buf = [0u8; 4096];
    while Instant::now() < end {
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            let text = String::from_utf8_lossy(&buf[..n]);
            // Some devices answer every search, whatever it asks for.
            let right = header(&text, "st").is_none_or(|t| {
                let no_version =
                    |s: &str| s.trim_end_matches(|c: char| c.is_ascii_digit()).to_owned();
                no_version(&t) == no_version(st)
            });
            if let Some(loc) = header(&text, "location").filter(|l| right && !seen.contains(l)) {
                seen.push(loc.clone());
                found(loc);
            }
        }
    }
}

pub fn header(response: &str, name: &str) -> Option<String> {
    response.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_owned())
    })
}

pub fn get_text(url: &str) -> Result<String, String> {
    let mut resp = ureq::get(url)
        .header("User-Agent", AGENT)
        .config()
        .timeout_global(Some(Duration::from_secs(8)))
        .build()
        .call()
        .map_err(|e| e.to_string())?;
    resp.body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())
}

fn describe(location: &str) -> Result<Server, String> {
    let xml = get_text(location)?;
    parse_description(&xml, location).ok_or_else(|| "no ContentDirectory service".into())
}

/// Resolve `href` against `base` ("http://host:port/desc.xml").
pub fn join_url(base: &str, href: &str) -> String {
    if href.contains("://") {
        return href.to_owned();
    }
    let Some((scheme, rest)) = base.split_once("://") else {
        return href.to_owned();
    };
    let host = rest.split('/').next().unwrap_or(rest);
    if let Some(abs) = href.strip_prefix('/') {
        return format!("{scheme}://{host}/{abs}");
    }
    let dir = base.rsplit_once('/').map_or(base, |x| x.0);
    format!("{dir}/{href}")
}

pub fn parse_description(xml: &str, location: &str) -> Option<Server> {
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
    let name = text(root, "friendlyName").unwrap_or_else(|| "Media server".into());
    let service = root.descendants().find(|n| {
        n.has_tag_name("service")
            && n.children().any(|c| {
                c.tag_name().name() == "serviceType"
                    && c.text().is_some_and(|t| t.contains("ContentDirectory"))
            })
    })?;
    let control = text(service, "controlURL")?;
    Some(Server {
        name,
        control: join_url(&base, &control),
    })
}

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The children of folder `id` ("0" is the top).
pub fn browse(control: &str, id: &str) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    let mut start = 0u32;
    loop {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
             <s:Body><u:Browse xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\">\
             <ObjectID>{}</ObjectID><BrowseFlag>BrowseDirectChildren</BrowseFlag><Filter>*</Filter>\
             <StartingIndex>{start}</StartingIndex><RequestedCount>200</RequestedCount><SortCriteria></SortCriteria>\
             </u:Browse></s:Body></s:Envelope>",
            xml_escape(id)
        );
        let mut resp = ureq::post(control)
            .header("User-Agent", AGENT)
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header(
                "SOAPAction",
                "\"urn:schemas-upnp-org:service:ContentDirectory:1#Browse\"",
            )
            .config()
            .timeout_global(Some(Duration::from_secs(15)))
            .build()
            .send(body.as_bytes())
            .map_err(|e| format!("The media server didn't answer ({e})"))?;
        let xml = resp
            .body_mut()
            .with_config()
            .limit(32 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| e.to_string())?;
        let (page, total) = parse_browse(&xml)?;
        let got = page.len() as u32;
        out.extend(page);
        start += got;
        if got == 0 || start >= total || start > 20_000 {
            break;
        }
    }
    Ok(out)
}

/// A Browse answer: the entries, and how many there are in all.
pub fn parse_browse(xml: &str) -> Result<(Vec<Entry>, u32), String> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| format!("The media server's answer isn't valid ({e})"))?;
    let field = |name: &str| {
        doc.descendants()
            .find(|n| n.tag_name().name() == name)
            .and_then(|n| n.text())
    };
    let didl = field("Result").unwrap_or("");
    let total = field("TotalMatches")
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0);
    if didl.trim().is_empty() {
        return Ok((Vec::new(), total));
    }
    Ok((parse_didl(didl)?, total))
}

pub fn parse_didl(didl: &str) -> Result<Vec<Entry>, String> {
    let doc = roxmltree::Document::parse(didl)
        .map_err(|e| format!("The media server's list isn't valid ({e})"))?;
    let child = |n: roxmltree::Node, name: &str| {
        n.children()
            .find(|c| c.tag_name().name() == name)
            .and_then(|c| c.text())
            .map(|t| t.trim().to_owned())
    };
    let mut out = Vec::new();
    for n in doc.root_element().children().filter(|n| n.is_element()) {
        let title = child(n, "title").unwrap_or_else(|| "(untitled)".into());
        match n.tag_name().name() {
            "container" => out.push(Entry::Folder {
                title,
                id: n.attribute("id").unwrap_or("").to_owned(),
                count: n.attribute("childCount").and_then(|c| c.parse().ok()),
            }),
            "item" => {
                let class = child(n, "class").unwrap_or_default();
                if class.contains("imageItem") {
                    continue;
                }
                // The first link mpv can fetch.
                let Some(res) = n
                    .children()
                    .filter(|c| c.tag_name().name() == "res")
                    .find(|r| {
                        r.attribute("protocolInfo")
                            .is_none_or(|p| p.starts_with("http-get"))
                            && r.text().is_some_and(|t| t.trim().starts_with("http"))
                    })
                else {
                    continue;
                };
                let mut d = Vec::new();
                if let Some(a) = child(n, "artist").or_else(|| child(n, "creator")) {
                    d.push(a);
                }
                if let Some(dur) = res.attribute("duration") {
                    d.push(
                        dur.split('.')
                            .next()
                            .unwrap_or(dur)
                            .trim_start_matches("0:")
                            .to_owned(),
                    );
                }
                if let Some(r) = res.attribute("resolution") {
                    d.push(r.to_owned());
                }
                out.push(Entry::Item {
                    title,
                    url: res.text().unwrap_or("").trim().to_owned(),
                    detail: d.join(" · "),
                });
            }
            _ => {}
        }
    }
    Ok(out)
}

// ---- shares through gvfs ----

/// "smb://nas/music" style addresses gvfs can mount.
pub fn is_share(uri: &str) -> bool {
    [
        "smb://", "sftp://", "ssh://", "dav://", "davs://", "ftp://", "ftps://", "afp://", "nfs://",
    ]
    .iter()
    .any(|s| uri.to_lowercase().starts_with(s))
}

/// `gio`, without the GIO module folder a snap (VS Code's, say) sets for
/// the programs it starts: that folder lacks gvfs's client module, and
/// every share then fails with "Operation not supported".
fn gio() -> Command {
    let mut c = Command::new("gio");
    if std::env::var_os("SNAP").is_some()
        || std::env::var("GIO_MODULE_DIR").is_ok_and(|d| d.contains("/snap/"))
    {
        c.env_remove("GIO_MODULE_DIR");
    }
    c
}

/// Mount `uri` (answering gio's prompts with `user` / `password`), then
/// return the local folder gvfs shows it as.
pub fn mount(uri: &str, user: &str, password: &str) -> Result<PathBuf, String> {
    if let Some(p) = local_path(uri) {
        return Ok(p);
    }
    let mut child = gio()
        .args(["mount", uri])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't run gio (GNOME's gvfs tools) to connect: {e}"))?;
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().ok_or("no gio output")?;
    let (user, password) = (user.to_owned(), password.to_owned());
    // Read gio's prompts as they come and answer each one.
    let reader = std::thread::spawn(move || {
        let mut seen = String::new();
        let mut asked_password = 0;
        let mut b = [0u8; 256];
        while let Ok(n) = stdout.read(&mut b) {
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&b[..n]));
            let last = seen.rsplit('\n').next().unwrap_or("").to_lowercase();
            if !last.trim_end().ends_with(':') {
                continue;
            }
            let answer = if last.contains("password") {
                asked_password += 1;
                // Asked again: the password was wrong; stop rather than loop.
                if asked_password > 1 {
                    break;
                }
                password.clone()
            } else if last.contains("user") {
                user.clone()
            } else if last.contains("domain") {
                String::new()
            } else if last.contains("choice") || last.contains("log in anyway") {
                // An SFTP server seen for the first time: like "Log In
                // Anyway" in the Files app.
                "1".into()
            } else {
                continue;
            };
            if let Some(s) = stdin.as_mut() {
                let _ = writeln!(s, "{answer}");
            }
            seen.clear();
        }
        drop(stdin);
        seen
    });
    let deadline = Instant::now() + Duration::from_secs(45);
    let status = loop {
        if let Ok(Some(s)) = child.try_wait() {
            break Some(s);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let out = reader.join().unwrap_or_default();
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    match status {
        Some(s) if s.success() => {}
        Some(_) => {
            let msg = err
                .lines()
                .chain(out.lines())
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            let hint = if msg.contains("implement mount") || msg.contains("not supported") {
                " (GNOME's network shares need the gvfs-backends package: sudo apt install gvfs-backends)"
            } else {
                ""
            };
            return Err(format!(
                "Couldn't connect to {uri}: {}{hint}",
                if msg.is_empty() {
                    "check the address, user and password"
                } else {
                    msg
                }
            ));
        }
        None => return Err(format!("Connecting to {uri} took too long")),
    }
    local_path(uri).ok_or_else(|| format!("Connected to {uri}, but gvfs shows no folder for it"))
}

/// The folder gvfs shows a mounted share as.
pub fn local_path(uri: &str) -> Option<PathBuf> {
    let out = gio().args(["info", uri]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| l.strip_prefix("local path: "))
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

/// Folders and media files in `dir`, folders first, natural order.
pub fn list_dir(dir: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("Couldn't read {}: {e}", dir.display()))?;
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for e in rd.flatten() {
        let p = e.path();
        let hidden = p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if hidden {
            continue;
        }
        if p.is_dir() {
            dirs.push(p);
        } else if crate::is_folder_media(&p) {
            files.push(p);
        }
    }
    let key = |p: &PathBuf| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    dirs.sort_by(|a, b| crate::natural_cmp(&key(a), &key(b)));
    files.sort_by(|a, b| crate::natural_cmp(&key(a), &key(b)));
    Ok((dirs, files))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against a media server on the network (scratch fake_dlna.py, or a
    /// real one): cargo test -- --ignored live_dlna
    #[test]
    #[ignore]
    fn live_dlna() {
        let servers = discover(Duration::from_secs(3));
        println!("{servers:?}");
        let s = servers.first().expect("no media server answered");
        let top = browse(&s.control, "0").unwrap();
        println!("top: {top:?}");
        let Entry::Folder { id, .. } = &top[0] else {
            panic!("no folder")
        };
        let inside = browse(&s.control, id).unwrap();
        println!("inside: {inside:?}");
        assert!(matches!(&inside[0], Entry::Item { url, .. } if url.starts_with("http")));
    }

    /// gio mount's failure comes back as a message: cargo test -- --ignored mount_failure
    #[test]
    #[ignore]
    fn mount_failure_is_reported() {
        let e = mount("smb://192.0.2.1/nothing", "", "").unwrap_err();
        println!("{e}");
        assert!(
            e.starts_with("Couldn't connect") || e.contains("too long"),
            "{e}"
        );
    }

    #[test]
    fn ssdp_headers_and_urls() {
        let r = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nLocation: http://192.168.1.5:8200/rootDesc.xml\r\n\r\n";
        assert_eq!(
            header(r, "location").as_deref(),
            Some("http://192.168.1.5:8200/rootDesc.xml")
        );
        assert_eq!(
            join_url("http://h:8200/rootDesc.xml", "/ctl/ContentDir"),
            "http://h:8200/ctl/ContentDir"
        );
        assert_eq!(
            join_url("http://h:8200/dev/desc.xml", "ctl"),
            "http://h:8200/dev/ctl"
        );
        assert_eq!(join_url("http://h/x.xml", "http://o/c"), "http://o/c");
    }

    #[test]
    fn description_names_the_content_directory() {
        let xml = r#"<?xml version="1.0"?><root xmlns="urn:schemas-upnp-org:device-1-0"><device>
            <friendlyName>NAS: minidlna</friendlyName><serviceList>
            <service><serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType><controlURL>/ctl/CM</controlURL></service>
            <service><serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType><controlURL>/ctl/ContentDir</controlURL></service>
            </serviceList></device></root>"#;
        let s = parse_description(xml, "http://192.168.1.5:8200/rootDesc.xml").unwrap();
        assert_eq!(s.name, "NAS: minidlna");
        assert_eq!(s.control, "http://192.168.1.5:8200/ctl/ContentDir");
    }

    #[test]
    fn browse_answer_lists_folders_and_files() {
        let didl = r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/">
            <container id="64$1" parentID="64" childCount="12"><dc:title>Albums</dc:title><upnp:class>object.container.storageFolder</upnp:class></container>
            <item id="64$2"><dc:title>Song &amp; Dance</dc:title><upnp:artist>Band</upnp:artist><upnp:class>object.item.audioItem.musicTrack</upnp:class>
              <res protocolInfo="http-get:*:audio/mpeg:*" duration="0:03:25.000">http://192.168.1.5:8200/MediaItems/22.mp3</res></item>
            <item id="64$3"><dc:title>Photo</dc:title><upnp:class>object.item.imageItem.photo</upnp:class><res protocolInfo="http-get:*:image/jpeg:*">http://x/1.jpg</res></item>
            </DIDL-Lite>"#;
        let escaped = didl
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let soap = format!(
            r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>
               <u:BrowseResponse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"><Result>{escaped}</Result>
               <NumberReturned>3</NumberReturned><TotalMatches>3</TotalMatches></u:BrowseResponse></s:Body></s:Envelope>"#
        );
        let (entries, total) = parse_browse(&soap).unwrap();
        assert_eq!(total, 3);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0],
            Entry::Folder {
                title: "Albums".into(),
                id: "64$1".into(),
                count: Some(12)
            }
        );
        match &entries[1] {
            Entry::Item { title, url, detail } => {
                assert_eq!(title, "Song & Dance");
                assert!(url.ends_with("22.mp3"));
                assert_eq!(detail, "Band · 03:25");
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn share_addresses() {
        assert!(
            is_share("smb://nas/music")
                && is_share("SFTP://me@host/x")
                && is_share("davs://cloud/dav")
        );
        assert!(!is_share("https://x/y") && !is_share("/home"));
    }
}
