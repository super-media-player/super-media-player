//! Headless playback tests against real libmpv.
//!
//! Sample media is generated with the `ffmpeg` CLI. Tests are skipped (pass with
//! a message) when libmpv or ffmpeg is not available.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use player_core::{
    EndReason, LoadMode, Player, PlayerConfig, PlayerEvent, Property, Seek, TrackKind,
};

fn fixtures_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fixtures");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Generate `name` with ffmpeg `args` unless it exists. Returns None without ffmpeg.
fn fixture(name: &str, args: &[&str]) -> Option<PathBuf> {
    let path = fixtures_dir().join(name);
    if path.exists() {
        return Some(path);
    }
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .arg(&path)
        .status()
        .ok()?;
    status.success().then_some(path)
}

fn audio_fixture() -> Option<PathBuf> {
    fixture(
        "tone.ogg",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=3",
            "-metadata",
            "title=Test Tone",
            "-metadata",
            "artist=Unit Test",
            "-c:a",
            "libvorbis",
        ],
    )
}

fn video_fixture() -> Option<PathBuf> {
    fixture(
        "clip.mkv",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=duration=2",
            "-shortest",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ],
    )
}

fn start() -> Option<(Player, Receiver<PlayerEvent>)> {
    let (tx, rx) = channel();
    match Player::new(&PlayerConfig::headless(), move |e| {
        let _ = tx.send(e);
    }) {
        Ok(p) => Some((p, rx)),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

/// Wait for an event matching `pred`, failing after a timeout.
fn wait_for(
    rx: &Receiver<PlayerEvent>,
    what: &str,
    mut pred: impl FnMut(&PlayerEvent) -> bool,
) -> PlayerEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(e) if pred(&e) => return e,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    panic!("timed out waiting for {what}");
}

fn path_str(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn plays_audio_file_and_reports_metadata() {
    let Some(file) = audio_fixture() else {
        return eprintln!("skipping: no ffmpeg");
    };
    let Some((player, rx)) = start() else { return };

    player.load(path_str(&file), LoadMode::Replace).unwrap();
    wait_for(&rx, "file-loaded", |e| matches!(e, PlayerEvent::FileLoaded));

    let dur = wait_for(&rx, "duration", |e| {
        matches!(e, PlayerEvent::Property(Property::Duration(Some(_))))
    });
    let PlayerEvent::Property(Property::Duration(Some(d))) = dur else {
        unreachable!()
    };
    assert!((d - 3.0).abs() < 0.2, "duration was {d}");

    let meta = wait_for(
        &rx,
        "metadata",
        |e| matches!(e, PlayerEvent::Property(Property::Metadata(m)) if !m.is_empty()),
    );
    let PlayerEvent::Property(Property::Metadata(m)) = meta else {
        unreachable!()
    };
    assert!(m.contains(&("artist".into(), "Unit Test".into())), "{m:?}");

    let tracks = player.get_value("track-list").expect("track-list");
    assert_eq!(tracks.as_array().len(), 1);
    assert_eq!(
        player.get_string("media-title").as_deref(),
        Some("Test Tone")
    );
}

#[test]
fn reports_video_and_audio_tracks() {
    let Some(file) = video_fixture() else {
        return eprintln!("skipping: no ffmpeg");
    };
    let Some((player, rx)) = start() else { return };

    player.load(path_str(&file), LoadMode::Replace).unwrap();
    let ev = wait_for(
        &rx,
        "track list",
        |e| matches!(e, PlayerEvent::Property(Property::Tracks(t)) if t.len() == 2),
    );
    let PlayerEvent::Property(Property::Tracks(tracks)) = ev else {
        unreachable!()
    };
    let video = tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .expect("video track");
    assert_eq!((video.width, video.height), (Some(320), Some(240)));
    assert_eq!(video.codec.as_deref(), Some("h264"));
    assert!(tracks.iter().any(|t| t.kind == TrackKind::Audio));
}

#[test]
fn seeks_pauses_and_reaches_end() {
    let Some(file) = audio_fixture() else {
        return eprintln!("skipping: no ffmpeg");
    };
    let Some((player, rx)) = start() else { return };

    player.load(path_str(&file), LoadMode::Replace).unwrap();
    wait_for(&rx, "file-loaded", |e| matches!(e, PlayerEvent::FileLoaded));

    player.set_pause(true).unwrap();
    wait_for(&rx, "pause", |e| {
        matches!(e, PlayerEvent::Property(Property::Pause(true)))
    });

    player.seek(Seek::Absolute(2.5)).unwrap();
    wait_for(&rx, "playback restart", |e| {
        matches!(e, PlayerEvent::PlaybackRestart)
    });
    let pos = player.get_double("time-pos").unwrap();
    assert!((pos - 2.5).abs() < 0.3, "position was {pos}");

    player.set_pause(false).unwrap();
    let end = wait_for(&rx, "end of file", |e| {
        matches!(e, PlayerEvent::EndFile { .. })
    });
    assert_eq!(
        end,
        PlayerEvent::EndFile {
            reason: EndReason::Eof,
            error: None
        }
    );
}

#[test]
fn playlist_append_and_navigation() {
    let (Some(a), Some(v)) = (audio_fixture(), video_fixture()) else {
        return eprintln!("skipping: no ffmpeg");
    };
    let Some((player, rx)) = start() else { return };

    player.set_pause(true).unwrap();
    player.load(path_str(&a), LoadMode::Append).unwrap();
    player.load(path_str(&v), LoadMode::Append).unwrap();
    wait_for(
        &rx,
        "playlist of 2",
        |e| matches!(e, PlayerEvent::Property(Property::Playlist(p)) if p.len() == 2),
    );

    player.play_index(1).unwrap();
    wait_for(&rx, "pos 1", |e| {
        matches!(e, PlayerEvent::Property(Property::PlaylistPos(Some(1))))
    });
    player.prev().unwrap();
    wait_for(&rx, "pos 0", |e| {
        matches!(e, PlayerEvent::Property(Property::PlaylistPos(Some(0))))
    });

    player.remove_index(1).unwrap();
    wait_for(
        &rx,
        "playlist of 1",
        |e| matches!(e, PlayerEvent::Property(Property::Playlist(p)) if p.len() == 1),
    );
}

#[test]
fn missing_file_reports_error() {
    let Some((player, rx)) = start() else { return };
    player
        .load("/definitely/not/here.mp3", LoadMode::Replace)
        .unwrap();
    let end = wait_for(&rx, "end-file", |e| {
        matches!(e, PlayerEvent::EndFile { .. })
    });
    assert!(
        matches!(
            end,
            PlayerEvent::EndFile {
                reason: EndReason::Error,
                error: Some(_)
            }
        ),
        "{end:?}"
    );
}

/// Seek-bar thumbnails come from a second, hidden player: with no video
/// output (`vo=null`), `screenshot-raw` must still hand back the frame.
#[test]
fn hidden_player_returns_raw_frames() {
    let Some(clip) = video_fixture() else { return };
    let (tx, rx) = channel();
    let mut cfg = PlayerConfig::headless();
    cfg.set("pause", "yes")
        .set("aid", "no")
        .set("sid", "no")
        .set("hwdec", "no")
        .set("vf", "scale=w=160:h=-2");
    let player = match Player::new(&cfg, move |e| {
        let _ = tx.send(e);
    }) {
        Ok(p) => p,
        Err(e) => return eprintln!("skipping: {e}"),
    };
    player.load(path_str(&clip), LoadMode::Replace).unwrap();
    wait_for(&rx, "first frame", |e| {
        matches!(e, PlayerEvent::PlaybackRestart)
    });
    player
        .command(&["seek", "1.2", "absolute+keyframes"])
        .unwrap();
    wait_for(&rx, "seek", |e| matches!(e, PlayerEvent::PlaybackRestart));
    let shot = player.command_ret(&["screenshot-raw", "video"]).unwrap();
    let w = shot.get("w").and_then(|v| v.as_i64()).unwrap();
    let h = shot.get("h").and_then(|v| v.as_i64()).unwrap();
    let stride = shot.get("stride").and_then(|v| v.as_i64()).unwrap();
    let data = shot.get("data").and_then(|v| v.as_bytes()).unwrap();
    assert_eq!((w, h), (160, 120));
    assert_eq!(shot.str_field("format").as_deref(), Some("bgr0"));
    assert!(data.len() >= (stride * h) as usize);
}

/// Clip export runs mpv's encoder (`o=`) in a hidden player: a part of the
/// file between two times is written to a new file.
#[test]
fn hidden_player_encodes_a_clip() {
    let Some(clip) = video_fixture() else { return };
    let out = fixtures_dir().join("cut.mp4");
    let _ = std::fs::remove_file(&out);
    let (tx, rx) = channel();
    let mut cfg = PlayerConfig::headless();
    cfg.set("idle", "no")
        .set("o", out.to_str().unwrap())
        .set("of", "mp4")
        .set("ovc", "libx264")
        .set("ovcopts", "preset=veryfast,crf=20")
        .set("oac", "aac")
        .set("start", "0.5")
        .set("end", "1.5")
        .set("hwdec", "no");
    cfg.options.retain(|(k, _)| k != "vo" && k != "ao");
    let player = match Player::new(&cfg, move |e| {
        let _ = tx.send(e);
    }) {
        Ok(p) => p,
        Err(e) => return eprintln!("skipping: {e}"),
    };
    player.load(path_str(&clip), LoadMode::Replace).unwrap();
    let end = wait_for(&rx, "end", |e| matches!(e, PlayerEvent::EndFile { .. }));
    assert!(
        matches!(
            end,
            PlayerEvent::EndFile {
                reason: EndReason::Eof,
                ..
            }
        ),
        "{end:?}"
    );
    drop(player);
    let len = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    assert!(len > 1000, "clip is {len} bytes");
}
