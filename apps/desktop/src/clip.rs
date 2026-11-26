//! Save a part of the playing file as its own file: an MP4, an animated GIF
//! or an MP3.
//!
//! A hidden mpv instance in encoding mode (`o=`) plays the part between the
//! two marks as fast as it can and writes it out. No FFmpeg program is
//! needed: mpv's own FFmpeg libraries do the encoding.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use player_core::{EndReason, LoadMode, Player, PlayerConfig, PlayerEvent, Property};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    Mp4,
    Gif,
    Mp3,
    /// Lossless audio.
    Flac,
    /// Small, good audio.
    Opus,
}

impl Format {
    fn ext(self) -> &'static str {
        match self {
            Format::Mp4 => "mp4",
            Format::Gif => "gif",
            Format::Mp3 => "mp3",
            Format::Flac => "flac",
            Format::Opus => "opus",
        }
    }
}

/// What to cut.
#[derive(Clone, Debug)]
pub struct Job {
    pub source: String,
    pub start: f64,
    pub end: f64,
    pub format: Format,
    /// The audio and video track ids playing now (None: mpv's choice).
    pub aid: Option<i64>,
    pub vid: Option<i64>,
    pub out: PathBuf,
    /// More mpv options (the CD ripper's drive and error correction).
    pub extra: Vec<(String, String)>,
}

/// mpv options for encoding `job`.
fn options(job: &Job) -> Vec<(String, String)> {
    let mut o: Vec<(&str, String)> = vec![
        ("o", job.out.to_string_lossy().into_owned()),
        ("start", format!("{:.3}", job.start)),
        ("end", format!("{:.3}", job.end)),
        ("idle", "no".into()),
        ("hwdec", "no".into()),
        ("sid", "no".into()),
        ("ytdl", "no".into()),
        ("load-scripts", "no".into()),
        ("resume-playback", "no".into()),
        ("audio-display", "no".into()),
    ];
    if let Some(a) = job.aid {
        o.push(("aid", a.to_string()));
    }
    if let Some(v) = job.vid {
        o.push(("vid", v.to_string()));
    }
    match job.format {
        Format::Mp4 => {
            o.push(("of", "mp4".into()));
            o.push(("ovc", "libx264".into()));
            o.push(("ovcopts", "preset=veryfast,crf=20,pix_fmt=yuv420p".into()));
            o.push(("oac", "aac".into()));
            o.push(("oacopts", "b=192k".into()));
            // Phones and web pages play it while it downloads.
            o.push(("ofopts", "movflags=+faststart".into()));
        }
        Format::Gif => {
            o.push(("of", "gif".into()));
            o.push(("ovc", "gif".into()));
            o.push(("aid", "no".into()));
            // A palette made from the clip itself looks far better than
            // the generic one.
            o.push((
                "vf",
                "lavfi=[fps=12,scale=480:-2:flags=lanczos,split[a][b];\
                 [a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4]"
                    .into(),
            ));
        }
        Format::Mp3 => {
            o.push(("of", "mp3".into()));
            o.push(("vid", "no".into()));
            o.push(("oac", "libmp3lame".into()));
            o.push(("oacopts", "b=192k".into()));
        }
        Format::Flac => {
            o.push(("of", "flac".into()));
            o.push(("vid", "no".into()));
            o.push(("oac", "flac".into()));
        }
        Format::Opus => {
            o.push(("of", "ogg".into()));
            o.push(("vid", "no".into()));
            o.push(("oac", "libopus".into()));
            o.push(("oacopts", "b=128k".into()));
        }
    }
    o.into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .chain(job.extra.iter().cloned())
        .collect()
}

/// Run `job`, reporting progress (0..1). Blocks; run it on a worker thread.
/// Leaves no partial file behind when it fails or is cancelled.
pub fn run(job: &Job, cancel: &AtomicBool, progress: impl Fn(f32)) -> Result<PathBuf, String> {
    let (tx, rx) = channel();
    let mut cfg = PlayerConfig::headless();
    // Encoding mode brings its own outputs.
    cfg.options.retain(|(k, _)| k != "vo" && k != "ao");
    for (k, v) in options(job) {
        cfg.set(&k, &v);
    }
    let player = Player::new(&cfg, move |e| {
        let _ = tx.send(e);
    })
    .map_err(|e| e.to_string())?;
    player
        .load(&job.source, LoadMode::Replace)
        .map_err(|e| e.to_string())?;
    let length = (job.end - job.start).max(0.01);
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = player.stop();
            break Err("Cancelled".to_owned());
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(PlayerEvent::Property(Property::TimePos(Some(t)))) => {
                progress((((t - job.start) / length).clamp(0.0, 1.0)) as f32);
            }
            Ok(PlayerEvent::EndFile {
                reason: EndReason::Eof,
                ..
            }) => break Ok(()),
            Ok(PlayerEvent::EndFile { reason, error }) => {
                break Err(error.unwrap_or_else(|| format!("{reason:?}")));
            }
            Ok(PlayerEvent::Shutdown) => break Err("the encoder stopped".to_owned()),
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break Err("the encoder stopped".to_owned()),
        }
    };
    // Dropping the player finishes the file (writes its index).
    drop(player);
    match result {
        Ok(()) if std::fs::metadata(&job.out).is_ok_and(|m| m.len() > 0) => Ok(job.out.clone()),
        Ok(()) => {
            let _ = std::fs::remove_file(&job.out);
            Err("nothing was written".to_owned())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&job.out);
            Err(e)
        }
    }
}

/// The user's Videos folder (XDG), else their home folder.
pub fn videos_dir() -> PathBuf {
    user_dir("XDG_VIDEOS_DIR", "Videos")
}

/// An XDG user folder (`key` from ~/.config/user-dirs.dirs), else
/// ~/`fallback`, else the home folder.
pub fn user_dir(key: &str, fallback: &str) -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let prefix = format!("{key}=");
    let from_xdg = std::fs::read_to_string(home.join(".config/user-dirs.dirs"))
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix(prefix.as_str()))
                .map(|v| {
                    v.trim_matches('"')
                        .replace("$HOME", &home.to_string_lossy())
                })
        })
        .map(PathBuf::from);
    from_xdg
        .or_else(|| Some(home.join(fallback)))
        .filter(|d| d.is_dir())
        .unwrap_or(home)
}

/// A file name for the clip that doesn't overwrite anything: the title, the
/// start time, the format's extension.
pub fn output_path(dir: &Path, title: &str, start: f64, format: Format) -> PathBuf {
    let clean: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let clean = clean.trim().trim_matches('.');
    let clean = if clean.is_empty() { "clip" } else { clean };
    let t = start.max(0.0) as u64;
    let stamp = format!("{}-{:02}-{:02}", t / 3600, t / 60 % 60, t % 60);
    let ext = format.ext();
    let mut path = dir.join(format!("{clean} clip {stamp}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{clean} clip {stamp} ({n}).{ext}"));
        n += 1;
    }
    path
}

/// A shared cancel flag.
pub fn cancel_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("smp-clip-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = output_path(&dir, "My/Film: part 1", 3725.0, Format::Mp4);
        assert_eq!(a.file_name().unwrap(), "My_Film_ part 1 clip 1-02-05.mp4");
        std::fs::write(&a, b"x").unwrap();
        let b = output_path(&dir, "My/Film: part 1", 3725.0, Format::Mp4);
        assert!(b.to_string_lossy().ends_with("(2).mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End to end: a GIF, MP3, FLAC and Opus from a generated clip. Skipped without
    /// libmpv or ffmpeg.
    #[test]
    fn saves_gif_and_mp3() {
        let dir = std::env::temp_dir().join(format!("smp-clip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mkv");
        let made = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=25:duration=3",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=duration=3",
                "-shortest",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
            ])
            .arg(&src)
            .status()
            .is_ok_and(|s| s.success());
        if !made || player_core::Player::new(&PlayerConfig::headless(), |_| {}).is_err() {
            eprintln!("ffmpeg or libmpv not available; skipping");
            return;
        }
        for format in [Format::Gif, Format::Mp3, Format::Flac, Format::Opus] {
            let job = Job {
                source: src.to_string_lossy().into_owned(),
                start: 0.5,
                end: 2.0,
                format,
                aid: None,
                vid: None,
                out: dir.join(format!("out.{}", format.ext())),
                extra: Vec::new(),
            };
            let out = run(&job, &AtomicBool::new(false), |_| {}).unwrap();
            let head = std::fs::read(&out).unwrap();
            match format {
                Format::Gif => assert!(head.starts_with(b"GIF89a"), "not a GIF"),
                Format::Flac => assert!(head.starts_with(b"fLaC"), "not a FLAC"),
                Format::Opus => assert!(
                    head.starts_with(b"OggS") && head.windows(8).any(|w| w == b"OpusHead"),
                    "not Opus"
                ),
                _ => assert!(head.starts_with(b"ID3") || head[0] == 0xff, "not an MP3"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
