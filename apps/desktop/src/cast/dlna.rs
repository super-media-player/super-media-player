//! DLNA / UPnP renderers (TVs, receivers, network speakers): SOAP calls to
//! their AVTransport and RenderingControl services. The renderer fetches the
//! media from our server by itself; its state is asked for once a second.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use super::discovery::Device;
use super::session::{Cmd, Media, Phase, Shared, update};
use crate::network;
use crate::network::xml_escape;

/// Failed status calls in a row before the renderer counts as gone.
const MAX_MISSES: u32 = 6;

struct Renderer<'a> {
    d: &'a Device,
    agent: ureq::Agent,
    max_volume: f64,
}

pub fn run(d: &Device, rx: &Receiver<Cmd>, status: &Shared) -> Result<(), String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut r = Renderer {
        d,
        agent,
        max_volume: 100.0,
    };
    // Is it there?
    r.transport_state()
        .map_err(|e| format!("{} didn't answer ({e})", d.name))?;
    let sink = r.sink();
    if let Some(u) = &d.dlna.rendering_scpd
        && let Some(m) = network::get_text(u).ok().and_then(|x| volume_max(&x))
    {
        r.max_volume = m;
    }
    update(status, |s| {
        s.sink = Some(sink);
        s.phase = Phase::Ready;
    });
    let mut loaded: Option<Media> = None;
    let mut seen_playing = false;
    let mut start_at: Option<f64> = None;
    let mut since_load = Instant::now();
    let mut misses = 0;
    let mut last_poll: Option<Instant> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Cmd::Load(m)) => {
                update(status, |s| {
                    s.phase = Phase::Loading;
                    s.id = m.id;
                    s.position = m.start;
                    s.at = Instant::now();
                });
                // Several TVs refuse a new link while one plays.
                if loaded.is_some() {
                    let _ = r.av("Stop", "");
                }
                seen_playing = false;
                start_at = (m.start > 1.0).then_some(m.start);
                since_load = Instant::now();
                match r.open(&m) {
                    Ok(()) => loaded = Some(*m),
                    Err(e) => {
                        loaded = None;
                        update(status, |s| s.phase = Phase::Failed(e));
                    }
                }
            }
            Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => {
                if loaded.is_some() {
                    let _ = r.av("Stop", "");
                }
                return Ok(());
            }
            Ok(cmd) => {
                if matches!(cmd, Cmd::Play)
                    && let Some(m) = &mut loaded
                {
                    m.paused = false;
                }
                let result = match cmd {
                    Cmd::Play => r.av("Play", "<Speed>1</Speed>"),
                    Cmd::Pause => r.av("Pause", ""),
                    Cmd::Seek(t) => {
                        let done = r.seek(t);
                        if done.is_ok() {
                            update(status, |s| {
                                s.position = t;
                                s.at = Instant::now();
                            });
                        }
                        done
                    }
                    Cmd::Volume(v) => r.rc(
                        "SetVolume",
                        &format!(
                            "<Channel>Master</Channel><DesiredVolume>{:.0}</DesiredVolume>",
                            v * r.max_volume
                        ),
                    ),
                    Cmd::Mute(m) => r.rc(
                        "SetMute",
                        &format!(
                            "<Channel>Master</Channel><DesiredMute>{}</DesiredMute>",
                            u8::from(m)
                        ),
                    ),
                    Cmd::Load(_) | Cmd::Quit => Ok(String::new()),
                };
                if let Err(e) = result {
                    log::info!("{}: {e}", d.name);
                    update(status, |s| s.note = Some(format!("{}: {e}", d.name)));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if last_poll.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            continue;
        }
        last_poll = Some(Instant::now());
        let (volume, muted) = r.volume();
        update(status, |s| {
            s.volume = volume.or(s.volume);
            s.muted = muted.or(s.muted);
        });
        let Some(m) = &loaded else { continue };
        let (state, transport_status) = match r.transport_state() {
            Ok(x) => {
                misses = 0;
                x
            }
            Err(e) => {
                misses += 1;
                if misses >= MAX_MISSES {
                    return Err(format!("{} stopped answering ({e})", d.name));
                }
                continue;
            }
        };
        let position = r.position();
        let phase = match state.as_str() {
            "PLAYING" => Phase::Playing,
            "PAUSED_PLAYBACK" | "PAUSED_RECORDING" => Phase::Paused,
            "TRANSITIONING" => Phase::Buffering,
            _ if transport_status == "ERROR_OCCURRED"
                && since_load.elapsed() > Duration::from_secs(2) =>
            {
                Phase::Failed("it couldn't play this".into())
            }
            "STOPPED" | "NO_MEDIA_PRESENT" if seen_playing => Phase::Stopped,
            // Loaded, not told to play yet.
            "STOPPED" if m.paused => Phase::Paused,
            // It accepts the link, then never starts: not its format.
            "STOPPED" | "NO_MEDIA_PRESENT" if since_load.elapsed() > Duration::from_secs(15) => {
                Phase::Failed("it didn't start playing".into())
            }
            _ => Phase::Loading,
        };
        match phase {
            Phase::Playing => {
                seen_playing = true;
                if let Some(t) = start_at.take() {
                    if let Err(e) = r.seek(t) {
                        update(status, |s| s.note = Some(format!("{}: {e}", d.name)));
                    }
                    if m.paused {
                        let _ = r.av("Pause", "");
                    }
                    continue;
                }
            }
            Phase::Stopped | Phase::Failed(_) => {
                loaded = None;
                update(status, |s| s.phase = phase);
                continue;
            }
            _ => {}
        }
        update(status, |s| {
            s.phase = phase;
            // A renderer that can't say keeps the clock running here.
            match position {
                Some(t) if t > 0.0 || s.position < 2.0 => {
                    // Whole seconds only: don't jump back and forth by one.
                    if (t - s.position_now()).abs() > 1.2 || s.phase != Phase::Playing {
                        s.position = t;
                    } else {
                        s.position = s.position_now();
                    }
                }
                _ => s.position = s.position_now(),
            }
            s.at = Instant::now();
        });
    }
}

impl Renderer<'_> {
    fn open(&self, m: &Media) -> Result<(), String> {
        let body = format!(
            "<CurrentURI>{}</CurrentURI><CurrentURIMetaData>{}</CurrentURIMetaData>",
            xml_escape(&m.url),
            xml_escape(&didl(m))
        );
        self.av("SetAVTransportURI", &body)?;
        if !m.paused || m.start > 1.0 {
            self.av("Play", "<Speed>1</Speed>")?;
        }
        Ok(())
    }

    fn seek(&self, t: f64) -> Result<String, String> {
        self.av(
            "Seek",
            &format!("<Unit>REL_TIME</Unit><Target>{}</Target>", clock(t)),
        )
    }

    /// CurrentTransportState and CurrentTransportStatus.
    fn transport_state(&self) -> Result<(String, String), String> {
        let xml = self.av("GetTransportInfo", "")?;
        Ok((
            field(&xml, "CurrentTransportState").unwrap_or_default(),
            field(&xml, "CurrentTransportStatus").unwrap_or_default(),
        ))
    }

    fn position(&self) -> Option<f64> {
        let xml = self.av("GetPositionInfo", "").ok()?;
        parse_clock(&field(&xml, "RelTime")?)
    }

    fn volume(&self) -> (Option<f64>, Option<bool>) {
        let v = self.rc("GetVolume", "<Channel>Master</Channel>").ok();
        let m = self.rc("GetMute", "<Channel>Master</Channel>").ok();
        (
            v.and_then(|x| field(&x, "CurrentVolume")?.parse::<f64>().ok())
                .map(|v| (v / self.max_volume).clamp(0.0, 1.0)),
            m.and_then(|x| field(&x, "CurrentMute"))
                .map(|m| m == "1" || m.eq_ignore_ascii_case("true")),
        )
    }

    /// The content types it says it plays.
    fn sink(&self) -> Vec<String> {
        let u = &self.d.dlna;
        let Some(url) = &u.connection else {
            return Vec::new();
        };
        self.soap(url, &u.connection_type, "GetProtocolInfo", "")
            .ok()
            .and_then(|x| field(&x, "Sink"))
            .map(|s| parse_sink(&s))
            .unwrap_or_default()
    }

    fn av(&self, action: &str, args: &str) -> Result<String, String> {
        let u = &self.d.dlna;
        self.soap(
            &u.transport,
            &u.transport_type,
            action,
            &format!("<InstanceID>0</InstanceID>{args}"),
        )
    }

    fn rc(&self, action: &str, args: &str) -> Result<String, String> {
        let u = &self.d.dlna;
        let Some(url) = &u.rendering else {
            return Err("it has no volume control".into());
        };
        self.soap(
            url,
            &u.rendering_type,
            action,
            &format!("<InstanceID>0</InstanceID>{args}"),
        )
    }

    fn soap(&self, url: &str, service: &str, action: &str, args: &str) -> Result<String, String> {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
             <s:Body><u:{action} xmlns:u=\"{service}\">{args}</u:{action}></s:Body></s:Envelope>"
        );
        let mut resp = self
            .agent
            .post(url)
            .header("User-Agent", network::AGENT)
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header("SOAPAction", format!("\"{service}#{action}\""))
            .send(body.as_bytes())
            .map_err(|e| e.to_string())?;
        let ok = resp.status().is_success();
        let xml = resp
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_string()
            .map_err(|e| e.to_string())?;
        if ok {
            return Ok(xml);
        }
        let code = field(&xml, "errorCode").unwrap_or_default();
        Err(match code.as_str() {
            "701" => "it can't do that right now".into(),
            "704" | "714" => "it can't play this format".into(),
            "710" | "711" => "it can't seek in this".into(),
            "716" => "it couldn't reach this computer".into(),
            _ => field(&xml, "errorDescription")
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| format!("{action} failed ({})", resp.status())),
        })
    }
}

/// The text of the first element named `name`.
fn field(xml: &str, name: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let n = doc.descendants().find(|n| n.tag_name().name() == name)?;
    Some(n.text().unwrap_or("").trim().to_owned())
}

/// RenderingControl's loudest volume.
fn volume_max(scpd: &str) -> Option<f64> {
    let doc = roxmltree::Document::parse(scpd).ok()?;
    let var = doc.descendants().find(|n| {
        n.tag_name().name() == "stateVariable"
            && n.children().any(|c| {
                c.tag_name().name() == "name" && c.text().is_some_and(|t| t.trim() == "Volume")
            })
    })?;
    let max = var
        .descendants()
        .find(|n| n.tag_name().name() == "maximum")?
        .text()?;
    max.trim().parse::<f64>().ok().filter(|m| *m >= 1.0)
}

/// "http-get:*:audio/mpeg:DLNA.ORG_PN=MP3,…" → the content types.
fn parse_sink(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in s.split(',') {
        let mut f = entry.split(':');
        if f.next().map(str::trim) == Some("http-get")
            && let Some(mime) = f.nth(1)
        {
            let mime = mime.split(';').next().unwrap_or(mime).trim().to_lowercase();
            if !mime.is_empty() && !out.contains(&mime) {
                out.push(mime);
            }
        }
    }
    out
}

/// "0:02:05".
fn clock(t: f64) -> String {
    let t = t.max(0.0) as u64;
    format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}

/// "0:02:05.5" (or "NOT_IMPLEMENTED").
fn parse_clock(s: &str) -> Option<f64> {
    let mut t = 0.0;
    let mut parts = 0;
    for p in s.split(':') {
        t = t * 60.0 + p.trim().parse::<f64>().ok()?;
        parts += 1;
    }
    (parts == 3 && t.is_finite() && t >= 0.0).then_some(t)
}

/// What the renderer is told about the media.
fn didl(m: &Media) -> String {
    let class = if m.video {
        "object.item.videoItem.movie"
    } else {
        "object.item.audioItem.musicTrack"
    };
    let mut x = format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\" \
         xmlns:dlna=\"urn:schemas-dlna-org:metadata-1-0/\" xmlns:sec=\"http://www.sec.co.kr/\">\
         <item id=\"{}\" parentID=\"0\" restricted=\"1\"><dc:title>{}</dc:title><upnp:class>{class}</upnp:class>",
        m.id,
        xml_escape(&m.title)
    );
    if !m.artist.is_empty() {
        x += &format!(
            "<dc:creator>{0}</dc:creator><upnp:artist>{0}</upnp:artist>",
            xml_escape(&m.artist)
        );
    }
    if !m.album.is_empty() {
        x += &format!("<upnp:album>{}</upnp:album>", xml_escape(&m.album));
    }
    if let Some(art) = &m.art {
        x += &format!("<upnp:albumArtURI>{}</upnp:albumArtURI>", xml_escape(art));
    }
    if let Some(srt) = &m.srt {
        // Samsung's way; the plain <res> below is everyone else's.
        x += &format!(
            "<sec:CaptionInfoEx sec:type=\"srt\">{}</sec:CaptionInfoEx>",
            xml_escape(srt)
        );
    }
    let length = if m.duration > 0.0 {
        format!(" duration=\"{}.000\"", clock(m.duration))
    } else {
        String::new()
    };
    x += &format!(
        "<res protocolInfo=\"http-get:*:{}:{}\"{length}>{}</res>",
        xml_escape(&m.mime),
        xml_escape(&m.dlna),
        xml_escape(&m.url)
    );
    if let Some(srt) = &m.srt {
        x += &format!(
            "<res protocolInfo=\"http-get:*:text/srt:*\">{}</res>",
            xml_escape(srt)
        );
    }
    x + "</item></DIDL-Lite>"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks() {
        assert_eq!(clock(7325.9), "2:02:05");
        assert_eq!(parse_clock("0:02:05"), Some(125.0));
        assert_eq!(parse_clock("00:00:01.500"), Some(1.5));
        assert_eq!(parse_clock("NOT_IMPLEMENTED"), None);
        assert_eq!(parse_clock(""), None);
    }

    #[test]
    fn answers() {
        let x = r#"<s:Envelope xmlns:s="x"><s:Body><u:R xmlns:u="y"><CurrentTransportState>PLAYING</CurrentTransportState></u:R></s:Body></s:Envelope>"#;
        assert_eq!(
            field(x, "CurrentTransportState").as_deref(),
            Some("PLAYING")
        );
        assert_eq!(field("not xml", "a"), None);
        assert_eq!(
            parse_sink(
                "http-get:*:audio/mpeg:DLNA.ORG_PN=MP3,http-get:*:audio/mpeg:*,rtsp-rtp-udp:*:video/mp4:*,http-get:*:audio/L16;rate=44100:*"
            ),
            vec!["audio/mpeg", "audio/l16"]
        );
        let scpd = "<scpd><serviceStateTable><stateVariable><name>Mute</name></stateVariable>\
            <stateVariable><name>Volume</name><allowedValueRange><minimum>0</minimum><maximum>50</maximum></allowedValueRange></stateVariable>\
            </serviceStateTable></scpd>";
        assert_eq!(volume_max(scpd), Some(50.0));
    }

    #[test]
    fn description_is_valid_xml() {
        let m = Media {
            id: 3,
            url: "http://h/1/a.mp4?x=1&y=2".into(),
            mime: "video/mp4".into(),
            title: "Tom & <Jerry>".into(),
            artist: "\"X\"".into(),
            srt: Some("http://h/1/a.srt".into()),
            art: Some("http://h/1/art.jpg".into()),
            dlna: "DLNA.ORG_OP=01".into(),
            duration: 3700.0,
            video: true,
            ..Default::default()
        };
        let x = didl(&m);
        let doc = roxmltree::Document::parse(&x).unwrap();
        let res = doc
            .descendants()
            .find(|n| n.tag_name().name() == "res")
            .unwrap();
        assert_eq!(res.text(), Some("http://h/1/a.mp4?x=1&y=2"));
        assert_eq!(
            res.attribute("protocolInfo"),
            Some("http-get:*:video/mp4:DLNA.ORG_OP=01")
        );
        assert_eq!(res.attribute("duration"), Some("1:01:40.000"));
        assert_eq!(field(&x, "title").as_deref(), Some("Tom & <Jerry>"));
    }
}
