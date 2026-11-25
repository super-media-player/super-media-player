//! The everyday basics every player has: recent files, audio and subtitle
//! delay, subtitle look, picture adjustments, the output device, chapters,
//! media information, jump to time, always on top, the sleep timer, and the
//! MPRIS service that lets media keys and the desktop control the player.
//!
//! Most actions come from the UI as `basic(action, value)`; one timer keeps
//! the menus (chapters, devices, delays, sleep time left) up to date.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use player_core::{Player, Value};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::features::every;
use crate::settings::{self, Settings};
use crate::{AppWindow, InfoRow, PictureState};

/// Most recent files kept.
const RECENT_MAX: usize = 15;
/// Sleep timer choices, minutes (index 1..=5 of the menu; 6 = end of file).
const SLEEP_MINUTES: [u64; 5] = [15, 30, 45, 60, 90];
/// The sleep timer turns the sound down over this long before pausing.
const SLEEP_FADE: Duration = Duration::from_secs(20);
/// Aspect ratio menu → mpv `video-aspect-override` ("stretch" handled apart).
const ASPECTS: [&str; 7] = ["-1", "16:9", "4:3", "2.35:1", "1.85:1", "1:1", "stretch"];
const ROTATIONS: [i64; 4] = [0, 90, 180, 270];

#[derive(Default)]
struct Sleep {
    choice: i32,
    /// When to pause (timer choices).
    deadline: Option<Instant>,
    /// Volume before the fade began.
    faded_from: Option<f64>,
}

#[derive(Default)]
struct Basics {
    chapters: Vec<(f64, String)>,
    recent: Vec<String>,
    devices: Vec<(String, String)>,
    sleep: Sleep,
    picture: PictureState,
    last_path: Option<String>,
    /// The saved output device was applied (it may appear only later).
    device_applied: bool,
    #[cfg(target_os = "linux")]
    mpris: Option<crate::mpris::Mpris>,
    track: u64,
    art_url: String,
    /// Every bookmark: file, time, name.
    bookmarks: Vec<(String, f64, String)>,
    /// Recording the stream playing: (its path, the file written, since).
    recording: Option<(String, PathBuf, Instant)>,
}

thread_local! {
    static B: RefCell<Basics> = RefCell::new(Basics::default());
}

pub fn install(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    B.with_borrow_mut(|b| {
        b.recent = load_recent();
        b.bookmarks = load_bookmarks();
    });
    ui.set_recent_files(labels(&B.with_borrow(|b| b.recent.clone())));
    ui.set_picture(B.with_borrow(|b| b.picture.clone()));
    apply_subtitle_look(player, &settings.borrow());
    apply_on_top(ui, settings.borrow().always_on_top, false);

    {
        let (p, weak, settings) = (player.clone(), ui.as_weak(), settings.clone());
        ui.on_basic(move |action, value| {
            let Some(ui) = weak.upgrade() else { return };
            act(&ui, &p, &settings, action.as_str(), value);
        });
    }
    {
        let p = player.clone();
        ui.on_jump_to(move |text| {
            let pos = crate::cast::position(&p).unwrap_or(0.0);
            let dur = p.get_double("duration").unwrap_or(0.0);
            match parse_time(&text, pos, dur) {
                Some(t) => {
                    let _ = p.command(&["seek", &format!("{t:.3}"), "absolute+exact"]);
                    true
                }
                None => false,
            }
        });
    }
    ui.on_chapter_name(|t| {
        B.with_borrow(|b| chapter_at(&b.chapters, f64::from(t)).unwrap_or_default().into())
    });

    #[cfg(target_os = "linux")]
    start_mpris(ui);

    // Keep menus and the sleep timer up to date.
    let (p, weak) = (player.clone(), ui.as_weak());
    let mut tick = 0u32;
    every(ui, Duration::from_millis(500), move || {
        let Some(ui) = weak.upgrade() else { return };
        tick = tick.wrapping_add(1);
        refresh(&ui, &p, &settings, tick);
    });
}

fn act(ui: &AppWindow, p: &Player, settings: &Rc<RefCell<Settings>>, action: &str, v: f32) {
    let v = f64::from(v);
    let toast = |t: String| ui.set_toast(t.into());
    match action {
        "audio-delay" | "sub-delay" => {
            let now = p.get_double(action).unwrap_or(0.0);
            let new = ((now + v) * 1000.0).round() / 1000.0;
            let _ = p.set_double(action, new);
            let what = if action == "audio-delay" { "Audio delay" } else { "Subtitle delay" };
            toast(format!("{what}: {}", fmt_delay(new)));
        }
        "audio-delay-reset" => {
            let _ = p.set_double("audio-delay", 0.0);
            toast("Audio delay: 0 ms".into());
        }
        "sub-delay-reset" => {
            let _ = p.set_double("sub-delay", 0.0);
            toast("Subtitle delay: 0 ms".into());
        }
        "chapter" => {
            let _ = p.set_double("chapter", v);
        }
        "chapter-step" => {
            if B.with_borrow(|b| b.chapters.is_empty()) {
                toast("This file has no chapters".into());
            } else {
                let _ = p.command(&["add", "chapter", if v < 0.0 { "-1" } else { "1" }]);
            }
        }
        "recent" => {
            let path = B.with_borrow(|b| b.recent.get(v as usize).cloned());
            if let Some(input) = path.as_deref().and_then(crate::parse_input) {
                crate::open_inputs(ui, vec![input], false);
            }
        }
        "recent-clear" => {
            B.with_borrow_mut(|b| b.recent.clear());
            save_recent(&[]);
            ui.set_recent_files(labels(&[]));
        }
        "audio-device" => {
            let dev = B.with_borrow(|b| b.devices.get(v as usize).cloned());
            if let Some((name, desc)) = dev {
                if let Err(e) = p.set_string("audio-device", &name) {
                    toast(format!("Couldn't switch to {desc}: {e}"));
                    return;
                }
                ui.set_s_audio_device(v as i32);
                let mut s = settings.borrow_mut();
                s.audio_device = name;
                s.save();
                toast(format!("Sound: {desc}"));
            }
        }
        "bookmark-add" => {
            let (Some(path), Some(t)) = (p.get_string("path"), crate::cast::position(p)) else { return };
            let name = chapter_at(&B.with_borrow(|b| b.chapters.clone()), t).unwrap_or_default();
            B.with_borrow_mut(|b| {
                b.bookmarks.push((path.clone(), t, name));
                save_bookmarks(&b.bookmarks);
            });
            show_bookmarks(ui, Some(&path));
            toast(format!("Bookmark at {}", crate::fmt_clock(t)));
        }
        "bookmark" => {
            let path = p.get_string("path");
            let t = B.with_borrow(|b| file_bookmarks(&b.bookmarks, path.as_deref()).get(v as usize).map(|m| m.0));
            if let Some(t) = t {
                let _ = p.command(&["seek", &format!("{t:.3}"), "absolute+exact"]);
            }
        }
        "bookmarks-clear" => {
            let path = p.get_string("path");
            B.with_borrow_mut(|b| {
                b.bookmarks.retain(|m| Some(m.0.as_str()) != path.as_deref());
                save_bookmarks(&b.bookmarks);
            });
            show_bookmarks(ui, path.as_deref());
            toast("Bookmarks removed".into());
        }
        "record" => toggle_recording(ui, p),
        "sleep" => set_sleep(ui, p, v as i32),
        "media-info" => show_media_info(ui, p),
        _ => picture(ui, p, action, v),
    }
}

// ---- picture ----

fn picture(ui: &AppWindow, p: &Player, action: &str, v: f64) {
    let mut toast = None;
    B.with_borrow_mut(|b| {
        let pic = &mut b.picture;
        match action {
            "brightness" | "contrast" | "saturation" | "gamma" | "hue" => {
                let v = v.round().clamp(-100.0, 100.0);
                match action {
                    "brightness" => pic.brightness = v as f32,
                    "contrast" => pic.contrast = v as f32,
                    "saturation" => pic.saturation = v as f32,
                    "gamma" => pic.gamma = v as f32,
                    _ => pic.hue = v as f32,
                }
                let _ = p.set_double(action, v);
            }
            "zoom" | "zoom-to" => {
                let z = if action == "zoom" { f64::from(pic.zoom) + v } else { v };
                pic.zoom = (z.clamp(-1.0, 2.0) * 100.0).round() as f32 / 100.0;
                let _ = p.set_double("video-zoom", f64::from(pic.zoom));
                toast = Some(format!("Zoom {:.0}%", 2f64.powf(f64::from(pic.zoom)) * 100.0));
            }
            "pan-x" | "pan-x-to" | "pan-y" | "pan-y-to" => {
                let x = action.starts_with("pan-x");
                let cur = if x { pic.pan_x } else { pic.pan_y };
                let n = if action.ends_with("-to") { v } else { f64::from(cur) + v };
                let n = (n.clamp(-1.0, 1.0) * 100.0).round() as f32 / 100.0;
                if x {
                    pic.pan_x = n;
                } else {
                    pic.pan_y = n;
                }
                let _ = p.set_double(if x { "video-pan-x" } else { "video-pan-y" }, f64::from(n));
            }
            "aspect" => {
                let i = (v as usize).min(ASPECTS.len() - 1);
                pic.aspect = i as i32;
                let stretch = ASPECTS[i] == "stretch";
                let _ = p.set_flag("keepaspect", !stretch);
                let _ = p.set_string("video-aspect-override", if stretch { "-1" } else { ASPECTS[i] });
                toast = Some(match i {
                    0 => "Aspect ratio: automatic".into(),
                    6 => "Aspect ratio: stretched to the window".into(),
                    _ => format!("Aspect ratio: {}", ASPECTS[i]),
                });
            }
            "rotate" => {
                let i = (v as usize).min(3);
                pic.rotate = i as i32;
                let _ = p.set_double("video-rotate", ROTATIONS[i] as f64);
            }
            "deinterlace" => {
                pic.deinterlace = v > 0.5;
                let _ = p.set_flag("deinterlace", pic.deinterlace);
                toast = Some(format!("Deinterlace {}", if pic.deinterlace { "on" } else { "off" }));
            }
            "reset" => {
                *pic = PictureState::default();
                for k in ["brightness", "contrast", "saturation", "gamma", "hue", "video-zoom", "video-pan-x", "video-pan-y", "video-rotate"] {
                    let _ = p.set_double(k, 0.0);
                }
                let _ = p.set_string("video-aspect-override", "-1");
                let _ = p.set_flag("keepaspect", true);
                let _ = p.set_flag("deinterlace", false);
                toast = Some("Picture reset".into());
            }
            other => log::warn!("unknown action {other}"),
        }
    });
    ui.set_picture(B.with_borrow(|b| b.picture.clone()));
    if let Some(t) = toast {
        ui.set_toast(t.into());
    }
}

// ---- subtitles, always on top ----

/// Size, position, colour and background of text subtitles.
pub fn apply_subtitle_look(p: &Player, s: &Settings) {
    let scale = match s.sub_size.as_str() {
        "small" => 0.8,
        "large" => 1.3,
        "huge" => 1.7,
        _ => 1.0,
    };
    let pos = match s.sub_pos.as_str() {
        "higher" => 92.0,
        "high" => 82.0,
        "top" => 12.0,
        _ => 100.0,
    };
    let color = match s.sub_color.as_str() {
        "yellow" => "#FFE84A",
        "grey" => "#D4D4D4",
        "cyan" => "#7FE8FF",
        "green" => "#8CFF7A",
        _ => "#FFFFFF",
    };
    let _ = p.set_double("sub-scale", scale);
    let _ = p.set_double("sub-pos", pos);
    let _ = p.set_string("sub-color", color);
    let boxed = matches!(s.sub_style.as_str(), "box" | "lightbox");
    // mpv colours are #AARRGGBB.
    let back = if s.sub_style == "lightbox" { "#80000000" } else { "#D0000000" };
    let has_style = p
        .set_string("sub-border-style", if boxed { "background-box" } else { "outline-and-shadow" })
        .is_ok();
    let _ = p.set_string("sub-border-color", "#FF000000");
    if boxed {
        let _ = p.set_string("sub-back-color", back);
        if has_style {
            let _ = p.set_double("sub-shadow-offset", 0.0);
        } else {
            // Before mpv 0.38: the back colour is a wide shadow.
            let _ = p.set_double("sub-shadow-offset", 4.0);
            let _ = p.set_double("sub-border-size", 4.0);
        }
    } else {
        let shadow = s.sub_style == "shadow";
        let _ = p.set_double("sub-border-size", 3.0);
        let _ = p.set_double("sub-shadow-offset", if shadow { 2.0 } else { 0.0 });
        let _ = p.set_string("sub-back-color", "#80000000");
    }
}

/// Keep the window above others. `tell`: explain when it can't be done.
pub fn apply_on_top(ui: &AppWindow, on: bool, tell: bool) {
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::window::WindowLevel;
    if on && tell && on_wayland(ui) {
        ui.set_toast(
            "Wayland doesn't let a program keep itself on top. Use the window menu (Alt+Space → Always on Top), or set Window system to X11 in Settings → System."
                .into(),
        );
    }
    ui.window().with_winit_window(|w| {
        w.set_window_level(if on { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal });
    });
}

pub fn on_wayland(ui: &AppWindow) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    ui.window()
        .window_handle()
        .window_handle()
        .is_ok_and(|h| matches!(h.as_raw(), RawWindowHandle::Wayland(_)))
}

/// Bring the window to the front (a file sent by a second start, MPRIS Raise).
pub fn raise(ui: &AppWindow) {
    use slint::winit_030::WinitWindowAccessor;
    let _ = ui.show();
    ui.window().set_minimized(false);
    ui.window().with_winit_window(|w| w.focus_window());
}

// ---- recording a stream ----

/// Streams and discs: what plays isn't a file that could just be copied.
fn can_record(path: &str) -> bool {
    path.contains("://") && !crate::dvdmenu::is_menu_path(path)
}

fn toggle_recording(ui: &AppWindow, p: &Player) {
    if B.with_borrow(|b| b.recording.is_some()) {
        stop_recording(ui, p);
        return;
    }
    let Some(src) = p.get_string("path").filter(|s| can_record(s)) else {
        ui.set_toast("Recording is for streams and discs; a file can be saved with Save a clip or convert".into());
        return;
    };
    let title = p.get_string("media-title").unwrap_or_else(|| "Recording".into());
    let ext = if ui.get_has_video() { "mkv" } else { "mka" };
    let out = recording_path(&crate::clip::videos_dir(), &title, ext);
    if let Err(e) = p.set_string("stream-record", &out.to_string_lossy()) {
        ui.set_toast(format!("Couldn't start recording: {e}").into());
        return;
    }
    B.with_borrow_mut(|b| b.recording = Some((src, out.clone(), Instant::now())));
    ui.set_recording(true);
    ui.set_recording_text("0:00".into());
    ui.set_toast(format!("Recording to {}", out.display()).into());
}

fn stop_recording(ui: &AppWindow, p: &Player) {
    let _ = p.set_string("stream-record", "");
    ui.set_recording(false);
    if let Some((_, out, since)) = B.with_borrow_mut(|b| b.recording.take()) {
        let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        ui.set_toast(if size == 0 {
            let _ = std::fs::remove_file(&out);
            "Recording stopped; nothing was written".into()
        } else {
            format!(
                "Recorded {} ({}) to {}",
                crate::fmt_clock(since.elapsed().as_secs_f64()),
                fmt_bytes(size as f64),
                out.display()
            )
            .into()
        });
    }
}

/// "<title> 2026-11-24 21-05.mkv" in `dir`, never overwriting.
fn recording_path(dir: &Path, title: &str, ext: &str) -> PathBuf {
    let clean: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' })
        .take(80)
        .collect();
    let clean = clean.trim().trim_matches('.');
    let clean = if clean.is_empty() { "Recording" } else { clean };
    let stamp = local_stamp();
    let mut path = dir.join(format!("{clean} {stamp}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{clean} {stamp} ({n}).{ext}"));
        n += 1;
    }
    path
}

/// "2026-11-24 21-05", local time (from `date`, else UTC).
fn local_stamp() -> String {
    if let Ok(o) = std::process::Command::new("date").arg("+%Y-%m-%d %H-%M").output()
        && o.status.success()
    {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
        if !s.is_empty() {
            return s;
        }
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}-{:02}", rem / 3600, rem % 3600 / 60)
}

// ---- sleep timer ----

fn set_sleep(ui: &AppWindow, p: &Player, choice: i32) {
    B.with_borrow_mut(|b| {
        if let Some(v) = b.sleep.faded_from.take() {
            let _ = p.set_volume(v);
        }
        b.sleep = Sleep {
            choice,
            deadline: (1..=5)
                .contains(&choice)
                .then(|| Instant::now() + Duration::from_secs(SLEEP_MINUTES[choice as usize - 1] * 60)),
            faded_from: None,
        };
    });
    // At the end of the file mpv pauses instead of moving on (see refresh).
    let _ = p.set_string("keep-open", if choice == 6 { "always" } else { "no" });
    ui.set_sleep_choice(choice);
    ui.set_toast(
        match choice {
            0 => "Sleep timer off".to_string(),
            6 => "Playback will pause at the end of this file".into(),
            c => format!("Playback will pause in {} minutes", SLEEP_MINUTES[c as usize - 1]),
        }
        .into(),
    );
}

fn tick_sleep(ui: &AppWindow, p: &Player) {
    let mut done = false;
    B.with_borrow_mut(|b| {
        let s = &mut b.sleep;
        match s.choice {
            1..=5 => {
                let Some(deadline) = s.deadline else { return };
                let left = deadline.saturating_duration_since(Instant::now());
                ui.set_sleep_left(fmt_left(left).into());
                if left.is_zero() {
                    if !crate::cast::set_pause(ui, true) {
                        let _ = p.set_pause(true);
                    }
                    if let Some(v) = s.faded_from.take() {
                        let _ = p.set_volume(v);
                    }
                    done = true;
                } else if left < SLEEP_FADE && !ui.get_paused() && !crate::cast::active() {
                    let from = *s.faded_from.get_or_insert(f64::from(ui.get_volume()));
                    let _ = p.set_volume(from * left.as_secs_f64() / SLEEP_FADE.as_secs_f64());
                }
            }
            6 => {
                ui.set_sleep_left("end of file".into());
                if p.get_value("eof-reached").and_then(|v| v.as_bool()) == Some(true) {
                    let _ = p.set_pause(true);
                    done = true;
                }
            }
            _ => ui.set_sleep_left(SharedString::new()),
        }
    });
    if done {
        let _ = p.set_string("keep-open", "no");
        B.with_borrow_mut(|b| b.sleep = Sleep::default());
        ui.set_sleep_choice(0);
        ui.set_sleep_left(SharedString::new());
        ui.set_toast("Sleep timer: paused".into());
    }
}

fn fmt_left(d: Duration) -> String {
    let m = d.as_secs().div_ceil(60);
    if m >= 60 {
        format!("{}:{:02} left", m / 60, m % 60)
    } else {
        format!("{m} min left")
    }
}

// ---- periodic refresh ----

fn refresh(ui: &AppWindow, p: &Player, settings: &Rc<RefCell<Settings>>, tick: u32) {
    // Chapters.
    let chapters = chapters_of(&p.get_value("chapter-list").unwrap_or_default());
    if B.with_borrow(|b| b.chapters != chapters) {
        ui.set_chapter_times(ModelRc::new(VecModel::from(
            chapters.iter().map(|c| c.0 as f32).collect::<Vec<_>>(),
        )));
        ui.set_chapter_titles(ModelRc::new(VecModel::from(
            chapters
                .iter()
                .map(|(t, n)| SharedString::from(format!("{}   {n}", crate::fmt_clock(*t))))
                .collect::<Vec<_>>(),
        )));
        B.with_borrow_mut(|b| b.chapters = chapters);
    }
    ui.set_chapter_current(p.get_double("chapter").map_or(-1, |c| c as i32));

    // Delays.
    ui.set_audio_delay_text(fmt_delay(p.get_double("audio-delay").unwrap_or(0.0)).into());
    ui.set_sub_delay_text(fmt_delay(p.get_double("sub-delay").unwrap_or(0.0)).into());

    tick_sleep(ui, p);

    // Recording: stop when another file starts; show how long so far.
    let rec = B.with_borrow(|b| b.recording.clone());
    if let Some((src, _, since)) = rec {
        if p.get_string("path").as_deref() != Some(src.as_str()) || ui.get_idle() {
            stop_recording(ui, p);
        } else {
            ui.set_recording_text(crate::fmt_clock(since.elapsed().as_secs_f64()).into());
        }
    }
    ui.set_can_record(p.get_string("path").is_some_and(|s| can_record(&s)));

    // A new file: remember it.
    let path = p.get_string("path");
    if B.with_borrow(|b| b.last_path != path) {
        B.with_borrow_mut(|b| {
            b.last_path = path.clone();
            b.track += 1;
            b.art_url = path.as_deref().and_then(folder_cover).unwrap_or_default();
        });
        show_bookmarks(ui, path.as_deref());
        if let Some(path) = path.filter(|p| worth_remembering(p)) {
            let list = B.with_borrow_mut(|b| {
                remember(&mut b.recent, &path);
                b.recent.clone()
            });
            save_recent(&list);
            ui.set_recent_files(labels(&list));
        }
    }

    // Output devices (every 2 s: plugging in headphones adds one).
    if tick % 4 == 1 {
        let devices = devices_of(&p.get_value("audio-device-list").unwrap_or_default());
        let wanted = settings.borrow().audio_device.clone();
        if B.with_borrow(|b| b.devices != devices) {
            ui.set_audio_devices(ModelRc::new(VecModel::from(
                devices.iter().map(|d| SharedString::from(d.1.as_str())).collect::<Vec<_>>(),
            )));
            B.with_borrow_mut(|b| b.devices = devices.clone());
        }
        let current = p.get_string("audio-device").unwrap_or_default();
        // The saved device, once it shows up (never one that isn't there:
        // mpv would play no sound at all).
        if !B.with_borrow(|b| b.device_applied) && wanted != current {
            if devices.iter().any(|d| d.0 == wanted) {
                let _ = p.set_string("audio-device", &wanted);
                B.with_borrow_mut(|b| b.device_applied = true);
            }
        } else {
            B.with_borrow_mut(|b| b.device_applied = true);
        }
        let current = p.get_string("audio-device").unwrap_or_default();
        ui.set_s_audio_device(devices.iter().position(|d| d.0 == current).unwrap_or(0) as i32);
    }

    if ui.get_media_info_open() && tick.is_multiple_of(2) {
        show_media_info(ui, p);
    }

    #[cfg(target_os = "linux")]
    update_mpris(ui);
}

/// Called when the settings dialog changed something of ours.
pub fn settings_changed(ui: &AppWindow, p: &Player, old: &Settings, new: &Settings) {
    if (&old.sub_size, &old.sub_pos, &old.sub_color, &old.sub_style)
        != (&new.sub_size, &new.sub_pos, &new.sub_color, &new.sub_style)
    {
        apply_subtitle_look(p, new);
    }
    if old.always_on_top != new.always_on_top {
        apply_on_top(ui, new.always_on_top, true);
    }
    if old.audio_device != new.audio_device
        && let Err(e) = p.set_string("audio-device", &new.audio_device)
    {
        ui.set_toast(format!("Couldn't switch the output device: {e}").into());
    }
}

/// The device list index chosen in the settings dialog, as a device name.
/// None until the saved device was looked for (the menu shows "automatic"
/// until then, which mustn't overwrite the saved choice).
pub fn device_name(index: i32) -> Option<String> {
    B.with_borrow(|b| {
        b.device_applied.then(|| b.devices.get(index.max(0) as usize).map(|d| d.0.clone())).flatten()
    })
}

// ---- bookmarks ----

fn bookmarks_file() -> Option<PathBuf> {
    settings::config_dir().map(|d| d.join("bookmarks.txt"))
}

/// Lines of "time<TAB>name<TAB>file".
fn load_bookmarks() -> Vec<(String, f64, String)> {
    let text = bookmarks_file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    parse_bookmarks(&text)
}

fn parse_bookmarks(text: &str) -> Vec<(String, f64, String)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let t: f64 = it.next()?.parse().ok()?;
            let name = it.next()?.to_owned();
            let file = it.next()?.to_owned();
            (!file.is_empty() && t >= 0.0).then_some((file, t, name))
        })
        .collect()
}

fn save_bookmarks(list: &[(String, f64, String)]) {
    let Some(f) = bookmarks_file() else { return };
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let text: String =
        list.iter().map(|(file, t, name)| format!("{t:.3}\t{}\t{}\n", clean(name), clean(file))).collect();
    let _ = std::fs::write(f, text);
}

/// The bookmarks of `path`, by time: (time, name).
fn file_bookmarks(all: &[(String, f64, String)], path: Option<&str>) -> Vec<(f64, String)> {
    let mut v: Vec<(f64, String)> =
        all.iter().filter(|m| Some(m.0.as_str()) == path).map(|m| (m.1, m.2.clone())).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

fn show_bookmarks(ui: &AppWindow, path: Option<&str>) {
    let list = B.with_borrow(|b| file_bookmarks(&b.bookmarks, path));
    ui.set_bookmark_times(ModelRc::new(VecModel::from(list.iter().map(|m| m.0 as f32).collect::<Vec<_>>())));
    ui.set_bookmark_titles(ModelRc::new(VecModel::from(
        list.iter()
            .map(|(t, n)| {
                SharedString::from(if n.is_empty() {
                    crate::fmt_clock(*t)
                } else {
                    format!("{}   {n}", crate::fmt_clock(*t))
                })
            })
            .collect::<Vec<_>>(),
    )));
}

// ---- recent files ----

fn recent_file() -> Option<PathBuf> {
    settings::config_dir().map(|d| d.join("recent.txt"))
}

fn load_recent() -> Vec<String> {
    recent_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).take(RECENT_MAX).map(String::from).collect())
        .unwrap_or_default()
}

fn save_recent(list: &[String]) {
    let Some(f) = recent_file() else { return };
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, list.join("\n") + "\n");
}

/// Put `path` first, once.
fn remember(list: &mut Vec<String>, path: &str) {
    list.retain(|p| p != path);
    list.insert(0, path.to_owned());
    list.truncate(RECENT_MAX);
}

/// Files and links, not discs or the player's own "Rip & play" copies.
fn worth_remembering(path: &str) -> bool {
    if path.starts_with("http://") || path.starts_with("https://") {
        return true;
    }
    if path.contains("://") {
        return false;
    }
    let p = Path::new(path);
    p.is_file() && !crate::ripper::temp_root().is_some_and(|r| p.starts_with(r))
}

/// Menu titles: file names (links as they are, shortened).
fn labels(list: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        list.iter()
            .map(|p| {
                let name = if p.contains("://") {
                    p.split_once("://").map_or(p.as_str(), |x| x.1).to_owned()
                } else {
                    Path::new(p)
                        .file_name()
                        .map_or_else(|| p.clone(), |n| n.to_string_lossy().into_owned())
                };
                let short: String = name.chars().take(70).collect();
                SharedString::from(if short.len() < name.len() { short + "…" } else { short })
            })
            .collect::<Vec<_>>(),
    ))
}

// ---- chapters, devices ----

fn chapters_of(v: &Value) -> Vec<(f64, String)> {
    v.as_array()
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let t = c.get("time")?.as_f64()?;
            let title = c.str_field("title").filter(|s| !s.trim().is_empty());
            Some((t, title.unwrap_or_else(|| format!("Chapter {}", i + 1))))
        })
        .collect()
}

fn chapter_at(chapters: &[(f64, String)], t: f64) -> Option<String> {
    chapters.iter().rev().find(|c| c.0 <= t).map(|c| c.1.clone())
}

fn devices_of(v: &Value) -> Vec<(String, String)> {
    v.as_array()
        .iter()
        .filter_map(|d| {
            let name = d.str_field("name")?;
            let desc = d.str_field("description").unwrap_or_else(|| name.clone());
            Some(if name == "auto" { (name, "Automatic (system default)".into()) } else { (name, desc) })
        })
        .collect()
}

// ---- jump to time ----

/// "1:23:45", "12:30", "90", "1h2m3s", "50%", or "+30" / "-1:00" from `pos`.
pub fn parse_time(text: &str, pos: f64, duration: f64) -> Option<f64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(pct) = t.strip_suffix('%') {
        let x: f64 = pct.trim().replace(',', ".").parse().ok()?;
        return (duration > 0.0 && (0.0..=100.0).contains(&x)).then(|| duration * x / 100.0);
    }
    let (sign, rest) = match t.chars().next()? {
        '+' => (1.0, &t[1..]),
        '-' => (-1.0, &t[1..]),
        _ => (0.0, t),
    };
    let secs = clock_seconds(rest.trim())?;
    let abs = if sign == 0.0 { secs } else { pos + sign * secs };
    let max = if duration > 0.0 { duration } else { f64::MAX };
    Some(abs.clamp(0.0, max))
}

fn clock_seconds(t: &str) -> Option<f64> {
    if t.contains(['h', 'm', 's']) {
        let mut total = 0.0;
        let mut num = String::new();
        for c in t.chars() {
            match c {
                '0'..='9' | '.' => num.push(c),
                'h' | 'm' | 's' => {
                    let n: f64 = num.parse().ok()?;
                    total += n * match c {
                        'h' => 3600.0,
                        'm' => 60.0,
                        _ => 1.0,
                    };
                    num.clear();
                }
                ' ' => {}
                _ => return None,
            }
        }
        if !num.is_empty() {
            total += num.parse::<f64>().ok()?;
        }
        return Some(total);
    }
    let parts: Vec<&str> = t.split([':', ' ']).filter(|s| !s.is_empty()).collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut total = 0.0;
    for p in &parts {
        let n: f64 = p.replace(',', ".").parse().ok()?;
        if n < 0.0 {
            return None;
        }
        total = total * 60.0 + n;
    }
    Some(total)
}

fn fmt_delay(s: f64) -> String {
    let ms = (s * 1000.0).round() as i64;
    if ms == 0 { "0 ms".into() } else { format!("{ms:+} ms") }
}

// ---- media information ----

fn show_media_info(ui: &AppWindow, p: &Player) {
    let rows = media_info(p);
    let text = rows
        .iter()
        .map(|r| if r.heading { format!("\n{}", r.label) } else { format!("  {}: {}", r.label, r.value) })
        .collect::<Vec<_>>()
        .join("\n");
    ui.set_media_info_text(text.trim_start().into());
    ui.set_media_info_rows(ModelRc::new(VecModel::from(rows)));
}

fn media_info(p: &Player) -> Vec<InfoRow> {
    let mut rows = Vec::new();
    let head = |rows: &mut Vec<InfoRow>, t: &str| {
        rows.push(InfoRow { label: t.into(), value: SharedString::new(), heading: true })
    };
    let row = |rows: &mut Vec<InfoRow>, l: &str, v: Option<String>| {
        if let Some(v) = v.filter(|v| !v.trim().is_empty()) {
            rows.push(InfoRow { label: l.into(), value: v.into(), heading: false });
        }
    };
    let s = |k: &str| p.get_string(k).filter(|v| !v.is_empty());
    let d = |k: &str| p.get_double(k);

    let Some(path) = s("path") else {
        head(&mut rows, "NOTHING PLAYING");
        return rows;
    };
    head(&mut rows, "FILE");
    row(&mut rows, "Title", s("media-title"));
    row(&mut rows, "Location", Some(path));
    row(&mut rows, "Format", s("file-format"));
    row(&mut rows, "Size", d("file-size").map(fmt_bytes));
    let duration = d("duration");
    row(&mut rows, "Length", duration.map(crate::fmt_clock));
    if let (Some(size), Some(dur)) = (d("file-size"), duration.filter(|d| *d > 0.0)) {
        row(&mut rows, "Overall bitrate", Some(fmt_rate(size * 8.0 / dur)));
    }
    let chapters = d("chapters").unwrap_or(0.0);
    if chapters > 0.0 {
        row(&mut rows, "Chapters", Some(format!("{chapters:.0}")));
    }

    let tracks = p.get_value("track-list").unwrap_or_default();
    let tracks = tracks.as_array();
    let selected = |kind: &str| {
        tracks.iter().find(|t| t.str_field("type").as_deref() == Some(kind) && t.bool_field("selected"))
    };
    let count = |kind: &str| tracks.iter().filter(|t| t.str_field("type").as_deref() == Some(kind)).count();

    if let Some(v) = selected("video").filter(|t| !t.bool_field("albumart")) {
        head(&mut rows, "VIDEO");
        row(&mut rows, "Codec", codec(v));
        let (w, h) = (d("video-params/w"), d("video-params/h"));
        if let (Some(w), Some(h)) = (w, h) {
            let mut res = format!("{w:.0} × {h:.0}");
            if let (Some(dw), Some(dh)) = (d("video-params/dw"), d("video-params/dh"))
                && (dw, dh) != (w, h)
            {
                res += &format!(" (shown as {dw:.0} × {dh:.0})");
            }
            row(&mut rows, "Resolution", Some(res));
            row(&mut rows, "Black bars cropped to", crate::autocrop::shown());
        }
        row(&mut rows, "Aspect ratio", d("video-params/aspect").map(|a| format!("{a:.3}:1")));
        let fps = d("container-fps").or_else(|| d("estimated-vf-fps"));
        row(&mut rows, "Frame rate", fps.map(|f| format!("{f:.3} fps")));
        row(&mut rows, "Bitrate", d("video-bitrate").filter(|b| *b > 0.0).map(fmt_rate));
        row(&mut rows, "Pixel format", s("video-params/pixelformat"));
        let gamma = s("video-params/gamma").unwrap_or_default();
        let hdr = match gamma.as_str() {
            "pq" => Some("HDR10 (PQ)".to_string()),
            "hlg" => Some("HLG".to_string()),
            _ => None,
        };
        let colour = [s("video-params/colormatrix"), s("video-params/primaries"), Some(gamma.clone())]
            .into_iter()
            .flatten()
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        row(&mut rows, "Colour", Some(colour));
        row(
            &mut rows,
            "HDR",
            Some(match hdr {
                Some(h) => match d("video-params/sig-peak").filter(|p| *p > 1.0) {
                    Some(peak) => format!("{h}, peak {:.0} nits", peak * 203.0),
                    None => h,
                },
                None => "No (SDR)".into(),
            }),
        );
        row(
            &mut rows,
            "Hardware decoding",
            Some(match s("hwdec-current").filter(|h| h != "no") {
                Some(h) => format!("Yes ({h})"),
                None => "No (CPU)".into(),
            }),
        );
        let drops = d("frame-drop-count").unwrap_or(0.0);
        let dec = d("decoder-frame-drop-count").unwrap_or(0.0);
        row(&mut rows, "Dropped frames", Some(format!("{drops:.0} by the display, {dec:.0} by the decoder")));
    }
    if let Some(a) = selected("audio") {
        head(&mut rows, "AUDIO");
        row(&mut rows, "Codec", codec(a));
        row(&mut rows, "Language", a.str_field("lang"));
        row(&mut rows, "Channels", s("audio-params/hr-channels").or_else(|| a.str_field("demux-channels")));
        row(&mut rows, "Sample rate", d("audio-params/samplerate").map(|r| format!("{:.1} kHz", r / 1000.0)));
        row(&mut rows, "Sample format", s("audio-params/format"));
        row(&mut rows, "Bitrate", d("audio-bitrate").filter(|b| *b > 0.0).map(fmt_rate));
        let out = [s("audio-out-params/format"), d("audio-out-params/samplerate").map(|r| format!("{:.1} kHz", r / 1000.0)), s("audio-out-params/hr-channels")];
        row(&mut rows, "Sent to the device as", Some(out.into_iter().flatten().collect::<Vec<_>>().join(" · ")));
        row(&mut rows, "Output device", s("audio-device"));
        let delay = d("audio-delay").unwrap_or(0.0);
        if delay != 0.0 {
            row(&mut rows, "Audio delay", Some(fmt_delay(delay)));
        }
    }
    let subs = count("sub");
    if subs > 0 {
        head(&mut rows, "SUBTITLES");
        for t in tracks.iter().filter(|t| t.str_field("type").as_deref() == Some("sub")) {
            let mut name = [t.str_field("lang"), t.str_field("title"), t.str_field("codec")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            if t.bool_field("external") {
                name += " (file)";
            }
            if t.bool_field("selected") {
                name += "  ← showing";
            }
            let id = t.get("id").and_then(|i| i.as_i64()).unwrap_or(0);
            row(&mut rows, &format!("Track {id}"), Some(name));
        }
        let delay = d("sub-delay").unwrap_or(0.0);
        if delay != 0.0 {
            row(&mut rows, "Subtitle delay", Some(fmt_delay(delay)));
        }
    }
    head(&mut rows, "TRACKS");
    row(
        &mut rows,
        "In this file",
        Some(format!("{} video, {} audio, {subs} subtitle", count("video"), count("audio"))),
    );
    if let Some(Value::Map(tags)) = p.get_value("metadata") {
        let tags: Vec<_> = tags.iter().filter_map(|(k, v)| Some((k, v.as_str()?.to_owned()))).take(20).collect();
        if !tags.is_empty() {
            head(&mut rows, "TAGS");
            for (k, v) in tags {
                row(&mut rows, &k.to_lowercase(), Some(v));
            }
        }
    }
    rows
}

fn codec(t: &Value) -> Option<String> {
    let c = t.str_field("codec")?;
    Some(match t.str_field("codec-desc").or_else(|| t.str_field("decoder-desc")) {
        Some(desc) if !desc.is_empty() && desc != c => format!("{c} ({desc})"),
        _ => c,
    })
}

fn fmt_bytes(b: f64) -> String {
    match b {
        b if b >= 1e9 => format!("{:.2} GB", b / 1e9),
        b if b >= 1e6 => format!("{:.1} MB", b / 1e6),
        b => format!("{:.0} kB", b / 1e3),
    }
}

fn fmt_rate(bps: f64) -> String {
    if bps >= 1e6 { format!("{:.2} Mbit/s", bps / 1e6) } else { format!("{:.0} kbit/s", bps / 1e3) }
}

// ---- MPRIS ----

/// A cover picture next to the file, as a file:// URL.
fn folder_cover(path: &str) -> Option<String> {
    let dir = Path::new(path).parent().filter(|_| !path.contains("://"))?;
    ["cover.jpg", "folder.jpg", "front.jpg", "cover.png", "folder.png", "Cover.jpg", "Folder.jpg", "AlbumArt.jpg"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
        .map(|p| format!("file://{}", p.display()))
}

#[cfg(target_os = "linux")]
fn start_mpris(ui: &AppWindow) {
    use crate::mpris::{Command, Mpris};
    let weak = ui.as_weak();
    let mpris = Mpris::start(move |cmd| {
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let pos = f64::from(ui.get_position());
            match cmd {
                Command::Play => {
                    if ui.get_paused() || ui.get_idle() {
                        ui.invoke_toggle_pause();
                    }
                }
                Command::Pause => {
                    if !ui.get_paused() && !ui.get_idle() {
                        ui.invoke_toggle_pause();
                    }
                }
                Command::PlayPause => ui.invoke_toggle_pause(),
                Command::Stop => ui.invoke_stop(),
                Command::Next => ui.invoke_next(),
                Command::Previous => ui.invoke_prev(),
                Command::Seek(us) => ui.invoke_seek((pos + us as f64 / 1e6).max(0.0) as f32),
                Command::SetPosition(us) => ui.invoke_seek((us as f64 / 1e6) as f32),
                Command::Volume(v) => ui.invoke_set_volume((v * 100.0) as f32),
                Command::Rate(r) => ui.invoke_set_speed(r.clamp(0.25, 4.0) as f32),
                Command::Shuffle(on) => {
                    if on != ui.get_shuffle() {
                        ui.invoke_toggle_shuffle();
                    }
                }
                Command::Loop(l) => ui.invoke_set_repeat(match l.as_str() {
                    "Playlist" => 1,
                    "Track" => 2,
                    _ => 0,
                }),
                Command::OpenUri(u) => ui.invoke_open_location(u.into()),
                Command::Raise => raise(&ui),
                Command::Quit => ui.invoke_quit(),
            }
        });
    });
    B.with_borrow_mut(|b| b.mpris = mpris);
}

#[cfg(target_os = "linux")]
fn update_mpris(ui: &AppWindow) {
    use slint::Model;
    B.with_borrow_mut(|b| {
        let (track, art) = (b.track, b.art_url.clone());
        let Some(m) = b.mpris.as_mut() else { return };
        let idle = ui.get_idle();
        let list = ui.get_playlist();
        let n = list.row_count();
        let cur = (0..n).find(|&i| list.row_data(i).is_some_and(|r| r.current));
        m.update(crate::mpris::Snapshot {
            status: if idle {
                "Stopped"
            } else if ui.get_paused() {
                "Paused"
            } else {
                "Playing"
            },
            title: ui.get_media_title().into(),
            artist: ui.get_media_artist().into(),
            album: ui.get_media_album().into(),
            art_url: art,
            track,
            length_us: (f64::from(ui.get_duration()) * 1e6) as i64,
            position_us: (f64::from(ui.get_position()) * 1e6) as i64,
            volume: f64::from(ui.get_volume()) / 100.0,
            rate: f64::from(ui.get_speed()),
            shuffle: ui.get_shuffle(),
            loop_status: match ui.get_repeat_mode() {
                1 => "Playlist",
                2 => "Track",
                _ => "None",
            },
            can_go_next: cur.is_some_and(|c| c + 1 < n) || ui.get_repeat_mode() == 1,
            can_go_previous: !idle,
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("1:23:45", 0.0, 0.0), Some(5025.0));
        assert_eq!(parse_time("12:30", 0.0, 0.0), Some(750.0));
        assert_eq!(parse_time("90", 0.0, 0.0), Some(90.0));
        assert_eq!(parse_time("1h2m3s", 0.0, 0.0), Some(3723.0));
        assert_eq!(parse_time("2m", 0.0, 0.0), Some(120.0));
        assert_eq!(parse_time("50%", 0.0, 200.0), Some(100.0));
        assert_eq!(parse_time("+30", 100.0, 1000.0), Some(130.0));
        assert_eq!(parse_time("-1:00", 30.0, 1000.0), Some(0.0));
        assert_eq!(parse_time("5000", 0.0, 100.0), Some(100.0));
        assert_eq!(parse_time("abc", 0.0, 100.0), None);
        assert_eq!(parse_time("", 0.0, 100.0), None);
        assert_eq!(parse_time("150%", 0.0, 100.0), None);
    }

    #[test]
    fn recent_list_moves_to_front() {
        let mut l = vec!["a".to_string(), "b".into(), "c".into()];
        remember(&mut l, "c");
        assert_eq!(l, ["c", "a", "b"]);
        for i in 0..20 {
            remember(&mut l, &i.to_string());
        }
        assert_eq!(l.len(), RECENT_MAX);
        assert_eq!(l[0], "19");
    }

    #[test]
    fn chapter_lookup() {
        let c = vec![(0.0, "Intro".to_string()), (60.0, "Part 1".into()), (300.0, "End".into())];
        assert_eq!(chapter_at(&c, 30.0).as_deref(), Some("Intro"));
        assert_eq!(chapter_at(&c, 60.0).as_deref(), Some("Part 1"));
        assert_eq!(chapter_at(&c, 900.0).as_deref(), Some("End"));
        assert_eq!(chapter_at(&[], 9.0), None);
    }

    #[test]
    fn bookmarks_round_trip() {
        let text = "12.500\tPart 1\t/a/film.mkv\n3.000\t\t/a/film.mkv\nbad line\n9\tx\thttps://y/z\n";
        let all = parse_bookmarks(text);
        assert_eq!(all.len(), 3);
        let mine = file_bookmarks(&all, Some("/a/film.mkv"));
        assert_eq!(mine, vec![(3.0, String::new()), (12.5, "Part 1".to_string())]);
    }

    #[test]
    fn recording_names() {
        let dir = std::env::temp_dir();
        let p = recording_path(&dir, "Radio: Jazz/Live", "mka");
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("Radio_ Jazz_Live 20"), "{name}");
        assert!(name.ends_with(".mka"));
        assert!(can_record("https://x/y.m3u8") && !can_record("/home/a.mkv"));
    }

    /// End to end: mpv's stream-record writes what plays. Skipped without
    /// libmpv or ffmpeg.
    #[test]
    fn stream_record_writes_a_file() {
        use player_core::{LoadMode, PlayerConfig, PlayerEvent};
        let dir = std::env::temp_dir().join(format!("smp-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mkv");
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=160x120:rate=25:duration=2"])
            .args(["-f", "lavfi", "-i", "sine=duration=2", "-shortest", "-c:v", "libx264", "-c:a", "aac"])
            .arg(&src)
            .status()
            .is_ok_and(|s| s.success());
        let (tx, rx) = std::sync::mpsc::channel();
        let mut cfg = PlayerConfig::headless();
        cfg.set("ao", "null").set("vo", "null");
        let Ok(p) = player_core::Player::new(&cfg, move |e| {
            let _ = tx.send(e);
        }) else {
            eprintln!("libmpv not available; skipping");
            return;
        };
        if !made {
            eprintln!("ffmpeg not available; skipping");
            return;
        }
        let out = dir.join("rec.mkv");
        p.set_string("stream-record", &out.to_string_lossy()).unwrap();
        p.load(&src.to_string_lossy(), LoadMode::Replace).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Ok(PlayerEvent::EndFile { .. }) = rx.recv_timeout(Duration::from_millis(200)) {
                break;
            }
        }
        let _ = p.set_string("stream-record", "");
        drop(p);
        let head = std::fs::read(&out).unwrap_or_default();
        assert!(head.len() > 1000 && head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]), "no Matroska recording");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delays() {
        assert_eq!(fmt_delay(0.0), "0 ms");
        assert_eq!(fmt_delay(0.1), "+100 ms");
        assert_eq!(fmt_delay(-0.25), "-250 ms");
    }
}
