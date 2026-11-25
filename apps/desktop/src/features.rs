//! Wiring for the features beyond plain playback: seek bar previews, the
//! command palette, the signal path, clips, intros, generated subtitles,
//! the phone remote, watching together, MilkDrop and video enhancement.
//! Each lives in its own module; this file connects them to the UI.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use player_core::Player;
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::settings::Settings;
use crate::{
    AppWindow, SeekMark, SignalStage, TranscriptLine, clip, enhance, intro, palette, party, remote, signal, silence, sponsor, subsync,
    thumbs, transcribe,
};

thread_local! {
    static CLIP_CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static TRANSCRIBE_CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    /// The current file's subtitles as text, for searching, and the query.
    static TRANSCRIPT: RefCell<(Vec<transcribe::Line>, String)> = const { RefCell::new((Vec::new(), String::new())) };
}

pub fn install(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    ui.set_milkdrop_available(crate::milkdrop::available());
    palette::install(ui);
    install_thumbnails(ui, player, settings.clone());
    install_signal_path(ui, player, settings.clone());
    install_clips(ui, player);
    install_intros(ui, player, settings.clone());
    install_transcripts(ui, player, settings.clone());
    install_subsync(ui, player, settings.clone());
    install_remote(ui, player);
    install_party(ui, player);
    install_enhance(ui, player, settings.clone());
    crate::navigation::install(ui, player, settings.clone());
}

/// The current file's path, if a local file is playing.
fn local_path(player: &Player) -> Option<String> {
    player.get_string("path").filter(|p| thumbs::previewable(p))
}

// ---- seek bar previews ----

fn install_thumbnails(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    let weak = ui.as_weak();
    let thumbs = Rc::new(RefCell::new(thumbs::Thumbnailer::new(move |t| {
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_thumb_image(slint::Image::from_rgba8(t.pixels));
            ui.set_has_thumb(true);
            log::trace!("thumbnail at {:.1}", t.time);
        });
    })));
    {
        let (p, thumbs, weak) = (player.clone(), thumbs.clone(), ui.as_weak());
        ui.on_hover_seek(move |t| {
            let Some(ui) = weak.upgrade() else { return };
            if !settings.borrow().seek_thumbnails || !ui.get_has_video() {
                return;
            }
            let Some(path) = local_path(&p) else { return };
            let mut th = thumbs.borrow_mut();
            th.open(Some(path));
            th.request(f64::from(t));
        });
    }
    // A new file: forget the old file's pictures (and its hidden player).
    let (p, weak) = (player.clone(), ui.as_weak());
    let last = RefCell::new(None::<String>);
    every(ui, Duration::from_millis(400), move || {
        let Some(ui) = weak.upgrade() else { return };
        let path = p.get_string("path");
        if *last.borrow() != path {
            last.replace(path);
            ui.set_has_thumb(false);
            thumbs.borrow_mut().open(None);
        }
    });
}

// ---- signal path ----

fn install_signal_path(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    let (p, weak) = (player.clone(), ui.as_weak());
    let show = Rc::new(move || {
        let Some(ui) = weak.upgrade() else { return };
        if !ui.get_signal_open() {
            return;
        }
        let r = signal::report(&signal::gather(&p, settings.borrow().bit_perfect));
        let stages: Vec<SignalStage> = r
            .stages
            .into_iter()
            .map(|s| SignalStage { title: s.title.into(), detail: s.detail.into(), level: s.level })
            .collect();
        ui.set_signal_stages(ModelRc::new(VecModel::from(stages)));
        ui.set_signal_verdict(r.verdict.into());
        ui.set_signal_level(r.level);
        ui.set_signal_can_fix(r.can_fix_rates);
    });
    let tick = show.clone();
    every(ui, Duration::from_millis(1500), move || tick());
    let weak = ui.as_weak();
    ui.on_signal_fix_rates(move || {
        let Some(ui) = weak.upgrade() else { return };
        match signal::allow_rates() {
            Ok(msg) => ui.set_toast(msg.into()),
            Err(e) => ui.set_toast(format!("Couldn't change PipeWire's rates: {e}").into()),
        }
        show();
    });
}

// ---- clips ----

fn install_clips(ui: &AppWindow, player: &Rc<Player>) {
    // Opening the panel: start from the A-B marks when there are some,
    // else the next 10 seconds.
    let (p, weak) = (player.clone(), ui.as_weak());
    let was_open = Cell::new(false);
    every(ui, Duration::from_millis(200), move || {
        let Some(ui) = weak.upgrade() else { return };
        let open = ui.get_clip_open();
        if open && !was_open.get() && !ui.get_clip_working() {
            let pos = f64::from(ui.get_position());
            let (a, b) = (p.get_double("ab-loop-a"), p.get_double("ab-loop-b"));
            let (start, end) = match (a, b) {
                (Some(a), Some(b)) if b > a => (a, b),
                (Some(a), None) => (a, pos.max(a + 1.0)),
                _ => (pos, (pos + 10.0).min(f64::from(ui.get_duration()).max(pos))),
            };
            ui.set_clip_start(start as f32);
            ui.set_clip_end(end as f32);
            ui.set_clip_status("".into());
        }
        was_open.set(open);
    });

    let (p, weak) = (player.clone(), ui.as_weak());
    ui.on_clip_save(move || {
        let Some(ui) = weak.upgrade() else { return };
        let Some(source) = p.get_string("path").filter(|s| !s.is_empty()) else {
            ui.set_clip_status("Nothing is playing".into());
            return;
        };
        // The format lists in ClipPanel: video files offer two more.
        let audio = [clip::Format::Mp3, clip::Format::Flac, clip::Format::Opus];
        let i = ui.get_clip_format().max(0) as usize;
        let format = if ui.get_has_video() {
            match i {
                0 => clip::Format::Mp4,
                1 => clip::Format::Gif,
                n => audio.get(n - 2).copied().unwrap_or(clip::Format::Mp3),
            }
        } else {
            audio.get(i).copied().unwrap_or(clip::Format::Mp3)
        };
        let (start, end) = (f64::from(ui.get_clip_start()), f64::from(ui.get_clip_end()));
        let title = ui.get_media_title().to_string();
        let out = clip::output_path(&clip::videos_dir(), &title, start, format);
        let id = |name: &str| p.get_string(name).and_then(|v| v.parse::<i64>().ok());
        let job = clip::Job { source, start, end, format, aid: id("aid"), vid: id("vid"), out, extra: Vec::new() };
        let cancel = clip::cancel_flag();
        CLIP_CANCEL.with_borrow_mut(|c| *c = Some(cancel.clone()));
        ui.set_clip_working(true);
        ui.set_clip_progress(0.0);
        ui.set_clip_status(format!("Saving {}…", job.out.display()).into());
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let w = weak.clone();
            let result = clip::run(&job, &cancel, move |f| {
                let _ = w.upgrade_in_event_loop(move |ui| ui.set_clip_progress(f));
            });
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_clip_working(false);
                ui.set_clip_progress(-1.0);
                ui.set_clip_status(
                    match result {
                        Ok(path) => format!("Saved {}", path.display()),
                        Err(e) if e == "Cancelled" => "Cancelled".to_owned(),
                        Err(e) => format!("Couldn't save the clip: {e}"),
                    }
                    .into(),
                );
            });
        });
    });
    ui.on_clip_cancel(|| {
        CLIP_CANCEL.with_borrow(|c| {
            if let Some(c) = c {
                c.store(true, Ordering::Relaxed);
            }
        });
    });
    let weak = ui.as_weak();
    ui.on_clip_open_folder(move || {
        if let Err(e) = crate::open_folder(&clip::videos_dir())
            && let Some(ui) = weak.upgrade()
        {
            ui.set_toast(format!("Couldn't open the folder: {e}").into());
        }
    });
}

// ---- intros, credits, next episode ----

/// What is known about the current file's intro and credits.
#[derive(Default)]
struct IntroState {
    path: Option<String>,
    marks: Option<intro::Marks>,
    /// Looking for them (or looked) for this path.
    asked: bool,
    /// The next episode was queued for this path (or there is none).
    queued: bool,
    /// Auto mode skipped this file's intro already (seeking back into it
    /// then means wanting to see it).
    auto_skipped: bool,
}

/// The sponsors and the like marked in the current video (sponsor.rs).
#[derive(Default)]
struct SponsorState {
    path: Option<String>,
    /// Asked SponsorBlock about this path (or it isn't a YouTube video).
    asked: bool,
    segments: Vec<sponsor::Segment>,
    /// What to skip of them: worked out for this level and duration.
    skips: Vec<sponsor::Skip>,
    made_for: (String, f64),
    /// Where skips that happened by themselves started (seeking back into
    /// one then means wanting to see it).
    done: Vec<f64>,
    /// The position at the last look.
    last_pos: f64,
}

thread_local! {
    static SPONSOR: RefCell<SponsorState> = RefCell::new(SponsorState::default());
    static INTRO: RefCell<IntroState> = RefCell::new(IntroState::default());
}

/// Seconds the "Next episode" button counts down in auto mode.
const NEXT_COUNTDOWN: f64 = 10.0;

fn install_intros(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    let (p, weak) = (player.clone(), ui.as_weak());
    let st = settings.clone();
    every(ui, Duration::from_millis(250), move || {
        let Some(ui) = weak.upgrade() else { return };
        let mode = st.borrow().skip_intro.clone();
        let path = p.get_string("path").filter(|s| !s.is_empty());
        let duration = p.get_double("duration").unwrap_or(0.0);
        let video = ui.get_has_video();
        INTRO.with_borrow_mut(|s| {
            if s.path != path {
                *s = IntroState { path: path.clone(), ..Default::default() };
            }
        });
        let sponsor = sponsors(&ui, &p, &st.borrow(), path.as_deref(), duration);
        let Some(path) = path.filter(|_| video && duration > 0.0) else {
            show_skip(&ui, sponsor.as_deref(), -1.0);
            return;
        };
        let local = thumbs::previewable(&path);

        // Queue the next episode after the last one in the playlist.
        let queue = INTRO.with_borrow_mut(|s| !std::mem::replace(&mut s.queued, true));
        if queue && local && st.borrow().next_episode {
            let count = p.get_double("playlist-count").unwrap_or(0.0) as i64;
            let pos = p.get_double("playlist-pos").unwrap_or(-1.0) as i64;
            if pos == count - 1
                && let Some(next) = intro::next_episode(std::path::Path::new(&path))
            {
                log::info!("next episode queued: {}", next.display());
                let _ = p.load(&next.to_string_lossy(), player_core::LoadMode::Append);
            }
        }

        // Find the intro: chapters first, else by listening (worker thread).
        let ask = INTRO.with_borrow_mut(|s| !std::mem::replace(&mut s.asked, true));
        if ask && mode != "off" {
            let chapters = p.get_value("chapter-list").unwrap_or_default();
            let marks = intro::from_chapters(&chapters, duration);
            if marks != intro::Marks::default() {
                INTRO.with_borrow_mut(|s| s.marks = Some(marks));
            } else if local && intro::available() {
                let weak = ui.as_weak();
                let path2 = path.clone();
                std::thread::spawn(move || {
                    let marks = intro::detect(std::path::Path::new(&path2), duration);
                    let _ = weak.upgrade_in_event_loop(move |_| {
                        INTRO.with_borrow_mut(|s| {
                            if s.path.as_deref() == Some(path2.as_str()) {
                                s.marks = marks;
                            }
                        });
                    });
                });
            }
        }

        let pos = crate::cast::position(&p).unwrap_or(0.0);
        let marks = INTRO.with_borrow(|s| s.marks).filter(|_| mode != "off");
        let has_next = {
            let count = p.get_double("playlist-count").unwrap_or(0.0);
            let i = p.get_double("playlist-pos").unwrap_or(0.0);
            i + 1.0 < count
        };
        let in_intro = marks.and_then(|m| m.intro).filter(|(a, b)| pos >= a - 0.5 && pos < b - 1.0);
        let in_credits = marks.and_then(|m| m.credits).filter(|c| pos >= *c && has_next);
        if sponsor.is_some() {
            show_skip(&ui, sponsor.as_deref(), -1.0);
        } else if let Some((_, end)) = in_intro {
            let skip_now = mode == "auto" && INTRO.with_borrow_mut(|s| !std::mem::replace(&mut s.auto_skipped, true));
            if skip_now {
                let _ = p.seek(player_core::Seek::Absolute(end));
                ui.set_toast("Intro skipped".into());
            }
            show_skip(&ui, Some("Skip intro"), -1.0);
        } else if let Some(c) = in_credits {
            let left = if mode == "auto" { NEXT_COUNTDOWN - (pos - c) } else { -1.0 };
            if mode == "auto" && left <= 0.0 {
                let _ = p.next();
            }
            show_skip(&ui, Some("Next episode"), left.max(-1.0));
        } else {
            show_skip(&ui, None, -1.0);
        }
        let (a, b) = marks.and_then(|m| m.intro).unwrap_or((-1.0, -1.0));
        if (ui.get_intro_start() - a as f32).abs() > 0.01 {
            ui.set_intro_start(a as f32);
            ui.set_intro_end(b as f32);
        }
    });

    let p = player.clone();
    ui.on_skip_pressed(move || {
        let pos = crate::cast::position(&p).unwrap_or(0.0);
        let marks = INTRO.with_borrow(|s| s.marks);
        let sponsor = SPONSOR.with_borrow(|s| sponsor::at(&s.skips, pos).map(|i| s.skips[i]));
        if let Some(s) = sponsor {
            skip_to_end_of(&p, s);
        } else if let Some((_, end)) = marks.and_then(|m| m.intro).filter(|(a, b)| pos >= a - 0.5 && pos < *b) {
            let _ = p.seek(player_core::Seek::Absolute(end));
        } else if marks.and_then(|m| m.credits).is_some_and(|c| pos >= c) {
            let _ = p.next();
        }
    });
}

fn show_skip(ui: &AppWindow, text: Option<&str>, countdown: f64) {
    let text = text.unwrap_or("");
    if ui.get_skip_text() != text {
        ui.set_skip_text(text.into());
    }
    let c = if countdown >= 0.0 { countdown as f32 } else { -1.0 };
    if (ui.get_skip_countdown() - c).abs() > 0.05 {
        ui.set_skip_countdown(c);
    }
}

// ---- sponsors ----

/// Look for sponsors in the video playing, skip them when that is wanted.
/// Returns the Skip button's text while one plays.
fn sponsors(ui: &AppWindow, p: &Player, settings: &Settings, path: Option<&str>, duration: f64) -> Option<String> {
    let mode = settings.sponsor_skip.as_str();
    let pos = crate::cast::position(p).unwrap_or(0.0);
    SPONSOR.with_borrow_mut(|s| {
        if s.path.as_deref() != path {
            *s = SponsorState { path: path.map(str::to_owned), last_pos: pos, ..Default::default() };
        }
        if mode != "off" && duration > 0.0 && !std::mem::replace(&mut s.asked, true)
            && let Some(path) = path
            && let Some(id) = youtube_id(p, path)
        {
            let (weak, path) = (ui.as_weak(), path.to_owned());
            std::thread::spawn(move || {
                let Ok(found) = sponsor::fetch(&id) else {
                    log::info!("SponsorBlock couldn't be reached");
                    return;
                };
                log::info!("SponsorBlock: {} segments", found.len());
                let _ = weak.upgrade_in_event_loop(move |_| {
                    SPONSOR.with_borrow_mut(|s| {
                        if s.path.as_deref() == Some(path.as_str()) {
                            s.segments = found;
                            s.made_for = Default::default();
                        }
                    });
                });
            });
        }

        // What to skip, and its marks on the seek bar.
        let level = if mode == "off" { "" } else { settings.sponsor_kinds.as_str() };
        if s.made_for.0 != level || (s.made_for.1 - duration).abs() > 0.01 {
            s.made_for = (level.to_owned(), duration);
            s.skips = if level.is_empty() { Vec::new() } else { sponsor::skips(&s.segments, level, duration) };
            let marks: Vec<SeekMark> = s
                .skips
                .iter()
                .map(|k| SeekMark { start: k.start as f32, end: k.end as f32, main: k.kind.in_level("promotion") })
                .collect();
            ui.set_sponsor_marks(ModelRc::new(VecModel::from(marks)));
        }

        let last = std::mem::replace(&mut s.last_pos, pos);
        let now = s.skips[sponsor::at(&s.skips, pos)?];
        // By itself only when playing runs into it: not while seeking
        // around, and not while looking closely (advanced navigation).
        if mode == "auto"
            && !s.done.contains(&now.start)
            && !settings.nav_enabled
            && !ui.get_paused()
            && (sponsor::flowed(last, pos, p.get_double("speed").unwrap_or(1.0)) || (pos > last && silence::jumps(p)))
        {
            s.done.push(now.start);
            s.last_pos = now.end;
            skip_to_end_of(p, now);
            let name = now.kind.name();
            let secs = (now.end - pos).round() as i64;
            ui.set_toast(format!("{}{} skipped ({}:{:02})", name[..1].to_uppercase(), &name[1..], secs / 60, secs % 60).into());
            return None;
        }
        Some(format!("Skip {}", now.kind.name()))
    })
}

/// The YouTube video that `path` is, or was downloaded from.
fn youtube_id(p: &Player, path: &str) -> Option<String> {
    if !thumbs::previewable(path) {
        return sponsor::id_in_link(path).map(str::to_owned);
    }
    if let Some(id) = sponsor::id_in_name(path) {
        return Some(id.to_owned());
    }
    // yt-dlp keeps the link in the tags (--embed-metadata).
    ["purl", "comment"].iter().find_map(|tag| {
        let v = p.get_string(&format!("metadata/by-key/{tag}"))?;
        sponsor::id_in_link(&v).map(str::to_owned)
    })
}

/// Jump past a sponsor. One that lasts until the end ends the video.
fn skip_to_end_of(p: &Player, s: sponsor::Skip) {
    let duration = p.get_double("duration").unwrap_or(f64::MAX);
    let _ = p.seek(player_core::Seek::Absolute(s.end.min(duration)));
}

// ---- lining subtitles up with the speech ----

/// Subtitles that were lined up and can still be undone or saved.
struct Synced {
    video: String,
    /// The subtitle file they came from, and its new contents.
    file: std::path::PathBuf,
    new: Vec<u8>,
    /// The track they replaced, and its delay.
    sid: String,
    delay: f64,
    /// The track showing them.
    added: String,
}

thread_local! {
    static SYNC_CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static SYNCED: RefCell<Option<Synced>> = const { RefCell::new(None) };
}

/// The subtitle file showing, if it is one that can be lined up.
fn subtitle_file(p: &Player) -> Result<std::path::PathBuf, String> {
    if p.get_string("sid").is_none_or(|s| s == "no") {
        return Err("Turn subtitles on first: these are the ones that will be lined up.".into());
    }
    let file = p.get_string("current-tracks/sub/external-filename").filter(|f| !f.is_empty());
    let Some(file) = file.map(std::path::PathBuf::from) else {
        return Err("These subtitles are built into the film. Only subtitle files can be lined up; Z and X move these by hand.".into());
    };
    if subsync::can_sync(&file) { Ok(file) } else { Err("Only SRT, WebVTT and ASS subtitle files can be lined up.".into()) }
}

fn sync_job(p: &Player, subtitles: std::path::PathBuf) -> Result<subsync::Job, String> {
    let video = local_path(p).ok_or("This works for files on this computer, not for streams and discs.")?;
    Ok(subsync::Job {
        video,
        aid: p.get_string("aid").and_then(|v| v.parse().ok()),
        duration: p.get_double("duration").unwrap_or(0.0),
        subtitles,
    })
}

/// Show `new` instead of the subtitles on screen. Returns what to undo.
fn show_synced(p: &Player, job: &subsync::Job, new: Vec<u8>) -> Result<Synced, String> {
    let dir = crate::settings::cache_dir().ok_or("no cache folder")?.join("subtitles");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let ext = job.subtitles.extension().unwrap_or_default().to_string_lossy();
    let copy = dir.join(format!("lined-up-{}.{ext}", std::process::id()));
    std::fs::write(&copy, &new).map_err(|e| format!("{}: {e}", copy.display()))?;
    let sid = p.get_string("sid").unwrap_or_else(|| "no".into());
    let delay = p.get_double("sub-delay").unwrap_or(0.0);
    let name = job.subtitles.file_stem().unwrap_or_default().to_string_lossy();
    let lang = p.get_string("current-tracks/sub/lang").unwrap_or_default();
    p.command(&["sub-add", &copy.to_string_lossy(), "select", &format!("{name} (lined up)"), &lang]).map_err(|e| e.to_string())?;
    let _ = p.set_double("sub-delay", 0.0);
    let added = p.get_string("sid").unwrap_or_default();
    Ok(Synced { video: job.video.clone(), file: job.subtitles.clone(), new, sid, delay, added })
}

fn install_subsync(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    // Copies shown in earlier sessions (mpv keeps what it has read).
    if let Some(dir) = crate::settings::cache_dir().and_then(|d| std::fs::read_dir(d.join("subtitles")).ok()) {
        for e in dir.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("lined-up-")) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let (p, weak) = (player.clone(), ui.as_weak());
    ui.on_sub_sync(move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_subsync_working() {
            return;
        }
        // The panel opened again on the same film: what was done still stands.
        if ui.get_subsync_done() && SYNCED.with_borrow(|s| s.as_ref().is_some_and(|s| p.get_string("sid").as_deref() == Some(&s.added))) {
            return;
        }
        ui.set_subsync_done(false);
        let job = match subtitle_file(&p).and_then(|f| sync_job(&p, f)) {
            Ok(j) => j,
            Err(e) => return ui.set_subsync_status(e.into()),
        };
        let cancel = clip::cancel_flag();
        SYNC_CANCEL.with_borrow_mut(|c| *c = Some(cancel.clone()));
        ui.set_subsync_working(true);
        ui.set_subsync_progress(0.0);
        ui.set_subsync_status("Listening to the film…".into());
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let w = weak.clone();
            let result = subsync::run(&job, &cancel, move |f| {
                let _ = w.upgrade_in_event_loop(move |ui| ui.set_subsync_progress(f));
            });
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_subsync_working(false);
                ui.set_subsync_progress(-1.0);
                let text = match result {
                    Err(e) => e,
                    Ok((fit, _)) if !fit.changes() => "These subtitles are already in time.".into(),
                    Ok((fit, new)) => {
                        let mut text = String::new();
                        crate::with_player(|p| {
                            // Only onto the film and subtitles it was made for.
                            let same = local_path(p).as_deref() == Some(&job.video)
                                && subtitle_file(p).is_ok_and(|f| f == job.subtitles);
                            text = if !same {
                                "Something else is playing by now, so nothing was changed.".into()
                            } else {
                                match show_synced(p, &job, new) {
                                    Ok(s) => {
                                        SYNCED.set(Some(s));
                                        ui.set_subsync_done(true);
                                        format!("Done: {}.", fit.describe())
                                    }
                                    Err(e) => format!("Couldn't show the subtitles: {e}"),
                                }
                            };
                        });
                        if !ui.get_subsync_open() {
                            ui.set_toast(text.clone().into());
                        }
                        text
                    }
                };
                ui.set_subsync_status(text.into());
            });
        });
    });

    ui.on_sub_sync_cancel(|| {
        SYNC_CANCEL.with_borrow(|c| {
            if let Some(c) = c {
                c.store(true, Ordering::Relaxed);
            }
        });
    });

    let weak = ui.as_weak();
    ui.on_sub_sync_keep(move || {
        let Some(ui) = weak.upgrade() else { return };
        let Some(s) = SYNCED.take() else { return };
        let name = s.file.file_name().unwrap_or_default().to_string_lossy().into_owned();
        match subsync::replace(&s.file, &s.new, true) {
            Ok(()) => {
                ui.set_subsync_done(false);
                ui.set_subsync_status(format!("Saved in {name}. The original is next to it, as {name}.orig.").into());
            }
            Err(e) => {
                ui.set_subsync_status(format!("Couldn't save {name}: {e}").into());
                SYNCED.set(Some(s));
            }
        }
    });

    let (p, weak) = (player.clone(), ui.as_weak());
    ui.on_sub_sync_undo(move || {
        let Some(ui) = weak.upgrade() else { return };
        let Some(s) = SYNCED.take() else { return };
        ui.set_subsync_done(false);
        if local_path(&p).as_deref() == Some(&s.video) {
            let _ = p.command(&["sub-remove", &s.added]);
            let _ = p.set_string("sid", &s.sid);
            let _ = p.set_double("sub-delay", s.delay);
        }
        ui.set_subsync_status("The subtitles are as they were.".into());
    });

    // Another film: the one before can't be undone any more.
    let (p, weak) = (player.clone(), ui.as_weak());
    every(ui, Duration::from_millis(1000), move || {
        let Some(ui) = weak.upgrade() else { return };
        let stale = SYNCED.with_borrow(|s| s.as_ref().is_some_and(|s| local_path(&p).as_deref() != Some(&s.video)));
        if stale {
            SYNCED.set(None);
            ui.set_subsync_done(false);
            ui.set_subsync_status("".into());
        }
    });

    // Subtitles just downloaded (opensubs.rs): nobody has seen them yet, so
    // the file itself is corrected.
    let (p, weak) = (player.clone(), ui.as_weak());
    ui.on_sub_sync_auto(move |file| {
        let Some(ui) = weak.upgrade() else { return };
        let file = std::path::PathBuf::from(file.as_str());
        if !settings.borrow().sub_auto_sync || ui.get_subsync_working() || !subsync::can_sync(&file) {
            return;
        }
        let Ok(job) = sync_job(&p, file) else { return };
        let cancel = clip::cancel_flag();
        SYNC_CANCEL.with_borrow_mut(|c| *c = Some(cancel.clone()));
        ui.set_subsync_working(true);
        ui.set_subsync_status("Listening to the film…".into());
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let result = subsync::run(&job, &cancel, |_| {});
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_subsync_working(false);
                ui.set_subsync_status("".into());
                match result {
                    Ok((fit, new)) if fit.changes() => match subsync::replace(&job.subtitles, &new, false) {
                        Ok(()) => {
                            crate::with_player(|p| {
                                if subtitle_file(p).is_ok_and(|f| f == job.subtitles) {
                                    let _ = p.command(&["sub-reload"]);
                                }
                            });
                            ui.set_toast(format!("Subtitles lined up with the speech: {}.", fit.describe()).into());
                        }
                        Err(e) => log::warn!("{}: {e}", job.subtitles.display()),
                    },
                    Ok(_) => log::info!("the downloaded subtitles are in time"),
                    Err(e) if e == subsync::UNSURE => {
                        ui.set_toast("These subtitles may be for another version: they don't clearly match the speech.".into());
                    }
                    Err(e) => log::info!("subtitles not lined up: {e}"),
                }
            });
        });
    });
}

// ---- subtitles from speech, and search ----

/// Most search results listed.
const MAX_RESULTS: usize = 400;

fn show_results(ui: &AppWindow) {
    TRANSCRIPT.with_borrow(|(lines, query)| {
        let items: Vec<TranscriptLine> = transcribe::search(lines, query, MAX_RESULTS)
            .into_iter()
            .map(|l| TranscriptLine {
                time: l.time as f32,
                clock: crate::fmt_clock(l.time).into(),
                text: l.text.as_str().into(),
            })
            .collect();
        ui.set_has_transcript(!lines.is_empty());
        ui.set_transcript_results(ModelRc::new(VecModel::from(items)));
    });
}

/// Subtitles to search for the file playing: ones made here before, else
/// the selected external subtitle file.
fn find_transcript(player: &Player, path: &str) -> Option<std::path::PathBuf> {
    if let Some(p) = transcribe::existing_subtitles(path) {
        return Some(p);
    }
    let file = player.get_string("current-tracks/sub/external-filename")?;
    let lower = file.to_lowercase();
    (lower.ends_with(".srt") || lower.ends_with(".vtt")).then(|| file.into())
}

fn load_transcript(ui: &AppWindow, srt: Option<&std::path::Path>) {
    let lines = srt
        .and_then(|p| std::fs::read(p).ok())
        .map(|b| transcribe::parse_subtitles(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();
    TRANSCRIPT.with_borrow_mut(|t| t.0 = lines);
    show_results(ui);
}

fn install_transcripts(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    // A new file, or the panel opened: find its subtitles, say what can be done.
    let (p, weak, st) = (player.clone(), ui.as_weak(), settings.clone());
    let last = RefCell::new((None::<String>, false));
    every(ui, Duration::from_millis(500), move || {
        let Some(ui) = weak.upgrade() else { return };
        let path = p.get_string("path").filter(|s| !s.is_empty());
        let open = ui.get_transcript_open();
        let now = (path.clone(), open);
        if *last.borrow() == now {
            return;
        }
        let file_changed = last.borrow().0 != path;
        last.replace(now);
        if file_changed || open {
            let srt = path.as_deref().and_then(|s| find_transcript(&p, s));
            load_transcript(&ui, srt.as_deref());
        }
        if ui.get_transcript_working() {
            return;
        }
        let disc = path.as_deref().is_some_and(|s| s.starts_with("dvd") || crate::dvdmenu::is_menu_path(s) || s.starts_with("bd://") || s.starts_with("cdda"));
        let (can, status) = match (&path, transcribe::whisper_cli()) {
            (None, _) => (false, "Open a file first.".to_owned()),
            (Some(_), _) if disc => (false, "Discs can't be transcribed directly: rip the title first.".to_owned()),
            (Some(_), None) => (
                false,
                "whisper.cpp is not installed. Run scripts/fetch-whisper.sh (it builds whisper-cli, using the GPU through Vulkan where it can), or install a whisper-cli.".to_owned(),
            ),
            (Some(_), Some(_)) => {
                let model = st.borrow().whisper_model.clone();
                let size = transcribe::model_info(&model).map_or(0, |m| m.3 >> 20);
                let have = transcribe::model_path(&model).is_some();
                (
                    true,
                    if ui.get_has_transcript() {
                        "Subtitles are ready: search them below.".to_owned()
                    } else if have {
                        format!("Model “{model}” is ready.")
                    } else {
                        format!("Model “{model}” will be downloaded first ({size} MB, checked against its published checksum).")
                    },
                )
            }
        };
        ui.set_can_transcribe(can);
        ui.set_transcript_status(status.into());
    });

    let weak = ui.as_weak();
    ui.on_transcript_search(move |q| {
        let Some(ui) = weak.upgrade() else { return };
        TRANSCRIPT.with_borrow_mut(|t| t.1 = q.to_string());
        show_results(&ui);
    });

    let (p, weak) = (player.clone(), ui.as_weak());
    ui.on_transcribe(move |translate| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(source) = p.get_string("path").filter(|s| !s.is_empty()) else { return };
        let job = transcribe::Job {
            source,
            aid: p.get_string("aid").and_then(|v| v.parse().ok()),
            duration: p.get_double("duration").unwrap_or(0.0),
            model: settings.borrow().whisper_model.clone(),
            translate,
        };
        let cancel = clip::cancel_flag();
        TRANSCRIBE_CANCEL.with_borrow_mut(|c| *c = Some(cancel.clone()));
        ui.set_transcript_working(true);
        ui.set_transcript_progress(0.0);
        ui.set_transcript_status("Starting…".into());
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let w = weak.clone();
            let result = transcribe::run(&job, &cancel, move |step| {
                let (text, f) = match step {
                    transcribe::Step::Download(f) => ("Downloading the speech model…", f),
                    transcribe::Step::Decode(f) => ("Reading the sound…", f),
                    transcribe::Step::Recognise(f) => ("Recognising speech…", f),
                };
                let _ = w.upgrade_in_event_loop(move |ui| {
                    ui.set_transcript_status(format!("{text} {:.0}%", f * 100.0).into());
                    ui.set_transcript_progress(f);
                });
            });
            let source = job.source.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_transcript_working(false);
                ui.set_transcript_progress(-1.0);
                match result {
                    Ok(srt) => {
                        crate::with_player(|p| {
                            // Only onto the file it was made for.
                            if p.get_string("path").as_deref() == Some(source.as_str()) {
                                let title = if job.translate { "Speech (English)" } else { "Speech" };
                                let _ = p.command(&["sub-add", &srt.to_string_lossy(), "select", title]);
                            }
                        });
                        load_transcript(&ui, Some(&srt));
                        ui.set_transcript_status("Done: the subtitles are on. Search them below.".into());
                    }
                    Err(e) => ui.set_transcript_status(
                        if e == "Cancelled" { e } else { format!("Couldn't make subtitles: {e}") }.into(),
                    ),
                }
            });
        });
    });
    ui.on_transcribe_cancel(|| {
        TRANSCRIBE_CANCEL.with_borrow(|c| {
            if let Some(c) = c {
                c.store(true, Ordering::Relaxed);
            }
        });
    });
}

// ---- phone remote ----

thread_local! {
    static REMOTE: RefCell<Option<remote::Server>> = const { RefCell::new(None) };
}

fn install_remote(ui: &AppWindow, player: &Rc<Player>) {
    let weak = ui.as_weak();
    ui.on_remote_start(move || {
        let Some(ui) = weak.upgrade() else { return };
        let w = ui.as_weak();
        let server = remote::start(move |cmd| {
            let _ = w.upgrade_in_event_loop(move |ui| {
                use remote::Command::*;
                match cmd {
                    TogglePause => ui.invoke_toggle_pause(),
                    Next => ui.invoke_next(),
                    Prev => ui.invoke_prev(),
                    Stop => ui.invoke_stop(),
                    SeekRelative(v) => ui.invoke_seek_relative(v as f32),
                    Seek(v) => ui.invoke_seek(v as f32),
                    Volume(v) => ui.invoke_set_volume(v as f32),
                    Mute => ui.invoke_toggle_mute(),
                    Fullscreen => ui.invoke_toggle_fullscreen(),
                    Skip => ui.invoke_skip_pressed(),
                }
            });
        });
        match server {
            Ok(server) => {
                if let Some(qr) = remote::qr_image(&server.url, 4) {
                    ui.set_remote_qr(slint::Image::from_rgba8(qr));
                }
                ui.set_remote_address(server.url.clone().into());
                ui.set_remote_status("Keep this window open while you use it.".into());
                ui.set_remote_running(true);
                REMOTE.with_borrow_mut(|r| *r = Some(server));
            }
            Err(e) => ui.set_remote_status(format!("Couldn't start the remote: {e}").into()),
        }
    });
    let weak = ui.as_weak();
    ui.on_remote_stop(move || {
        REMOTE.with_borrow_mut(|r| *r = None);
        if let Some(ui) = weak.upgrade() {
            ui.set_remote_running(false);
            ui.set_remote_status("The remote is off; the old link no longer works.".into());
        }
    });
    // Keep the page's picture of the player current.
    let (p, weak) = (player.clone(), ui.as_weak());
    every(ui, Duration::from_millis(500), move || {
        let Some(ui) = weak.upgrade() else { return };
        REMOTE.with_borrow(|r| {
            let Some(server) = r else { return };
            if let Ok(mut s) = server.state.lock() {
                *s = remote::State {
                    title: ui.get_media_title().to_string(),
                    artist: ui.get_media_artist().to_string(),
                    position: crate::cast::position(&p).unwrap_or(0.0),
                    duration: f64::from(ui.get_duration()),
                    paused: ui.get_paused(),
                    idle: ui.get_idle(),
                    volume: f64::from(ui.get_volume()),
                    muted: ui.get_muted(),
                    skip: ui.get_skip_text().to_string(),
                };
            }
        });
    });
}

// ---- watch together ----

#[derive(Default)]
struct PartyState {
    party: Option<party::Party>,
    /// Our own changes (following the host / a guest) look like the user's
    /// for a moment: ignore what happens until then.
    quiet_until: Option<std::time::Instant>,
    /// Last seen: paused, position, when.
    last: Option<(bool, f64, std::time::Instant)>,
    last_broadcast: Option<std::time::Instant>,
}

thread_local! {
    static PARTY: RefCell<PartyState> = RefCell::new(PartyState::default());
}

/// The name of the file playing, as the party compares it.
fn party_file(p: &Player) -> String {
    p.get_string("filename").unwrap_or_default()
}

fn party_people(ui: &AppWindow, pt: &party::Party, names: Vec<String>) {
    let list = if pt.hosting {
        let mut all = vec![format!("{} (host, you)", party::clean_name(&ui.get_party_name()))];
        all.extend(pt.guests());
        pt.send_people(&all);
        all
    } else {
        names
    };
    let model: Vec<slint::SharedString> = list.into_iter().map(Into::into).collect();
    ui.set_party_people(ModelRc::new(VecModel::from(model)));
}

fn install_party(ui: &AppWindow, player: &Rc<Player>) {
    if ui.get_party_name().is_empty() {
        let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
        ui.set_party_name(party::clean_name(&user).into());
    }
    let weak = ui.as_weak();
    ui.on_party_host(move || {
        let Some(ui) = weak.upgrade() else { return };
        match party::Party::host(&ui.get_party_name()) {
            Ok(pt) => {
                ui.set_party_invite(pt.invite.clone().into());
                ui.set_party_mode(1);
                ui.set_party_status("Waiting for the others to join.".into());
                party_people(&ui, &pt, Vec::new());
                PARTY.with_borrow_mut(|s| *s = PartyState { party: Some(pt), ..Default::default() });
            }
            Err(e) => ui.set_party_status(format!("Couldn't host: {e}").into()),
        }
    });
    let weak = ui.as_weak();
    ui.on_party_join(move || {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_party_status("Joining…".into());
        match party::Party::join(&ui.get_party_join_address(), &ui.get_party_name()) {
            Ok(pt) => {
                ui.set_party_mode(2);
                ui.set_party_status("Joined: open the same file as the host.".into());
                PARTY.with_borrow_mut(|s| *s = PartyState { party: Some(pt), ..Default::default() });
            }
            Err(e) => ui.set_party_status(format!("Couldn't join: {e}").into()),
        }
    });
    let weak = ui.as_weak();
    ui.on_party_leave(move || {
        PARTY.with_borrow_mut(|s| *s = PartyState::default());
        if let Some(ui) = weak.upgrade() {
            ui.set_party_mode(0);
            ui.set_party_status("".into());
            ui.set_party_people(ModelRc::default());
        }
    });

    let (p, weak) = (player.clone(), ui.as_weak());
    every(ui, Duration::from_millis(250), move || {
        let Some(ui) = weak.upgrade() else { return };
        let now = std::time::Instant::now();
        let mut ended = None;
        PARTY.with_borrow_mut(|st| {
            let Some(pt) = &st.party else { return };
            let paused = p.get_string("pause").as_deref() == Some("yes");
            let pos = p.get_double("time-pos").unwrap_or(0.0);
            let file = party_file(&p);
            let quiet = st.quiet_until.is_some_and(|q| now < q);
            let mut hush = false;
            for ev in pt.events.try_iter() {
                match ev {
                    party::Event::Control(c) => {
                        // A guest asks: do it here; the broadcast below tells everyone.
                        match c {
                            party::Control::Pause => { let _ = p.set_pause(true); }
                            party::Control::Play => { let _ = p.set_pause(false); }
                            party::Control::Seek(t) => { let _ = p.seek(player_core::Seek::Absolute(t)); }
                        }
                        hush = true;
                        st.last_broadcast = None;
                    }
                    party::Event::State(host) => {
                        if !file.is_empty() && host.file != file {
                            ui.set_party_status(format!("The host is playing “{}”: open the same file to follow.", host.file).into());
                            continue;
                        }
                        if file.is_empty() {
                            ui.set_party_status(format!("The host is playing “{}”: open it to follow.", host.file).into());
                            continue;
                        }
                        ui.set_party_status("In step with the host.".into());
                        let (pause, seek) = party::follow(&host, paused, pos, 0.15);
                        if let Some(pz) = pause { let _ = p.set_pause(pz); hush = true; }
                        if let Some(t) = seek { let _ = p.seek(player_core::Seek::Absolute(t)); hush = true; }
                    }
                    party::Event::People(names) => party_people(&ui, pt, names),
                    party::Event::Closed(why) => ended = Some(why),
                }
            }
            if hush {
                st.quiet_until = Some(now + Duration::from_millis(1200));
                st.last = None;
                return;
            }
            // What the user here did since the last look: a pause change or a jump.
            let speed = p.get_double("speed").unwrap_or(1.0);
            let action = match st.last {
                Some((lp, lpos, at)) if !quiet => {
                    let expected = lpos + if lp { 0.0 } else { now.duration_since(at).as_secs_f64() * speed };
                    if lp != paused {
                        Some(if paused { party::Control::Pause } else { party::Control::Play })
                    } else if (pos - expected).abs() > 2.0 {
                        Some(party::Control::Seek(pos))
                    } else {
                        None
                    }
                }
                _ => None,
            };
            st.last = Some((paused, pos, now));
            if pt.hosting {
                let due = st.last_broadcast.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1));
                if (due || action.is_some()) && !file.is_empty() {
                    pt.broadcast(&party::Shared { paused, position: pos, file });
                    st.last_broadcast = Some(now);
                }
            } else if let Some(a) = action {
                pt.control(&a);
            }
        });
        if let Some(why) = ended {
            ui.invoke_party_leave();
            ui.set_party_status(why.into());
        }
    });
}

// ---- video enhancement ----

/// The screen's refresh rate, Hz.
fn refresh_rate(ui: &AppWindow) -> Option<f64> {
    use slint::winit_030::WinitWindowAccessor;
    ui.window()
        .with_winit_window(|w| w.current_monitor().and_then(|m| m.refresh_rate_millihertz()))
        .flatten()
        .map(|mhz| f64::from(mhz) / 1000.0)
}

/// Menu and toast names of `settings::TONE_MAPS`, in order.
const TONE_MAP_NAMES: &[&str] = &["Automatic", "Balanced (BT.2390)", "Smooth (spline)", "Filmic (Hable)", "Bright (Mobius)", "Clip (no tone-mapping)"];

fn install_enhance(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    // Dev aid (with --snapshot): start with the before / after split on.
    if std::env::var_os("SMP_DEV_COMPARE").is_some() {
        ui.set_compare_on(true);
    }
    let weak = ui.as_weak();
    ui.on_set_enhance(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_s_video_enhance(i);
        if i == 0 {
            ui.set_compare_on(false);
        }
        ui.invoke_settings_changed();
    });
    let weak = ui.as_weak();
    ui.on_set_tone_map(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_s_tone_map(i);
        ui.invoke_settings_changed();
        let name = TONE_MAP_NAMES.get(i as usize).copied().unwrap_or("Automatic");
        ui.set_toast(format!("HDR tone-mapping: {name}").into());
    });
    // Apply what the settings and the split ask for; the split only once
    // it has stopped moving (each change recompiles the shaders).
    let (p, weak) = (player.clone(), ui.as_weak());
    let applied: RefCell<Option<(String, Option<u32>, bool)>> = RefCell::new(None);
    let applied_tone: RefCell<Option<(String, bool)>> = RefCell::new(None);
    let moving = Cell::new((None::<u32>, 0u32));
    every(ui, Duration::from_millis(120), move || {
        let Some(ui) = weak.upgrade() else { return };
        // Only while a video shows: changing video options before mpv's
        // render context exists makes mpv wait for it, on this very thread.
        if !ui.get_has_video() {
            return;
        }
        let (mode, smooth, tone) = {
            let s = settings.borrow();
            (s.video_enhance.clone(), s.smooth_motion, (s.hdr_tone_map.clone(), s.hdr_peak_detect))
        };
        if applied_tone.borrow().as_ref() != Some(&tone) {
            enhance::set_tone_mapping(&p, &tone.0, tone.1);
            applied_tone.replace(Some(tone));
        }
        let split = (ui.get_compare_on() && ui.get_has_video() && mode != "off").then(|| {
            (enhance::window_to_video(&p, f64::from(ui.get_compare_split())) * 1000.0).round() as u32
        });
        // Wait for the split to rest for ~3 ticks.
        let (last, n) = moving.get();
        moving.set((split, if last == split { n + 1 } else { 0 }));
        if split.is_some() && moving.get().1 < 3 {
            return;
        }
        let want = (mode.clone(), split, smooth);
        let prev = applied.borrow().clone();
        if prev.as_ref() == Some(&want) {
            return;
        }
        if prev.as_ref().is_none_or(|(m, s, _)| *m != mode || *s != split)
            && let Err(e) = enhance::apply(&p, &mode, split.map(|s| f64::from(s) / 1000.0))
        {
            ui.set_toast(format!("Video enhancement: {e}").into());
        }
        if prev.as_ref().is_none_or(|(_, _, sm)| *sm != smooth) && (smooth || prev.is_some()) {
            enhance::set_smooth(&p, smooth, refresh_rate(&ui));
        }
        applied.replace(Some(want));
    });
}

/// Run `f` every `interval` for the life of the program.
pub fn every(_ui: &AppWindow, interval: Duration, f: impl FnMut() + 'static) {
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, interval, f);
    std::mem::forget(timer);
}
