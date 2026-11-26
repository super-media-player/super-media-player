//! Deciding how a file goes to a device (as it is, or converted), and
//! converting it while it is sent.
//!
//! The FFmpeg program does the converting when it is installed: it can pass
//! the picture on untouched and only convert the sound, which costs next to
//! nothing. Without it (and for web sites, and to draw subtitles into the
//! picture) a hidden mpv encodes everything. Either writes into a pipe, so
//! nothing is stored, and waits whenever the TV has buffered enough.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use player_core::{LoadMode, Player, PlayerConfig, PlayerEvent};

use super::discovery::Kind;

// ---- what is playing ----

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoTrack {
    pub id: i64,
    /// The stream's number in the file (FFmpeg's).
    pub index: Option<i64>,
    pub codec: String,
    pub width: i64,
    pub height: i64,
    pub ten_bit: bool,
    pub hdr: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioTrack {
    pub id: i64,
    pub index: Option<i64>,
    pub codec: String,
    pub channels: i64,
    pub rate: i64,
    /// What a TV plays by itself: the file's first.
    pub first: bool,
    /// Loaded from another file; then, how many the file itself has.
    pub file: Option<String>,
    pub embedded: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubTrack {
    pub id: i64,
    pub index: Option<i64>,
    pub codec: String,
    /// Loaded from another file; then, how many the file itself has.
    pub file: Option<String>,
    pub embedded: i64,
}

impl SubTrack {
    /// Pictures (DVD, Blu-ray): they can only be drawn into the video.
    pub fn is_picture(&self) -> bool {
        ["pgs", "dvd", "dvb", "xsub", "vobsub"]
            .iter()
            .any(|k| self.codec.contains(k))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Source {
    pub path: String,
    /// What mpv really opened (a web site's stream).
    pub stream: String,
    /// mpv's "file-format".
    pub format: String,
    pub duration: f64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art: Option<PathBuf>,
    pub video: Option<VideoTrack>,
    pub audio: Option<AudioTrack>,
    pub sub: Option<SubTrack>,
    pub sub_delay: f64,
    /// mpv options a web site needs (yt-dlp and the quality chosen).
    pub site: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Place {
    File,
    /// A plain file or radio stream on a web server.
    Web,
    /// A stream in pieces (HLS, DASH), which FFmpeg reads by itself.
    Pieces,
    /// A web site: only mpv (with yt-dlp) can open it.
    Site,
}

impl Source {
    fn place(&self) -> Result<Place, String> {
        let p = self.path.as_str();
        if [
            "dvd://",
            "smpdvd://",
            "dvdnav://",
            "bd://",
            "bluray://",
            "cdda://",
            "edl://",
        ]
        .iter()
        .any(|s| p.starts_with(s))
        {
            return Err(
                "Discs can't be cast directly: rip the disc first, then cast the file.".into(),
            );
        }
        if !p.contains("://") {
            return Ok(Place::File);
        }
        if !(p.starts_with("http://") || p.starts_with("https://")) {
            return Ok(Place::Pieces);
        }
        if !self.stream.is_empty() && self.stream != p {
            return Ok(Place::Site);
        }
        let pieces = ["hls", "dash", "applehttp"]
            .iter()
            .any(|f| self.format.contains(f));
        Ok(if pieces { Place::Pieces } else { Place::Web })
    }

    /// Content types for it, the usual one first.
    fn mimes(&self) -> &'static [&'static str] {
        let ext = Path::new(self.path.split('?').next().unwrap_or(""))
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let f = self.format.as_str();
        let video = self.video.is_some();
        let has = |n: &str| f.split(',').any(|x| x == n);
        if has("mp4") || has("mov") {
            return if video {
                &["video/mp4", "video/quicktime", "video/x-m4v"]
            } else {
                &["audio/mp4", "audio/x-m4a", "audio/m4a"]
            };
        }
        if has("mkv") || has("matroska") || has("webm") {
            return match (video, ext == "webm") {
                (true, true) => &["video/webm"],
                (true, false) => &["video/x-matroska", "video/x-mkv", "video/matroska"],
                (false, _) => &["audio/webm", "audio/x-matroska"],
            };
        }
        if has("mpegts") {
            return &["video/mp2t", "video/vnd.dlna.mpeg-tts", "video/mpeg"];
        }
        if has("mpeg") {
            return &["video/mpeg"];
        }
        if has("avi") {
            return &["video/x-msvideo", "video/avi", "video/divx"];
        }
        if has("asf") {
            return if video {
                &["video/x-ms-wmv", "video/x-ms-asf"]
            } else {
                &["audio/x-ms-wma"]
            };
        }
        if has("flv") {
            return &["video/x-flv"];
        }
        if has("mp3") {
            return &["audio/mpeg", "audio/mp3"];
        }
        if has("flac") {
            return &["audio/flac", "audio/x-flac"];
        }
        if has("ogg") {
            return if video {
                &["video/ogg"]
            } else {
                &["audio/ogg", "application/ogg", "audio/x-ogg"]
            };
        }
        if has("wav") {
            return &["audio/wav", "audio/x-wav", "audio/wave"];
        }
        if has("aac") {
            return &["audio/aac", "audio/vnd.dlna.adts", "audio/x-aac"];
        }
        &[]
    }
}

// ---- how it is sent ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    /// As it is where the device can play it, converted where not.
    #[default]
    Auto,
    Original,
    Convert,
}

pub struct Target<'a> {
    pub kind: Kind,
    /// It has a screen.
    pub video: bool,
    /// DLNA: the content types it plays (empty: it didn't say).
    pub sink: &'a [String],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoOut {
    None,
    Copy,
    H264,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioOut {
    None,
    Copy,
    Aac,
    Ac3,
    Mp3,
    Flac,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Ts,
    Mp3,
    Flac,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// Sent as it is.
    pub direct: bool,
    pub mime: String,
    pub video: VideoOut,
    pub audio: AudioOut,
    pub container: Container,
    /// Subtitles drawn into the picture.
    pub burn: bool,
    /// No larger than 720p.
    pub small: bool,
    /// Encode with mpv instead of the FFmpeg program.
    pub mpv: bool,
}

impl Plan {
    /// For people: "sent as it is", "sound converted".
    pub fn summary(&self) -> String {
        if self.direct {
            return "sent as it is".into();
        }
        let mut s = match (self.video, self.audio) {
            (VideoOut::Copy, AudioOut::Copy | AudioOut::None) => {
                "repackaged, quality untouched".to_owned()
            }
            (VideoOut::Copy, _) => "picture as it is, sound converted".into(),
            (VideoOut::H264, _) if self.small => "converted to H.264, 720p".into(),
            (VideoOut::H264, _) => "converted to H.264".into(),
            (VideoOut::None, AudioOut::Copy) => "sound as it is".into(),
            (VideoOut::None, AudioOut::Flac) => "sound converted to FLAC".into(),
            (VideoOut::None, _) => "sound converted to MP3".into(),
        };
        if self.burn {
            s += ", with subtitles";
        }
        s
    }
}

/// The content type of `names` the device takes, if any.
fn accepted(t: &Target, names: &[&str]) -> Option<String> {
    let yes = |m: &&&str| match t.kind {
        Kind::Chromecast => matches!(
            **m,
            "video/mp4"
                | "video/webm"
                | "video/x-matroska"
                | "audio/mp4"
                | "audio/webm"
                | "audio/mpeg"
                | "audio/flac"
                | "audio/ogg"
                | "audio/wav"
                | "audio/aac"
        ),
        Kind::Dlna if t.sink.is_empty() => matches!(
            **m,
            "video/mp4"
                | "video/x-matroska"
                | "video/mp2t"
                | "video/mpeg"
                | "video/x-msvideo"
                | "audio/mpeg"
                | "audio/mp4"
                | "audio/flac"
                | "audio/wav"
        ),
        Kind::Dlna => t.sink.iter().any(|s| {
            s == **m
                || s == "*"
                || Some(s.as_str())
                    == m.split('/')
                        .next()
                        .map(|k| if k == "video" { "video/*" } else { "audio/*" })
        }),
    };
    names.iter().find(yes).map(|m| (*m).to_owned())
}

fn plays_video(t: &Target, v: &VideoTrack) -> bool {
    match (t.kind, v.codec.as_str()) {
        // Ten-bit H.264 (some anime) plays on no TV at all.
        (_, "h264") => !v.ten_bit,
        (Kind::Chromecast, "vp8" | "vp9" | "hevc" | "av1") => true,
        (Kind::Dlna, "hevc" | "mpeg2video" | "mpeg1video" | "mpeg4") => true,
        (Kind::Dlna, "vp9" | "av1") => t.sink.iter().any(|s| s == "video/webm"),
        (Kind::Dlna, "wmv3" | "vc1") => t.sink.iter().any(|s| s.contains("wmv")),
        _ => false,
    }
}

fn plays_audio(t: &Target, codec: &str) -> bool {
    match (t.kind, codec) {
        (_, "aac" | "mp3" | "flac") => true,
        (_, c) if c.starts_with("pcm_s16") || c.starts_with("pcm_s24") => true,
        (Kind::Chromecast, "opus" | "vorbis") => true,
        (Kind::Dlna, "ac3" | "eac3" | "mp2") => true,
        (Kind::Dlna, "vorbis") => t
            .sink
            .iter()
            .any(|s| s.contains("ogg") || s.contains("vorbis")),
        (Kind::Dlna, c) if c.starts_with("wma") => t.sink.iter().any(|s| s.contains("wma")),
        _ => false,
    }
}

/// How to send `src` to `t`. `attempt`: 0 first; 1 and 2 after the device
/// refused the try before.
pub fn plan(
    src: &Source,
    t: &Target,
    quality: Quality,
    burn_text: bool,
    attempt: u8,
    ffmpeg: bool,
) -> Result<Plan, String> {
    let place = src.place()?;
    if src.video.is_none() && src.audio.is_none() {
        return Err("There is nothing to cast in this file.".into());
    }
    let video = src.video.as_ref().filter(|_| t.video);
    if video.is_none() && src.audio.is_none() {
        return Err("This device only plays sound, and the file has none.".into());
    }
    let burn = video.is_some()
        && quality != Quality::Original
        && src
            .sub
            .as_ref()
            .is_some_and(|s| s.is_picture() || burn_text);

    // As it is?
    let whole = matches!(place, Place::File | Place::Web) && (src.video.is_none() || t.video);
    let mime = accepted(t, src.mimes());
    let codecs = video.is_none_or(|v| plays_video(t, v))
        && src
            .audio
            .as_ref()
            .is_none_or(|a| plays_audio(t, &a.codec) && a.first && a.file.is_none());
    if quality == Quality::Original {
        if !whole {
            return Err("This can't be sent as it is: choose Automatic.".into());
        }
        let mime = mime.or_else(|| src.mimes().first().map(|m| (*m).to_owned()));
        return mime
            .map(direct)
            .ok_or_else(|| "This can't be sent as it is: choose Automatic.".into());
    }
    if whole
        && quality == Quality::Auto
        && attempt == 0
        && codecs
        && !burn
        && let Some(m) = mime
    {
        return Ok(direct(m));
    }

    // Converted.
    let mpv = burn || place == Place::Site || !ffmpeg;
    let untouched = !mpv
        && quality == Quality::Auto
        && [
            "mkv", "matroska", "webm", "mp4", "mov", "mpegts", "flv", "hls",
        ]
        .iter()
        .any(|f| src.format.contains(f));
    let (container, mime) = match (video.is_some(), t.kind) {
        (true, Kind::Chromecast) => (Container::Mp4, Some("video/mp4".into())),
        (true, Kind::Dlna) => {
            match accepted(t, &["video/vnd.dlna.mpeg-tts", "video/mp2t", "video/mpeg"]) {
                Some(m) => (Container::Ts, Some(m)),
                None => (Container::Mp4, accepted(t, &["video/mp4"])),
            }
        }
        (false, Kind::Chromecast) => (Container::Flac, Some("audio/flac".into())),
        (false, Kind::Dlna) => match accepted(t, &["audio/flac", "audio/x-flac"])
            .filter(|_| !t.sink.is_empty() && attempt == 0)
        {
            Some(m) => (Container::Flac, Some(m)),
            None => (Container::Mp3, Some("audio/mpeg".into())),
        },
    };
    let mime = mime.unwrap_or_else(|| "video/mpeg".into());
    let video_out = match video {
        None => VideoOut::None,
        Some(v) => {
            let copy = untouched
                && match v.codec.as_str() {
                    "h264" => !v.ten_bit && attempt < 2,
                    "hevc" => attempt == 0,
                    _ => false,
                };
            if copy { VideoOut::Copy } else { VideoOut::H264 }
        }
    };
    let audio_out = match (&src.audio, container) {
        (None, _) => AudioOut::None,
        (Some(_), Container::Flac) => AudioOut::Flac,
        (Some(_), Container::Mp3) => AudioOut::Mp3,
        (Some(a), Container::Mp4) => {
            if untouched && a.codec == "aac" && attempt < 2 {
                AudioOut::Copy
            } else {
                AudioOut::Aac
            }
        }
        (Some(a), Container::Ts) => match a.codec.as_str() {
            "ac3" | "aac" | "mp2" | "mp3" if untouched && attempt < 2 => AudioOut::Copy,
            // TVs take surround as AC-3.
            _ if a.channels > 2 => AudioOut::Ac3,
            _ => AudioOut::Aac,
        },
    };
    Ok(Plan {
        direct: false,
        mime,
        video: video_out,
        audio: audio_out,
        container,
        burn,
        small: attempt >= 2,
        mpv,
    })
}

fn direct(mime: String) -> Plan {
    Plan {
        direct: true,
        mime,
        video: VideoOut::Copy,
        audio: AudioOut::Copy,
        container: Container::Mp4,
        burn: false,
        small: false,
        mpv: false,
    }
}

/// DLNA's notes on a stream: can it be sought by bytes, is it converted.
pub fn dlna_features(direct: bool) -> String {
    let (op, ci) = if direct { ("01", "0") } else { ("00", "1") };
    format!("DLNA.ORG_OP={op};DLNA.ORG_CI={ci};DLNA.ORG_FLAGS=01700000000000000000000000000000")
}

// ---- converting ----

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub source: Source,
    pub plan: Plan,
    /// Where to start, seconds.
    pub start: f64,
}

/// A program of FFmpeg's, where one is installed.
pub fn program(name: &str) -> Option<PathBuf> {
    let dirs = std::env::var_os("PATH")?;
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    std::env::split_paths(&dirs)
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
}

/// How FFmpeg should open `path`: a file name must not look like a protocol.
fn input(path: &str) -> String {
    if path.contains("://") {
        path.to_owned()
    } else {
        format!("file:{path}")
    }
}

fn run_text(cmd: &mut Command) -> Option<String> {
    let out = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Where a copied picture can start near `start`: it has to be a keyframe.
/// Going on from `from`, it must not land there again. Blocks.
pub fn keyframe_near(src: &Source, start: f64, from: f64) -> Option<f64> {
    if start <= 0.0 {
        return Some(0.0);
    }
    let text = run_text(
        Command::new(program("ffprobe")?)
            .args(["-v", "error", "-select_streams", "v:0", "-read_intervals"])
            .arg(format!("{start:.3}%+12"))
            .args(["-show_entries", "packet=pts_time,flags", "-of", "csv=p=0"])
            .arg(input(&src.path)),
    )?;
    let keys: Vec<f64> = text
        .lines()
        .filter_map(|l| {
            let (t, flags) = l.split_once(',')?;
            flags.starts_with('K').then(|| t.parse().ok()).flatten()
        })
        .collect();
    pick_keyframe(&keys, start, from)
}

fn pick_keyframe(keys: &[f64], start: f64, from: f64) -> Option<f64> {
    let near = keys
        .iter()
        .copied()
        .min_by(|a, b| (a - start).abs().total_cmp(&(b - start).abs()))?;
    // Forward, but the nearest is where it already is, or behind.
    let ahead = (start > from + 1.0 && near <= from + 0.5)
        .then(|| {
            keys.iter()
                .copied()
                .filter(|k| *k > start)
                .min_by(f64::total_cmp)
        })
        .flatten();
    let k = ahead.unwrap_or(near);
    // Far off: the file's clock doesn't start at 0.
    (k >= 0.0 && (k - start).abs() < 30.0).then_some(k)
}

/// The filters that make any picture one every TV plays.
fn video_filter(src: &Source, small: bool) -> String {
    let (w, h) = if small { (1280, 720) } else { (1920, 1080) };
    let hdr = src.video.as_ref().is_some_and(|v| v.hdr);
    let mut f = String::from("yadif=deint=interlaced,scale=w='trunc(iw*sar/2)*2':h=ih,setsar=1,");
    f += &format!(
        "scale=w='min({w},iw)':h='min({h},ih)':force_original_aspect_ratio=decrease:force_divisible_by=2"
    );
    if hdr {
        f += ",zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv";
    }
    f + ",format=yuv420p"
}

fn ffmpeg_args(job: &Job) -> Vec<String> {
    let (src, plan) = (&job.source, &job.plan);
    let mut a: Vec<String> = Vec::new();
    let mut add = |xs: &[&str]| a.extend(xs.iter().map(|s| (*s).to_owned()));
    add(&["-hide_banner", "-loglevel", "error", "-nostdin"]);
    // A copied picture starts at the keyframe `start` was set to: sound
    // must start there too. (FFmpeg seeks a little short of what is asked.)
    let copy = plan.video == VideoOut::Copy;
    let seek = if job.start <= 0.0 {
        String::new()
    } else if copy {
        format!("{:.3}", job.start + 0.2)
    } else {
        format!("{:.3}", job.start)
    };
    let mut open = |path: &str| {
        if !seek.is_empty() {
            if copy {
                add(&["-noaccurate_seek"]);
            }
            add(&["-ss", &seek]);
        }
        add(&["-i", &input(path)]);
    };
    open(&src.path);
    let outside = src.audio.as_ref().and_then(|x| x.file.clone());
    if let Some(f) = &outside {
        open(f);
    }
    if let Some(v) = src.video.as_ref().filter(|_| plan.video != VideoOut::None) {
        add(&[
            "-map",
            &v.index.map_or("0:v:0".into(), |i| format!("0:{i}")),
        ]);
    }
    match (&src.audio, &outside) {
        (Some(_), Some(_)) => add(&["-map", "1:a:0"]),
        (Some(x), None) => add(&[
            "-map",
            &x.index.map_or("0:a:0".into(), |i| format!("0:{i}")),
        ]),
        (None, _) => {}
    }
    match plan.video {
        VideoOut::None => add(&["-vn"]),
        VideoOut::Copy => {
            add(&["-c:v", "copy"]);
            if plan.container == Container::Mp4
                && src.video.as_ref().is_some_and(|v| v.codec == "hevc")
            {
                add(&["-tag:v", "hvc1"]);
            }
        }
        VideoOut::H264 => {
            add(&[
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-maxrate", "14M",
                "-bufsize", "28M",
            ]);
            add(&[
                "-profile:v",
                "high",
                "-force_key_frames",
                "expr:gte(t,n_forced*2)",
                "-vf",
                &video_filter(src, plan.small),
            ]);
        }
    }
    let rate = src.audio.as_ref().map_or(0, |x| x.rate);
    match plan.audio {
        AudioOut::None => add(&["-an"]),
        AudioOut::Copy => add(&["-c:a", "copy"]),
        AudioOut::Aac => add(&["-c:a", "aac", "-b:a", "256k", "-ac", "2", "-ar", "48000"]),
        AudioOut::Ac3 => add(&["-c:a", "ac3", "-b:a", "448k", "-ac", "6", "-ar", "48000"]),
        AudioOut::Mp3 => {
            add(&["-c:a", "libmp3lame", "-b:a", "320k", "-ac", "2"]);
            if ![32_000, 44_100, 48_000].contains(&rate) {
                add(&["-ar", "48000"]);
            }
        }
        AudioOut::Flac => {
            add(&["-c:a", "flac", "-ac", "2"]);
            if rate > 96_000 || rate == 0 {
                add(&["-ar", "48000"]);
            }
        }
    }
    add(&[
        "-sn",
        "-dn",
        "-map_chapters",
        "-1",
        "-map_metadata",
        "-1",
        "-avoid_negative_ts",
        "make_zero",
    ]);
    match plan.container {
        Container::Mp4 => add(&[
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof",
            "-f",
            "mp4",
        ]),
        Container::Ts => add(&["-muxdelay", "0", "-muxpreload", "0", "-f", "mpegts"]),
        Container::Mp3 => add(&["-f", "mp3"]),
        Container::Flac => add(&["-f", "flac"]),
    }
    add(&["pipe:1"]);
    a
}

/// mpv options for encoding into file descriptor `fd`.
#[cfg(unix)]
fn mpv_options(job: &Job, fd: i32) -> Vec<(String, String)> {
    let (src, plan) = (&job.source, &job.plan);
    let mut o: Vec<(&str, String)> = vec![
        ("o", format!("pipe:{fd}")),
        ("idle", "no".into()),
        ("hwdec", "no".into()),
        ("load-scripts", "no".into()),
        ("resume-playback", "no".into()),
        ("audio-display", "no".into()),
        ("sub-auto", "no".into()),
        ("audio-file-auto", "no".into()),
    ];
    if job.start > 0.0 {
        o.push(("start", format!("{:.3}", job.start)));
    }
    if let Some(a) = &src.audio {
        // The file's own tracks are numbered before the ones added.
        o.push((
            "aid",
            if a.file.is_some() {
                a.embedded + 1
            } else {
                a.id
            }
            .to_string(),
        ));
    }
    if plan.video == VideoOut::None {
        o.push(("vid", "no".into()));
    } else {
        if let Some(v) = &src.video {
            o.push(("vid", v.id.to_string()));
        }
        o.push(("ovc", "libx264".into()));
        o.push((
            "ovcopts",
            "preset=veryfast,crf=20,maxrate=14000000,bufsize=28000000,g=100".into(),
        ));
        o.push(("vf", format!("lavfi=[{}]", video_filter(src, plan.small))));
    }
    match (&src.sub, plan.burn) {
        (Some(s), true) => {
            o.push((
                "sid",
                if s.file.is_some() {
                    s.embedded + 1
                } else {
                    s.id
                }
                .to_string(),
            ));
            if src.sub_delay != 0.0 {
                o.push(("sub-delay", format!("{:.3}", src.sub_delay)));
            }
        }
        _ => o.push(("sid", "no".into())),
    }
    let audio = match plan.audio {
        AudioOut::None => {
            o.push(("aid", "no".into()));
            None
        }
        AudioOut::Mp3 => Some(("libmp3lame", "b=320k", "stereo")),
        AudioOut::Flac => Some(("flac", "", "stereo")),
        AudioOut::Ac3 => Some(("ac3", "b=448k", "5.1")),
        AudioOut::Aac | AudioOut::Copy => Some(("aac", "b=256k", "stereo")),
    };
    if let Some((codec, opts, channels)) = audio {
        o.push(("oac", codec.into()));
        o.push(("oacopts", opts.into()));
        o.push(("audio-channels", channels.into()));
        let rate = src.audio.as_ref().map_or(0, |x| x.rate);
        let keep = plan.video == VideoOut::None && [44_100, 48_000].contains(&rate);
        if !keep {
            o.push(("audio-samplerate", "48000".into()));
        }
    }
    match plan.container {
        Container::Mp4 => {
            o.push(("of", "mp4".into()));
            o.push((
                "ofopts",
                "movflags=frag_keyframe+empty_moov+default_base_moof".into(),
            ));
        }
        Container::Ts => o.push(("of", "mpegts".into())),
        Container::Mp3 => o.push(("of", "mp3".into())),
        Container::Flac => o.push(("of", "flac".into())),
    }
    o.into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .chain(src.site.iter().cloned())
        .collect()
}

/// A conversion under way, read as it is made. Dropping it stops it.
pub struct Encoder {
    out: Box<dyn Read + Send>,
    child: Option<Child>,
    stop: Arc<AtomicBool>,
}

impl Encoder {
    pub fn start(job: &Job) -> Result<Self, String> {
        if job.plan.mpv {
            Self::mpv(job)
        } else {
            Self::ffmpeg(job)
        }
    }

    fn ffmpeg(job: &Job) -> Result<Self, String> {
        let exe = program("ffmpeg").ok_or("FFmpeg isn't installed")?;
        let mut child = Command::new(exe)
            .args(ffmpeg_args(job))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let out = child.stdout.take().ok_or("no output")?;
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                for l in std::io::BufRead::lines(std::io::BufReader::new(err))
                    .map_while(Result::ok)
                    .take(50)
                {
                    log::info!("cast: ffmpeg: {l}");
                }
            });
        }
        Ok(Self {
            out: Box::new(out),
            child: Some(child),
            stop: Arc::default(),
        })
    }

    #[cfg(unix)]
    fn mpv(job: &Job) -> Result<Self, String> {
        use std::os::fd::AsRawFd;
        // A socket rather than a pipe: reads from it can time out.
        let (reader, writer) = std::os::unix::net::UnixStream::pair().map_err(|e| e.to_string())?;
        reader
            .set_read_timeout(Some(Duration::from_millis(500)))
            .map_err(|e| e.to_string())?;
        let mut cfg = PlayerConfig::headless();
        // Encoding mode brings its own outputs.
        cfg.options.retain(|(k, _)| k != "vo" && k != "ao");
        for (k, v) in mpv_options(job, writer.as_raw_fd()) {
            cfg.set(&k, &v);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let player = Player::new(&cfg, move |e| {
            let _ = tx.send(e);
        })
        .map_err(|e| e.to_string())?;
        let add = |list: &str, file: Option<&String>| {
            if let Some(f) = file {
                let _ = player.command(&["change-list", list, "append", f]);
            }
        };
        add(
            "audio-files",
            job.source.audio.as_ref().and_then(|a| a.file.as_ref()),
        );
        add(
            "sub-files",
            job.source
                .sub
                .as_ref()
                .and_then(|x| x.file.as_ref())
                .filter(|_| job.plan.burn),
        );
        player
            .load(&job.source.path, LoadMode::Replace)
            .map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        std::thread::spawn(move || {
            loop {
                if st.load(Ordering::Relaxed) {
                    let _ = player.stop();
                    break;
                }
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(PlayerEvent::EndFile { error, .. }) => {
                        if let Some(e) = error {
                            log::warn!("cast: couldn't convert: {e}");
                        }
                        break;
                    }
                    Ok(PlayerEvent::Shutdown)
                    | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    _ => {}
                }
            }
            // Finishes the stream; then the reader sees its end.
            drop(player);
            drop(writer);
        });
        Ok(Self {
            out: Box::new(reader),
            child: None,
            stop,
        })
    }

    #[cfg(not(unix))]
    fn mpv(_: &Job) -> Result<Self, String> {
        Err("Converting this needs the FFmpeg program".into())
    }
}

impl Read for Encoder {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.out.read(buf)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Close our end first, so a writer waiting on us gives up.
        self.out = Box::new(std::io::empty());
        if let Some(c) = &mut self.child {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

// ---- subtitles ----

#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// The cues of SRT or WebVTT text, without markup.
pub fn parse_cues(text: &str) -> Vec<Cue> {
    let text = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut lines = block.lines().map(str::trim).filter(|l| !l.is_empty());
        let Some(timing) = lines.by_ref().find(|l| l.contains("-->")) else {
            continue;
        };
        let Some((a, b)) = timing.split_once("-->") else {
            continue;
        };
        let b = b.split_whitespace().next().unwrap_or("");
        let (Some(start), Some(end)) = (
            crate::transcribe::parse_time(a),
            crate::transcribe::parse_time(b),
        ) else {
            continue;
        };
        let words: Vec<String> = lines
            .map(crate::transcribe::strip_markup)
            .filter(|l| !l.is_empty())
            .collect();
        if !words.is_empty() && end > start {
            out.push(Cue {
                start,
                end,
                text: words.join("\n"),
            });
        }
    }
    out
}

/// Text in whatever encoding: UTF-8, UTF-16, else Windows' western one.
fn decode(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] | [0xFE, 0xFF, rest @ ..] => {
            let le = bytes[0] == 0xFF;
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| {
                    if le {
                        u16::from_le_bytes([c[0], c[1]])
                    } else {
                        u16::from_be_bytes([c[0], c[1]])
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::from_utf8(bytes.to_vec())
            .unwrap_or_else(|_| bytes.iter().map(|&b| b as char).collect()),
    }
}

/// Read the cues of the subtitles chosen. Blocks (the whole film is read
/// when they are inside it).
pub fn read_cues(src: &Source) -> Vec<Cue> {
    let Some(sub) = src.sub.as_ref().filter(|s| !s.is_picture()) else {
        return Vec::new();
    };
    if let Some(f) = &sub.file {
        let ext = Path::new(f)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if ext == "srt" || ext == "vtt" {
            return std::fs::read(f)
                .map(|b| parse_cues(&decode(&b)))
                .unwrap_or_default();
        }
    }
    let Some(exe) = program("ffmpeg") else {
        return Vec::new();
    };
    let (file, map) = match (&sub.file, sub.index) {
        (Some(f), _) => (f.as_str(), "0:s:0".to_owned()),
        (None, Some(i)) => (src.path.as_str(), format!("0:{i}")),
        _ => return Vec::new(),
    };
    let text = run_text(Command::new(exe).args([
        "-v",
        "error",
        "-nostdin",
        "-i",
        &input(file),
        "-map",
        &map,
        "-f",
        "srt",
        "pipe:1",
    ]));
    text.map(|t| parse_cues(&t)).unwrap_or_default()
}

/// `cues` moved by `shift` seconds, as WebVTT (or SRT).
pub fn write_cues(cues: &[Cue], shift: f64, vtt: bool) -> String {
    let stamp = |t: f64| {
        let ms = (t * 1000.0).round().max(0.0) as u64;
        let (h, m, s, ms) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
        format!("{h:02}:{m:02}:{s:02}{}{ms:03}", if vtt { '.' } else { ',' })
    };
    let mut out = String::from(if vtt { "WEBVTT\n\n" } else { "" });
    for (i, c) in cues.iter().filter(|c| c.end + shift > 0.0).enumerate() {
        let text = if vtt {
            c.text
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        } else {
            c.text.clone()
        };
        out += &format!(
            "{}\n{} --> {}\n{text}\n\n",
            i + 1,
            stamp((c.start + shift).max(0.0)),
            stamp(c.end + shift)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CC: Target = Target {
        kind: Kind::Chromecast,
        video: true,
        sink: &[],
    };

    fn film(v: &str, a: &str, format: &str) -> Source {
        Source {
            path: "/v/film.mkv".into(),
            format: format.into(),
            duration: 100.0,
            video: Some(VideoTrack {
                id: 1,
                index: Some(0),
                codec: v.into(),
                width: 1920,
                height: 1080,
                ..Default::default()
            }),
            audio: Some(AudioTrack {
                id: 1,
                index: Some(1),
                codec: a.into(),
                channels: 6,
                rate: 48000,
                first: true,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn auto(s: &Source, t: &Target, attempt: u8) -> Plan {
        plan(s, t, Quality::Auto, false, attempt, true).unwrap()
    }

    #[test]
    fn chromecast_plans() {
        // Plays as it is; refused: repackaged, then converted.
        let f = film("h264", "aac", "mkv");
        assert!(auto(&f, &CC, 0).direct);
        assert_eq!(auto(&f, &CC, 0).mime, "video/x-matroska");
        assert_eq!(auto(&f, &CC, 1).video, VideoOut::Copy);
        assert_eq!(
            (auto(&f, &CC, 2).video, auto(&f, &CC, 2).small),
            (VideoOut::H264, true)
        );
        // A Blu-ray rip: only the sound is converted.
        let f = film("h264", "truehd", "mkv");
        let p = auto(&f, &CC, 0);
        assert_eq!(
            (p.direct, p.video, p.audio, p.mime.as_str()),
            (false, VideoOut::Copy, AudioOut::Aac, "video/mp4")
        );
        assert_eq!(p.summary(), "picture as it is, sound converted");
        // HEVC is tried, then converted.
        let f = film("hevc", "eac3", "mkv");
        assert_eq!(auto(&f, &CC, 0).video, VideoOut::Copy);
        assert_eq!(auto(&f, &CC, 1).video, VideoOut::H264);
        // Ten-bit H.264 and old formats are always converted.
        let mut f = film("h264", "flac", "mkv");
        f.video.as_mut().unwrap().ten_bit = true;
        assert_eq!(auto(&f, &CC, 0).video, VideoOut::H264);
        assert_eq!(
            auto(&film("mpeg4", "mp3", "avi"), &CC, 0).video,
            VideoOut::H264
        );
        // Another sound track than the first.
        let mut f = film("h264", "aac", "mkv");
        f.audio.as_mut().unwrap().first = false;
        let p = auto(&f, &CC, 0);
        assert_eq!(
            (p.direct, p.video, p.audio),
            (false, VideoOut::Copy, AudioOut::Copy)
        );
        // Subtitles that are pictures are drawn in; text ones when asked.
        let mut f = film("h264", "aac", "mkv");
        f.sub = Some(SubTrack {
            codec: "hdmv_pgs_subtitle".into(),
            ..Default::default()
        });
        let p = auto(&f, &CC, 0);
        assert!(p.burn && p.mpv && p.video == VideoOut::H264);
        f.sub.as_mut().unwrap().codec = "subrip".into();
        assert!(auto(&f, &CC, 0).direct);
        assert!(plan(&f, &CC, Quality::Auto, true, 0, true).unwrap().burn);
        // Without the FFmpeg program mpv encodes.
        assert!(
            plan(
                &film("h264", "dts", "mkv"),
                &CC,
                Quality::Auto,
                false,
                0,
                false
            )
            .unwrap()
            .mpv
        );
        // As the user wants it.
        assert!(
            plan(
                &film("mpeg4", "dts", "avi"),
                &CC,
                Quality::Original,
                false,
                0,
                true
            )
            .unwrap()
            .direct
        );
        assert_eq!(
            plan(
                &film("h264", "aac", "mkv"),
                &CC,
                Quality::Convert,
                false,
                0,
                true
            )
            .unwrap()
            .video,
            VideoOut::H264
        );
    }

    #[test]
    fn speakers_get_the_sound() {
        let t = Target { video: false, ..CC };
        let p = auto(&film("h264", "aac", "mkv"), &t, 0);
        assert_eq!(
            (p.video, p.audio, p.container),
            (VideoOut::None, AudioOut::Flac, Container::Flac)
        );
        let mut song = Source {
            path: "/m/a.flac".into(),
            format: "flac".into(),
            ..film("", "flac", "flac")
        };
        song.video = None;
        assert!(auto(&song, &t, 0).direct);
        song.path = "/m/a.wma".into();
        song.format = "asf".into();
        song.audio.as_mut().unwrap().codec = "wmav2".into();
        assert_eq!(auto(&song, &t, 0).audio, AudioOut::Flac);
        song.audio = None;
        assert!(plan(&song, &t, Quality::Auto, false, 0, true).is_err());
    }

    #[test]
    fn dlna_plans() {
        let sink: Vec<String> = ["video/mp4", "video/vnd.dlna.mpeg-tts", "audio/mpeg"]
            .map(String::from)
            .to_vec();
        let t = Target {
            kind: Kind::Dlna,
            video: true,
            sink: &sink,
        };
        // Its list has no MKV: repackaged as MPEG-TS.
        let p = auto(&film("h264", "ac3", "mkv"), &t, 0);
        assert_eq!(
            (p.direct, p.container, p.mime.as_str()),
            (false, Container::Ts, "video/vnd.dlna.mpeg-tts")
        );
        assert_eq!((p.video, p.audio), (VideoOut::Copy, AudioOut::Copy));
        assert_eq!(p.summary(), "repackaged, quality untouched");
        assert!(auto(&film("h264", "ac3", "mov,mp4,m4a"), &t, 0).direct);
        assert_eq!(
            auto(&film("h264", "dts", "mkv"), &t, 0).audio,
            AudioOut::Ac3
        );
        // One that says nothing about itself.
        let t = Target { sink: &[], ..t };
        assert!(auto(&film("h264", "ac3", "mkv"), &t, 0).direct);
        let mut song = film("", "alac", "mov,mp4,m4a");
        song.video = None;
        assert_eq!(auto(&song, &t, 0).audio, AudioOut::Mp3);
    }

    #[test]
    fn places() {
        let mut s = film("h264", "aac", "hls");
        s.path = "https://x/live.m3u8".into();
        assert!(!auto(&s, &CC, 0).direct);
        s.path = "https://x/a.mp4".into();
        s.format = "mov,mp4".into();
        assert!(auto(&s, &CC, 0).direct);
        s.stream = "edl://!new_stream".into();
        assert!(auto(&s, &CC, 0).mpv);
        s.path = "bd://1".into();
        assert!(
            plan(&s, &CC, Quality::Auto, false, 0, true)
                .unwrap_err()
                .contains("rip")
        );
    }

    #[test]
    fn keyframes() {
        let k = [10.0, 20.0, 30.0];
        assert_eq!(pick_keyframe(&k, 14.0, 0.0), Some(10.0));
        assert_eq!(pick_keyframe(&k, 16.0, 0.0), Some(20.0));
        // Five seconds on from 10.5 must not end at 10 again.
        assert_eq!(pick_keyframe(&k, 14.5, 10.5), Some(20.0));
        // Back may.
        assert_eq!(pick_keyframe(&k, 11.0, 16.0), Some(10.0));
        assert_eq!(pick_keyframe(&[1000.0], 14.0, 0.0), None);
        assert_eq!(pick_keyframe(&[], 14.0, 0.0), None);
    }

    #[test]
    fn cues() {
        let srt = "\u{feff}1\r\n00:00:05,000 --> 00:00:08,500\r\n<i>Hi</i> & bye\r\nline 2\r\n\r\n2\r\n00:01:00,000 --> 00:01:02,000\r\n{\\an8}Up\r\n";
        let c = parse_cues(srt);
        assert_eq!(c.len(), 2);
        assert_eq!(
            c[0],
            Cue {
                start: 5.0,
                end: 8.5,
                text: "Hi & bye\nline 2".into()
            }
        );
        let vtt = write_cues(&c, 0.0, true);
        assert!(
            vtt.starts_with("WEBVTT\n\n1\n00:00:05.000 --> 00:00:08.500\nHi &amp; bye\nline 2\n"),
            "{vtt}"
        );
        // Started in the middle: what is over is left out.
        let late = write_cues(&c, -30.0, false);
        assert_eq!(late, "1\n00:00:30,000 --> 00:00:32,000\nUp\n\n");
        assert_eq!(
            write_cues(&c, -6.0, false).lines().nth(1),
            Some("00:00:00,000 --> 00:00:02,500")
        );
        assert_eq!(decode(b"caf\xe9"), "café");
        assert_eq!(decode(&[0xFF, 0xFE, b'h', 0, b'i', 0]), "hi");
        assert_eq!(parse_cues(&write_cues(&c, 0.0, false)), c);
    }

    // With real files: needs the FFmpeg programs and libmpv.
    fn sample(dir: &Path) -> Option<Source> {
        let f = dir.join("film.mkv");
        let srt = dir.join("film.srt");
        std::fs::write(&srt, "1\n00:00:02,000 --> 00:00:04,000\nHello\n").ok()?;
        let ok = Command::new(program("ffmpeg")?)
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=640x360:r=25",
                "-f",
                "lavfi",
                "-i",
                "sine=440",
            ])
            .args([
                "-i",
                srt.to_str()?,
                "-t",
                "24",
                "-map",
                "0",
                "-map",
                "1",
                "-map",
                "2",
                "-c:v",
                "libx264",
                "-g",
                "100",
            ])
            .args([
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "ac3",
                "-c:s",
                "srt",
                f.to_str()?,
            ])
            .status()
            .ok()?
            .success();
        let mut s = film("h264", "ac3", "mkv");
        s.path = f.to_str()?.into();
        s.duration = 24.0;
        s.audio.as_mut()?.channels = 1;
        s.sub = Some(SubTrack {
            id: 1,
            index: Some(2),
            codec: "subrip".into(),
            ..Default::default()
        });
        ok.then_some(s)
    }

    fn probe(data: &[u8], dir: &Path, name: &str) -> String {
        let f = dir.join(name);
        std::fs::write(&f, data).unwrap();
        // Decode all of it too: no errors.
        let out = Command::new(program("ffmpeg").unwrap())
            .args([
                "-v",
                "error",
                "-nostdin",
                "-i",
                f.to_str().unwrap(),
                "-f",
                "null",
                "-",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success() && out.stderr.is_empty(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        run_text(Command::new(program("ffprobe").unwrap()).args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name:format=duration",
            "-of",
            "csv=p=0",
            f.to_str().unwrap(),
        ]))
        .unwrap()
        .split_whitespace()
        // MPEG-TS lists its streams twice (per programme, too).
        .fold(Vec::new(), |mut v, w| {
            if !v.contains(&w) {
                v.push(w);
            }
            v
        })
        .join(" ")
    }

    #[test]
    #[ignore]
    fn converts() {
        let dir = std::env::temp_dir().join(format!("smp-conv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = sample(&dir).unwrap();
        let cues = read_cues(&src);
        assert!(
            cues.len() == 1 && cues[0].text == "Hello" && (cues[0].start - 2.0).abs() < 0.05,
            "{cues:?}"
        );
        let key = keyframe_near(&src, 13.0, 0.0).unwrap();
        assert!((key - 12.0).abs() < 0.1, "{key}");
        assert!((keyframe_near(&src, 13.0, 11.8).unwrap() - 16.0).abs() < 0.1);
        let sink = ["video/mpeg".to_owned(), "audio/mpeg".to_owned()];
        let tv = Target {
            kind: Kind::Dlna,
            video: true,
            sink: &sink,
        };
        let speaker = Target {
            kind: Kind::Dlna,
            video: false,
            sink: &sink,
        };
        let mut burnt = src.clone();
        burnt.sub.as_mut().unwrap().codec = "hdmv_pgs".into();
        let mut alone = auto(&src, &CC, 0);
        alone.mpv = true;
        let mut alone_speaker = auto(&src, &speaker, 0);
        alone_speaker.mpv = true;
        for (name, plan, start, want) in [
            ("copy.mp4", auto(&src, &CC, 0), 0.0, "h264 aac 24"),
            ("copy-seek.mp4", auto(&src, &CC, 0), key, "h264 aac 12"),
            ("encode.mp4", auto(&src, &CC, 2), 10.0, "h264 aac 14"),
            ("copy.ts", auto(&src, &tv, 0), key, "h264 ac3 12"),
            ("sound.mp3", auto(&src, &speaker, 0), 4.0, "mp3 20"),
            ("mpv.mp4", alone, 10.0, "h264 aac 14"),
            ("mpv.mp3", alone_speaker, 4.0, "mp3 20"),
            ("burn.ts", auto(&burnt, &tv, 0), 0.0, "h264 aac 24"),
        ] {
            let job = Job {
                source: src.clone(),
                plan,
                start,
            };
            let mut data = Vec::new();
            let mut enc = Encoder::start(&job).unwrap();
            let mut buf = [0u8; 8192];
            loop {
                match enc.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => data.extend_from_slice(&buf[..n]),
                    Err(e) => assert!(
                        matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ),
                        "{e}"
                    ),
                }
            }
            let got = probe(&data, &dir, name);
            println!("{name}: {got}");
            let (codecs, secs) = got.trim().rsplit_once(' ').unwrap();
            let (want_codecs, want_secs) = want.rsplit_once(' ').unwrap();
            // mpv may put the sound first.
            let set = |s: &str| {
                s.split(' ')
                    .map(str::to_owned)
                    .collect::<std::collections::BTreeSet<_>>()
            };
            assert_eq!(set(codecs), set(want_codecs), "{name}");
            assert!(
                (secs.parse::<f64>().unwrap() - want_secs.parse::<f64>().unwrap()).abs() < 0.6,
                "{name}: {secs}"
            );
        }
        // Called off in the middle: it ends at once.
        for mpv in [false, true] {
            let mut plan = auto(&src, &CC, 2);
            plan.mpv = mpv;
            let mut enc = Encoder::start(&Job {
                source: src.clone(),
                plan,
                start: 0.0,
            })
            .unwrap();
            let mut one = [0u8; 100];
            while enc.read(&mut one).is_err() {}
            let t = std::time::Instant::now();
            drop(enc);
            assert!(t.elapsed() < Duration::from_secs(1));
        }
        std::thread::sleep(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(dir);
    }
}
