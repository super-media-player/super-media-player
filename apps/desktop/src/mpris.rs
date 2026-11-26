//! MPRIS over D-Bus: media keys, GNOME's media controls in the top bar and
//! on the lock screen, KDE Connect and the like control the player and show
//! what is playing.
//!
//! zbus serves the two interfaces on its own thread. Commands from D-Bus go
//! to the UI thread through `on_command`; the UI thread sends a [`Snapshot`]
//! of the player now and then, and the properties that changed are announced.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::blocking;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

const PATH: &str = "/org/mpris/MediaPlayer2";
const NAME: &str = "org.mpris.MediaPlayer2.super_media_player";

/// What a D-Bus client asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Relative seek, microseconds.
    Seek(i64),
    /// Absolute position, microseconds.
    SetPosition(i64),
    /// 0.0 ..= 1.5 (150 %).
    Volume(f64),
    Rate(f64),
    Shuffle(bool),
    /// "None", "Track" or "Playlist".
    Loop(String),
    OpenUri(String),
    Raise,
    Quit,
}

/// The player as MPRIS describes it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    /// "Playing", "Paused" or "Stopped".
    pub status: &'static str,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// file:// URL of the cover, if there is one.
    pub art_url: String,
    /// Changes with every new file (the track id).
    pub track: u64,
    pub length_us: i64,
    pub position_us: i64,
    pub volume: f64,
    pub rate: f64,
    pub shuffle: bool,
    pub loop_status: &'static str,
    pub can_go_next: bool,
    pub can_go_previous: bool,
}

type Sink = Arc<dyn Fn(Command) + Send + Sync>;

struct Root {
    send: Sink,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {
        (self.send)(Command::Raise);
    }
    fn quit(&self) {
        (self.send)(Command::Quit);
    }
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> String {
        "Super Media Player".into()
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        "super-media-player".into()
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        ["file", "http", "https", "dvd", "bd", "cdda"]
            .map(String::from)
            .to_vec()
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        [
            "audio/mpeg",
            "audio/flac",
            "audio/ogg",
            "audio/x-wav",
            "video/mp4",
            "video/x-matroska",
            "video/webm",
        ]
        .map(String::from)
        .to_vec()
    }
}

struct PlayerIface {
    send: Sink,
    state: Arc<Mutex<Snapshot>>,
}

impl PlayerIface {
    fn snap(&self) -> Snapshot {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

fn track_path(track: u64) -> ObjectPath<'static> {
    ObjectPath::try_from(format!("/org/super_media_player/track/{track}")).unwrap_or_else(|_| {
        ObjectPath::from_static_str_unchecked("/org/mpris/MediaPlayer2/TrackList/NoTrack")
    })
}

fn owned(v: Value<'_>) -> Option<OwnedValue> {
    OwnedValue::try_from(v).ok()
}

/// The `Metadata` property's map.
pub fn metadata(s: &Snapshot) -> HashMap<String, OwnedValue> {
    let mut m = HashMap::new();
    if s.status == "Stopped" {
        return m;
    }
    let mut put = |k: &str, v: Value<'_>| {
        if let Some(v) = owned(v) {
            m.insert(k.to_owned(), v);
        }
    };
    put("mpris:trackid", Value::from(track_path(s.track)));
    if s.length_us > 0 {
        put("mpris:length", Value::from(s.length_us));
    }
    if !s.title.is_empty() {
        put("xesam:title", Value::from(s.title.clone()));
    }
    if !s.artist.is_empty() {
        put("xesam:artist", Value::from(vec![s.artist.clone()]));
    }
    if !s.album.is_empty() {
        put("xesam:album", Value::from(s.album.clone()));
    }
    if !s.art_url.is_empty() {
        put("mpris:artUrl", Value::from(s.art_url.clone()));
    }
    m
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl PlayerIface {
    fn next(&self) {
        (self.send)(Command::Next);
    }
    fn previous(&self) {
        (self.send)(Command::Previous);
    }
    fn pause(&self) {
        (self.send)(Command::Pause);
    }
    fn play_pause(&self) {
        (self.send)(Command::PlayPause);
    }
    fn stop(&self) {
        (self.send)(Command::Stop);
    }
    fn play(&self) {
        (self.send)(Command::Play);
    }
    fn seek(&self, offset: i64) {
        (self.send)(Command::Seek(offset));
    }
    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        // Ignored when it is about an earlier track, as the spec asks.
        if track_id == track_path(self.snap().track) {
            (self.send)(Command::SetPosition(position));
        }
    }
    fn open_uri(&self, uri: String) {
        (self.send)(Command::OpenUri(uri));
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        let s = self.snap().status;
        if s.is_empty() { "Stopped" } else { s }.into()
    }
    #[zbus(property)]
    fn loop_status(&self) -> String {
        let s = self.snap().loop_status;
        if s.is_empty() { "None" } else { s }.into()
    }
    #[zbus(property)]
    fn set_loop_status(&mut self, v: String) {
        (self.send)(Command::Loop(v));
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        let r = self.snap().rate;
        if r > 0.0 { r } else { 1.0 }
    }
    #[zbus(property)]
    fn set_rate(&mut self, v: f64) {
        if v > 0.0 {
            (self.send)(Command::Rate(v));
        }
    }
    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.snap().shuffle
    }
    #[zbus(property)]
    fn set_shuffle(&mut self, v: bool) {
        (self.send)(Command::Shuffle(v));
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        metadata(&self.snap())
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.snap().volume
    }
    #[zbus(property)]
    fn set_volume(&mut self, v: f64) {
        (self.send)(Command::Volume(v.clamp(0.0, 1.5)));
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.snap().position_us
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        0.25
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        4.0
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.snap().can_go_next
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.snap().can_go_previous
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.snap().length_us > 0
    }
    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }
}

/// The running MPRIS service.
pub struct Mpris {
    conn: blocking::Connection,
    state: Arc<Mutex<Snapshot>>,
    last: Snapshot,
}

impl Mpris {
    /// Register on the session bus. None when there is no session bus.
    pub fn start(on_command: impl Fn(Command) + Send + Sync + 'static) -> Option<Mpris> {
        let send: Sink = Arc::new(on_command);
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let build = |name: &str| -> zbus::Result<blocking::Connection> {
            blocking::connection::Builder::session()?
                .name(name.to_owned())?
                .serve_at(PATH, Root { send: send.clone() })?
                .serve_at(
                    PATH,
                    PlayerIface {
                        send: send.clone(),
                        state: state.clone(),
                    },
                )?
                .build()
        };
        // A second player (single instance turned off) takes its own name.
        let conn = build(NAME)
            .or_else(|_| build(&format!("{NAME}.instance{}", std::process::id())))
            .map_err(|e| log::info!("MPRIS unavailable: {e}"))
            .ok()?;
        log::info!("MPRIS: registered on the session bus");
        Some(Mpris {
            conn,
            state,
            last: Snapshot::default(),
        })
    }

    /// Publish the player's state; announce what changed.
    pub fn update(&mut self, s: Snapshot) {
        if let Ok(mut st) = self.state.lock() {
            *st = s.clone();
        }
        let old = std::mem::replace(&mut self.last, s.clone());
        // A jump in position the clock doesn't explain is a seek.
        let jumped = s.track == old.track
            && !old.status.is_empty()
            && (s.position_us - old.position_us).abs() > 3_000_000
            && s.status != "Stopped";
        let Ok(iface) = self.conn.object_server().interface::<_, PlayerIface>(PATH) else {
            return;
        };
        let emitter = iface.signal_emitter().clone();
        let p = iface.get();
        let r: zbus::Result<()> = zbus::block_on(async {
            if s.status != old.status {
                p.playback_status_changed(&emitter).await?;
            }
            if metadata(&s) != metadata(&old) {
                p.metadata_changed(&emitter).await?;
            }
            if s.volume != old.volume {
                p.volume_changed(&emitter).await?;
            }
            if s.rate != old.rate {
                p.rate_changed(&emitter).await?;
            }
            if s.shuffle != old.shuffle {
                p.shuffle_changed(&emitter).await?;
            }
            if s.loop_status != old.loop_status {
                p.loop_status_changed(&emitter).await?;
            }
            if s.can_go_next != old.can_go_next {
                p.can_go_next_changed(&emitter).await?;
            }
            if s.can_go_previous != old.can_go_previous {
                p.can_go_previous_changed(&emitter).await?;
            }
            if (s.length_us > 0) != (old.length_us > 0) {
                p.can_seek_changed(&emitter).await?;
            }
            if jumped {
                PlayerIface::seeked(&emitter, s.position_us).await?;
            }
            Ok(())
        });
        if let Err(e) = r {
            log::debug!("MPRIS: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_has_track_and_tags() {
        let s = Snapshot {
            status: "Playing",
            title: "Song".into(),
            artist: "Band".into(),
            track: 3,
            length_us: 5_000_000,
            ..Default::default()
        };
        let m = metadata(&s);
        assert!(m.contains_key("mpris:trackid"));
        assert!(m.contains_key("xesam:title"));
        assert!(m.contains_key("xesam:artist"));
        assert!(m.contains_key("mpris:length"));
        assert!(!m.contains_key("xesam:album"));
        assert!(
            metadata(&Snapshot {
                status: "Stopped",
                ..s
            })
            .is_empty()
        );
    }
}
