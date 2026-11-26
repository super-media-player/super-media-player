//! Decode a file's sound to raw 16-bit mono samples with a hidden mpv,
//! as fast as the computer goes (no playing in real time). Used to listen
//! for intros (intro.rs) and to recognise speech (transcribe.rs).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

use player_core::{EndReason, LoadMode, Player, PlayerConfig, PlayerEvent, Property};

/// What to decode.
pub struct Decode<'a> {
    pub source: &'a str,
    pub out: &'a Path,
    pub rate: u32,
    /// Write a WAV header (else headerless samples).
    pub wav: bool,
    pub start: f64,
    /// Seconds (None: to the end).
    pub length: Option<f64>,
    /// Audio track id (None: mpv's choice).
    pub aid: Option<i64>,
}

/// Decode `d` into `d.out`, reporting how far it got (seconds into the
/// file). Removes the output on failure or cancel.
pub fn render(d: &Decode, cancel: &AtomicBool, progress: impl Fn(f64)) -> Result<(), String> {
    let (tx, rx) = channel();
    let mut cfg = PlayerConfig::headless();
    let rate = d.rate.to_string();
    let start = format!("{:.2}", d.start);
    let out = d.out.to_string_lossy();
    let mut opts: Vec<(&str, &str)> = vec![
        ("ao", "pcm"),
        ("ao-pcm-file", &out),
        ("ao-pcm-waveheader", if d.wav { "yes" } else { "no" }),
        ("audio-format", "s16"),
        ("audio-samplerate", &rate),
        ("audio-channels", "mono"),
        ("untimed", "yes"),
        ("vid", "no"),
        ("sid", "no"),
        ("idle", "no"),
        ("start", &start),
        ("load-scripts", "no"),
        ("resume-playback", "no"),
        ("replaygain", "no"),
        ("volume", "100"),
        ("ytdl", "no"),
    ];
    let length = d.length.map(|l| format!("{l:.2}"));
    if let Some(l) = &length {
        opts.push(("length", l));
    }
    let aid = d.aid.map(|a| a.to_string());
    if let Some(a) = &aid {
        opts.push(("aid", a));
    }
    for (k, v) in opts {
        cfg.set(k, v);
    }
    let player = Player::new(&cfg, move |e| {
        let _ = tx.send(e);
    })
    .map_err(|e| e.to_string())?;
    player
        .load(d.source, LoadMode::Replace)
        .map_err(|e| e.to_string())?;
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = player.stop();
            break Err("Cancelled".to_owned());
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(PlayerEvent::Property(Property::TimePos(Some(t)))) => progress(t),
            Ok(PlayerEvent::EndFile {
                reason: EndReason::Eof,
                ..
            }) => break Ok(()),
            Ok(PlayerEvent::EndFile { error, .. }) => {
                break Err(error.unwrap_or_else(|| "the file could not be read".into()));
            }
            Ok(PlayerEvent::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                break Err("the decoder stopped".into());
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
        }
    };
    drop(player);
    if result.is_err() {
        let _ = std::fs::remove_file(d.out);
    }
    result
}

/// Decode to memory: mono 16-bit samples.
pub fn samples(source: &Path, rate: u32, start: f64, length: f64) -> Option<Vec<i16>> {
    let dir = crate::settings::cache_dir()?.join("pcm");
    std::fs::create_dir_all(&dir).ok()?;
    let out = dir.join(format!(
        "{}-{:?}.raw",
        std::process::id(),
        std::thread::current().id()
    ));
    let d = Decode {
        source: &source.to_string_lossy(),
        out: &out,
        rate,
        wav: false,
        start,
        length: Some(length),
        aid: None,
    };
    render(&d, &AtomicBool::new(false), |_| {}).ok()?;
    let bytes = std::fs::read(&out).ok();
    let _ = std::fs::remove_file(&out);
    Some(
        bytes?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect(),
    )
}
