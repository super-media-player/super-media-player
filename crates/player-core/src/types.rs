//! Plain data types the player reports to the UI.

use crate::node::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
}

/// One entry of mpv's `track-list`.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// mpv track id, unique within its kind. Use it with [`crate::Player::select_track`].
    pub id: i64,
    pub kind: TrackKind,
    pub title: Option<String>,
    pub lang: Option<String>,
    pub codec: Option<String>,
    pub selected: bool,
    pub default: bool,
    pub external: bool,
    /// Embedded cover art presented as a video track.
    pub albumart: bool,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub channels: Option<String>,
    /// The number in the file (an MPEG stream number, for DVDs).
    pub src_id: Option<i64>,
}

impl Track {
    pub(crate) fn from_value(v: &Value) -> Option<Self> {
        let kind = match v.get("type")?.as_str()? {
            "video" => TrackKind::Video,
            "audio" => TrackKind::Audio,
            "sub" => TrackKind::Subtitle,
            _ => return None,
        };
        Some(Self {
            id: v.get("id")?.as_i64()?,
            kind,
            title: v.str_field("title"),
            lang: v.str_field("lang"),
            codec: v.str_field("codec"),
            selected: v.bool_field("selected"),
            default: v.bool_field("default"),
            external: v.bool_field("external"),
            albumart: v.bool_field("albumart"),
            width: v.get("demux-w").and_then(Value::as_i64),
            height: v.get("demux-h").and_then(Value::as_i64),
            channels: v.str_field("demux-channels"),
            src_id: v.get("src-id").and_then(Value::as_i64),
        })
    }

    /// Human-readable label, e.g. "English · aac · 5.1".
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(t) = &self.title {
            parts.push(t.clone());
        }
        if let Some(l) = &self.lang {
            parts.push(l.clone());
        }
        if let Some(c) = &self.codec {
            parts.push(c.clone());
        }
        if let (Some(w), Some(h)) = (self.width, self.height) {
            parts.push(format!("{w}×{h}"));
        }
        if let Some(ch) = &self.channels {
            parts.push(ch.clone());
        }
        if self.external {
            parts.push("external".into());
        }
        if parts.is_empty() {
            format!("Track {}", self.id)
        } else {
            parts.join(" · ")
        }
    }
}

/// One entry of mpv's `playlist`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistEntry {
    pub filename: String,
    pub title: Option<String>,
    pub current: bool,
    pub playing: bool,
}

impl PlaylistEntry {
    pub(crate) fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            filename: v.str_field("filename")?,
            title: v.str_field("title"),
            current: v.bool_field("current"),
            playing: v.bool_field("playing"),
        })
    }

    /// Title if known, else the file name without its directory.
    pub fn display_name(&self) -> String {
        if let Some(t) = &self.title {
            return t.clone();
        }
        display_name_for(&self.filename)
    }
}

/// Last path segment of a file path or URL, with percent-encoding left as is.
pub fn display_name_for(path_or_url: &str) -> String {
    let trimmed = path_or_url.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path_or_url)
        .to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Eof,
    Stop,
    Quit,
    Error,
    Redirect,
    Other,
}

/// Properties the player observes and reports when they change.
#[derive(Debug, Clone, PartialEq)]
pub enum Property {
    /// Playback position in seconds; `None` when nothing is loaded.
    TimePos(Option<f64>),
    Duration(Option<f64>),
    Pause(bool),
    /// Volume in percent (0–130 by default).
    Volume(f64),
    Mute(bool),
    MediaTitle(Option<String>),
    /// True when no file is loaded.
    Idle(bool),
    Tracks(Vec<Track>),
    Playlist(Vec<PlaylistEntry>),
    PlaylistPos(Option<i64>),
    Speed(f64),
    /// Network cache fill state in percent, while buffering.
    Buffering(Option<i64>),
    PausedForCache(bool),
    /// Name of the hardware decoder in use, or `None` for software decoding.
    HwDecoder(Option<String>),
    /// Tags of the current file (artist, album, ...), keys lowercased.
    Metadata(Vec<(String, String)>),
    LoopFile(bool),
    LoopPlaylist(bool),
    /// Path or URL of the current file.
    Path(Option<String>),
    /// Audio bitrate in bits per second.
    AudioBitrate(Option<f64>),
    /// Audio sample rate in Hz.
    SampleRate(Option<i64>),
    /// The window should fill the screen (a script can ask for it).
    Fullscreen(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    Property(Property),
    StartFile,
    FileLoaded,
    EndFile {
        reason: EndReason,
        error: Option<String>,
    },
    Seek,
    PlaybackRestart,
    Log {
        level: String,
        prefix: String,
        text: String,
    },
    Shutdown,
}
