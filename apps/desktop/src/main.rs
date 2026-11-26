// A release build on Windows is a window program: no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod art;
mod artist;
mod autocrop;
mod basics;
mod bluray;
mod cast;
mod cd;
mod clip;
mod collection;
mod disc;
mod discrip;
mod doctor;
mod dvdmenu;
mod engine;
mod enhance;
mod features;
mod frame;
mod instance;
mod intro;
mod library;
mod medialib;
mod meter;
mod milkdrop;
#[cfg(target_os = "linux")]
mod mpris;
mod musicinfo;
mod navigation;
mod network;
mod opensubs;
mod pace;
mod palette;
mod party;
mod pcm;
mod playlists;
mod pulse;
mod remote;
mod resume;
mod ripper;
mod scripts;
mod settings;
mod shortcuts;
mod signal;
mod silence;
mod skins;
mod sponsor;
mod state;
mod subsync;
mod thumbs;
mod tools;
mod transcribe;
mod video;
mod visual;

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use player_core::{LoadMode, Player, PlayerConfig, Seek, TrackKind};
use slint::ComponentHandle;

use disc::{DiscKind, DiscPlan};
use settings::Settings;
use state::UiState;
use video::{SnapshotRequest, VideoRenderer};

slint::include_modules!();

const MEDIA_EXTENSIONS: &[&str] = &[
    // video
    "mkv", "mp4", "m4v", "avi", "mov", "webm", "wmv", "flv", "mpg", "mpeg", "ts", "m2ts", "mts",
    "vob", "ogv", "3gp", "rm", "rmvb", "asf", "divx", "f4v", "mxf", // audio
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "aac", "wav", "wma", "ape", "wv", "alac", "aiff",
    "aif", "mka", "ac3", "dts", "mid", "midi", "mod", "xm", "it", "s3m", "tta", "dsf", "dff",
    // playlists & images
    "m3u", "m3u8", "pls", "xspf", "cue", "iso",
];
/// Not picked up when scanning a folder: playlists would duplicate the files
/// next to them, and disc images are opened on their own.
const SKIP_IN_FOLDERS: &[&str] = &["m3u", "m3u8", "pls", "xspf", "cue", "iso"];
const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "ass", "ssa", "vtt", "sub", "idx", "sup", "smi"];
/// Upper bound for one "open folder", so a whole disk can't freeze the player.
const MAX_FOLDER_FILES: usize = 10_000;

// Option lists for the settings, in the order the UI shows them.
const VIEW_MODES: &[&str] = settings::VIEW_MODES;
const ARTIST_POSITIONS: &[&str] = &["off", "top-left", "top-right"];
const BLURAY_DECRYPTION: &[&str] = &["auto", "libaacs", "makemkv"];
const WINDOW_SYSTEMS: &[&str] = &["auto", "x11", "wayland"];

/// Logical size of each small skinned view (the whole window: skins have no
/// title bar or menu bar): default width, height, and the widest it may stay
/// when switched to.
/// They all grow by `SKIN_PLAYLIST_HEIGHT` with their playlist open.
fn skin_size(mode: &str) -> Option<(f32, f32, f32)> {
    match mode {
        "compact" => Some((500.0, 210.0, 640.0)),
        "classic" => Some((550.0, 196.0, 700.0)),
        // Fixed: the silver deck's shapes are drawn for this size.
        "silver" => Some((600.0, 340.0, 600.0)),
        _ => None,
    }
}
const SKIN_PLAYLIST_HEIGHT: f32 = 300.0;

thread_local! {
    /// The docked playlist's height as the user last left it (see
    /// `note_playlist_height`).
    static PLAYLIST_HEIGHT: Cell<f32> = const { Cell::new(SKIN_PLAYLIST_HEIGHT) };
}

/// While a skin shows its playlist docked, remember how tall the user made
/// it, so closing and reopening the playlist keeps that height.
fn note_playlist_height(ui: &AppWindow, mode: &str) {
    let Some((_, skin_h, _)) = skin_size(mode) else {
        return;
    };
    if !ui.get_playlist_open() || playlist_in_own_window(ui, mode) || ui.get_panel_open() {
        return;
    }
    let h = ui
        .window()
        .size()
        .to_logical(ui.window().scale_factor())
        .height
        - skin_h;
    if h >= 120.0 {
        PLAYLIST_HEIGHT.with(|p| p.set(h));
    }
}

/// The least room the panels (saved links, settings…) need.
const PANEL_SIZE: (f32, f32) = (600.0, 560.0);

thread_local! {
    /// The skinned window's size before it grew for a panel.
    static BEFORE_PANEL: Cell<Option<slint::LogicalSize>> = const { Cell::new(None) };
}

/// The skinned players are too small for the panels: grow the window while
/// one is open, and go back to its size afterwards. True if it did either.
fn fit_panel(ui: &AppWindow) -> bool {
    let window = ui.window();
    match (ui.get_panel_open(), BEFORE_PANEL.get()) {
        (true, None) => {
            let now = window.size().to_logical(window.scale_factor());
            let grown =
                slint::LogicalSize::new(now.width.max(PANEL_SIZE.0), now.height.max(PANEL_SIZE.1));
            if grown == now {
                return false;
            }
            BEFORE_PANEL.set(Some(now));
            window.set_size(grown);
            true
        }
        (false, Some(before)) => {
            BEFORE_PANEL.set(None);
            window.set_size(before);
            true
        }
        _ => false,
    }
}

thread_local! {
    /// The skins in the order the UI lists them (skins.rs).
    static SKINS: RefCell<Vec<skins::Skin>> = const { RefCell::new(Vec::new()) };
    /// The classic deck's own playlist window, which follows the skin.
    static PLAYLIST_WIN: RefCell<Option<slint::Weak<PlaylistWindow>>> = const { RefCell::new(None) };
}

/// Read the skins and list them in the UI. Returns what was wrong with the
/// user's skin files.
fn load_skins(ui: &AppWindow) -> Vec<String> {
    let (all, warnings) = skins::load_all();
    for w in &warnings {
        log::warn!("skin {w}");
    }
    let names: Vec<slint::SharedString> = all.iter().map(|s| s.name.as_str().into()).collect();
    let views: Vec<i32> = all.iter().map(skins::Skin::view_index).collect();
    ui.set_skin_names(slint::ModelRc::new(slint::VecModel::from(names)));
    ui.set_skin_views(slint::ModelRc::new(slint::VecModel::from(views)));
    SKINS.with_borrow_mut(|s| *s = all);
    warnings
}

/// Index of skin `id` in the list, if it dresses `view`; else the plain view.
fn skin_index(id: &str, view: &str) -> i32 {
    SKINS.with_borrow(|all| {
        all.iter()
            .position(|s| s.id == id && s.layout == view)
            .or_else(|| all.iter().position(|s| s.id == view))
            .unwrap_or(0) as i32
    })
}

/// The id of the skin chosen in the UI ("" for a plain view).
fn chosen_skin(ui: &AppWindow) -> String {
    SKINS.with_borrow(|all| {
        all.get(ui.get_s_skin().max(0) as usize)
            .filter(|s| s.path.is_some() || !s.colors.is_empty())
            .map(|s| s.id.clone())
            .unwrap_or_default()
    })
}

/// Colour the windows with the skin chosen in the UI.
fn apply_skin(ui: &AppWindow) {
    let pw = PLAYLIST_WIN.with_borrow(|w| w.as_ref().and_then(|w| w.upgrade()));
    SKINS.with_borrow(|all| {
        if let Some(skin) = all.get(ui.get_s_skin().max(0) as usize) {
            skins::apply(ui, pw.as_ref(), skin);
        }
    });
}

/// Show a folder in the desktop's file manager (or a web page in its browser).
fn open_folder(dir: &Path) -> std::io::Result<()> {
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(dir)
        .spawn()
        .map(|_| ())
}

thread_local! {
    static UI_STATE: RefCell<Option<UiState>> = const { RefCell::new(None) };
    static PLAYER: RefCell<Option<Rc<Player>>> = const { RefCell::new(None) };
}

/// Run `f` with the player, once the engine has started.
fn with_player(f: impl FnOnce(&Rc<Player>)) {
    // Clone out first, so `f` may itself call with_player.
    if let Some(p) = PLAYER.with_borrow(|p| p.clone()) {
        f(&p)
    }
}

struct Args {
    inputs: Vec<String>,
    show_playlist: bool,
    show_about: bool,
    show_settings: bool,
    snapshot: Option<SnapshotRequest>,
}

fn parse_args() -> Args {
    let mut inputs = Vec::new();
    let mut snapshot = None;
    let mut show_playlist = false;
    let mut show_about = false;
    let mut show_settings = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            // Dev aid: --snapshot <file.ppm> [--snapshot-delay <secs>] saves the window and quits.
            "--snapshot" => {
                snapshot = it.next().map(|p| SnapshotRequest {
                    path: PathBuf::from(p),
                    after: Duration::from_secs(3),
                    then_quit: true,
                })
            }
            "--snapshot-delay" => {
                if let (Some(s), Some(secs)) = (
                    snapshot.as_mut(),
                    it.next().and_then(|v| v.parse::<f64>().ok()),
                ) {
                    s.after = Duration::from_secs_f64(secs);
                }
            }
            "--playlist" => show_playlist = true,
            "--about" => show_about = true,
            "--settings" => show_settings = true,
            "-h" | "--help" => {
                println!(
                    "Usage: super-media-player [--playlist] [--about] [--settings] [FILE|FOLDER|URL]...\n       \
                     super-media-player --self-test    check the playback engine and libraries, without a window"
                );
                std::process::exit(0);
            }
            "--self-test" => std::process::exit(self_test()),
            // Everything after "--" is an input, even if it starts with "-".
            "--" => inputs.extend(it.by_ref()),
            _ => inputs.push(a),
        }
    }
    Args {
        inputs,
        show_playlist,
        show_about,
        show_settings,
        snapshot,
    }
}

/// `--self-test`: start the playback engine without a window and check that
/// what the player relies on is there (the package tests run it in bare
/// containers). Prints one line per check; 0 when everything needed is.
fn self_test() -> i32 {
    let mut failed = 0;
    let mut line = |ok: bool, needed: bool, what: &str, detail: String| {
        let mark = if ok {
            "ok  "
        } else if needed {
            "FAIL"
        } else {
            "--  "
        };
        println!("{mark} {what}: {detail}");
        if !ok && needed {
            failed += 1;
        }
    };
    let player = match player_core::Player::new(&player_core::PlayerConfig::headless(), |_| {}) {
        Ok(p) => p,
        Err(e) => {
            line(false, true, "libmpv", e.to_string());
            return 1;
        }
    };
    let get = |p: &str| player.get_string(p).unwrap_or_default();
    line(
        true,
        true,
        "libmpv",
        format!(
            "{} with FFmpeg {}",
            get("mpv-version"),
            get("ffmpeg-version")
        ),
    );
    let encoders = get("encoder-list");
    for (enc, what) in [
        ("libx264", "MP4 clips"),
        ("libmp3lame", "MP3 clips"),
        ("libopus", "Opus clips"),
        ("flac", "FLAC"),
    ] {
        line(
            encoders.contains(&format!("\"{enc}\"")),
            true,
            &format!("encoder {enc}"),
            what.into(),
        );
    }
    // "cdda,rtmp,…,dvd,…" (the encoders are JSON).
    let protocols = get("protocol-list");
    let linux = cfg!(target_os = "linux");
    for (proto, what) in [("dvd", "DVDs"), ("bd", "Blu-rays"), ("cdda", "audio CDs")] {
        line(
            protocols.split(',').any(|p| p == proto),
            linux,
            &format!("{proto}://"),
            what.into(),
        );
    }
    let libs: &[(&[&str], &str, bool)] = &[
        (
            &["libcdio_paranoia.so.2"],
            "exact CD ripping (libcdio-paranoia)",
            linux,
        ),
        (
            &[
                "libchromaprint.so.1",
                "libchromaprint.dylib",
                "chromaprint.dll",
            ],
            "finding intros (chromaprint)",
            false,
        ),
        (
            &["libaacs.so.0", "libaacs.dylib", "libaacs.dll"],
            "Blu-ray decryption (libaacs)",
            false,
        ),
        (
            &["libdvdcss.so.2", "libdvdcss.2.dylib", "libdvdcss-2.dll"],
            "DVD decryption (libdvdcss)",
            false,
        ),
        (
            &[
                "libprojectM-4.so.4",
                "libprojectM-4.dylib",
                "projectM-4.dll",
            ],
            "MilkDrop (libprojectM)",
            false,
        ),
    ];
    for (names, what, needed) in libs {
        line(disc::library_present(names), *needed, what, names[0].into());
    }
    let ytdlp = tools::find_ytdlp();
    line(
        ytdlp.is_some(),
        false,
        "yt-dlp",
        ytdlp
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found".into()),
    );
    if failed == 0 {
        println!("all needed parts are there");
        0
    } else {
        println!("{failed} needed part(s) missing");
        1
    }
}

/// What the user asked to open: a local path or a URL.
#[derive(Debug, PartialEq)]
enum Input {
    Path(PathBuf),
    Url(String),
}

/// Parse one line of pasted or typed text, or a command-line argument:
/// a URL, a file:// URI, a quoted path, or a path starting with ~.
fn parse_input(s: &str) -> Option<Input> {
    let s = s.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    if s.is_empty() || s.starts_with('#') {
        return None;
    }
    if let Some(rest) = s.strip_prefix("file://") {
        // file:///path or file://localhost/path
        let path = rest.strip_prefix("localhost").unwrap_or(rest);
        return Some(Input::Path(PathBuf::from(percent_decode(path))));
    }
    if s.contains("://") {
        return Some(Input::Url(s.to_owned()));
    }
    let path = match s.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(|h| Path::new(&h).join(rest))?,
        None => PathBuf::from(s),
    };
    Some(Input::Path(std::fs::canonicalize(&path).unwrap_or(path)))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |j: usize| b.get(j).and_then(|c| (*c as char).to_digit(16));
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (hex(i + 1), hex(i + 2))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Libraries that decrypt Blu-rays are chosen with environment variables,
/// read by libbluray each time it opens a disc.
fn setup_bluray_decryption(s: &Settings) {
    if s.bluray_decryption == "makemkv" && std::env::var_os("LIBAACS_PATH").is_none() {
        // SAFETY: runs first thing in main, before any other thread exists.
        unsafe {
            std::env::set_var("LIBAACS_PATH", "libmmbd");
            std::env::set_var("LIBBDPLUS_PATH", "libmmbd");
        }
    }
}

/// On Linux, use X11 (XWayland on a Wayland desktop) when the user chose it:
/// winit only supports dropping files onto a window there.
fn select_backend(s: &Settings) {
    #[cfg(target_os = "linux")]
    if s.window_system == "x11" && std::env::var_os("DISPLAY").is_some() {
        use slint::winit_030::winit::event_loop::EventLoop;
        use slint::winit_030::winit::platform::x11::EventLoopBuilderExtX11;
        let mut builder = EventLoop::<slint::winit_030::SlintEvent>::with_user_event();
        builder.with_x11();
        if let Err(e) = slint::BackendSelector::new()
            .backend_name("winit".into())
            .with_winit_event_loop_builder(builder)
            .select()
        {
            log::warn!("could not switch to X11: {e}");
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = s;
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("warn,super_media_player=info,player_core=info"),
    )
    .init();
    let args = parse_args();
    let settings = Rc::new(RefCell::new(Settings::load()));
    // One player window: hand the files to the running player and leave.
    if settings.borrow().single_instance
        && !args.inputs.is_empty()
        && instance::forward(&args.inputs)
    {
        log::info!("sent {} item(s) to the running player", args.inputs.len());
        return Ok(());
    }
    setup_bluray_decryption(&settings.borrow());
    select_backend(&settings.borrow());

    let ui = AppWindow::new()?;
    // Lets the desktop match the window to super-media-player.desktop (icon, name).
    let _ = slint::set_xdg_app_id("super-media-player");
    ui.set_playlist_open(args.show_playlist);
    ui.set_playlist_detached(settings.borrow().playlist_detached);
    ui.set_bass(settings.borrow().bass as f32);
    ui.set_treble(settings.borrow().treble as f32);
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_app_author(env!("CARGO_PKG_AUTHORS").replace(':', ", ").into());
    ui.set_build_date(env!("SMP_BUILD_DATE").into());
    ui.set_build_year(env!("SMP_BUILD_YEAR").into());
    load_skins(&ui);
    settings_to_ui(&ui, &settings.borrow());
    apply_skin(&ui);
    {
        let weak = ui.as_weak();
        ui.on_reload_skins(move || {
            let Some(ui) = weak.upgrade() else { return };
            let id = chosen_skin(&ui);
            let warnings = load_skins(&ui);
            let view = pick(VIEW_MODES, ui.get_s_view_mode());
            ui.set_s_skin(skin_index(&id, &view));
            apply_skin(&ui);
            let count = SKINS.with_borrow(|s| s.len());
            ui.set_toast(match warnings.as_slice() {
                [] => format!("{count} skins loaded").into(),
                [w] => format!("{count} skins loaded. Skipped: {w}").into(),
                [w, rest @ ..] => format!(
                    "{count} skins loaded. Skipped: {w} (and {} more, see the log)",
                    rest.len()
                )
                .into(),
            });
        });
        let weak = ui.as_weak();
        ui.on_open_skins_folder(move || {
            let Some(ui) = weak.upgrade() else { return };
            match skins::prepare_user_dir(&ui).and_then(|dir| open_folder(&dir).map(|()| dir)) {
                Ok(dir) => ui.set_toast(
                    format!(
                        "Skins folder: {} (README.txt there shows how)",
                        dir.display()
                    )
                    .into(),
                ),
                Err(e) => ui.set_toast(format!("Couldn't open the skins folder: {e}").into()),
            }
        });
    }
    let view = settings.borrow().view_mode.clone();
    if let Some((w, h, _)) = skin_size(&view) {
        ui.window().set_size(slint::LogicalSize::new(w, h));
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(50), move || {
            if let Some(ui) = weak.upgrade() {
                resize_for_view(&ui, &view);
            }
        });
    }
    install_drop_handler(&ui);
    {
        let weak = ui.as_weak();
        ui.on_minimize_window(move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().set_minimized(true);
            }
        });
    }
    frame::install(&ui);
    // Closing the main window quits, even with the detached playlist open.
    ui.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });

    // The window (with its "Starting…" splash) is drawn first; the playback
    // engine, which loads libmpv and FFmpeg, starts right after that frame.
    let player: Rc<OnceCell<Rc<Player>>> = Rc::new(OnceCell::new());
    let video_active = Rc::new(Cell::new(false));
    let inputs = args.inputs;
    let on_first_frame = {
        let weak = ui.as_weak();
        let player = player.clone();
        let video_active = video_active.clone();
        let settings = settings.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                start_engine(&ui, &player, video_active, settings);
            }
        }
    };
    let on_ready = {
        let weak = ui.as_weak();
        let show_about = args.show_about;
        let show_settings = args.show_settings;
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if show_about {
                ui.invoke_show_about();
            }
            if show_settings {
                ui.invoke_show_settings();
            }
            if !inputs.is_empty() {
                open_inputs(
                    &ui,
                    inputs.iter().filter_map(|s| parse_input(s)).collect(),
                    false,
                );
            }
        }
    };
    VideoRenderer::install(
        &ui,
        player.clone(),
        video_active,
        args.snapshot,
        on_first_frame,
        on_ready,
    )?;

    if settings.borrow().single_instance {
        let weak = ui.as_weak();
        instance::listen(move |inputs| {
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let inputs: Vec<Input> = inputs.iter().filter_map(|s| parse_input(s)).collect();
                if !inputs.is_empty() {
                    open_inputs(&ui, inputs, false);
                }
                basics::raise(&ui);
            });
        });
    }

    // "Rip & play" copies a crashed or killed player left behind.
    std::thread::spawn(|| ripper::clean_leftovers(Duration::from_secs(120)));

    ui.run()?;
    instance::stop_listening();

    cast::shutdown(&ui, player.get().map(|p| &**p));
    // Stop playback before the window (and its GL context) goes away.
    if let Some(p) = player.get() {
        let _ = p.stop();
    }
    let mut s = settings.borrow_mut();
    s.volume = ui.get_volume() as f64;
    s.playlist_detached = ui.get_playlist_detached();
    s.bass = ui.get_bass() as f64;
    s.treble = ui.get_treble() as f64;
    s.save();
    // Shut mpv down here, in order, rather than in thread-local destructors
    // at process exit: drop every handle to the player, the window last.
    scripts::limit_shutdown();
    UI_STATE.with_borrow_mut(|s| *s = None);
    PLAYER.with_borrow_mut(|p| *p = None);
    drop(player);
    drop(ui);
    autocrop::tidy_up();
    discard_rips_on_exit();
    Ok(())
}

thread_local! {
    /// "Rip & play" copies made by this player, and whether each has
    /// started playing. Deleted once played (see [`watch_rips`]).
    static RIP_FILES: RefCell<Vec<(PathBuf, bool)>> = const { RefCell::new(Vec::new()) };
    /// The folder the running rip writes into.
    static RIP_DEST: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Delete each "Rip & play" copy once it has played: it ended, was stopped,
/// or something else is playing now. It also leaves the playlist, so no
/// entry points at a deleted file.
fn watch_rips(player: &Player) {
    if RIP_FILES.with_borrow(Vec::is_empty) {
        return;
    }
    let current = player.get_string("path").filter(|p| !p.is_empty());
    let looping = |name: &str| player.get_string(name).is_some_and(|v| v != "no");
    // Between two rounds of a loop "path" is briefly empty: not "done".
    let repeating = current.is_none() && (looping("loop-file") || looping("loop-playlist"));
    let done: Vec<PathBuf> = RIP_FILES.with_borrow_mut(|files| {
        let mut done = Vec::new();
        files.retain_mut(|(file, seen)| {
            let playing = current
                .as_deref()
                .is_some_and(|c| Path::new(c) == file.as_path());
            if playing {
                *seen = true;
                true
            } else if *seen && !repeating {
                done.push(file.clone());
                false
            } else {
                true
            }
        });
        done
    });
    for file in done {
        let count = player.get_double("playlist-count").unwrap_or(0.0) as usize;
        for i in (0..count).rev() {
            let entry = player.get_string(&format!("playlist/{i}/filename"));
            if entry.is_some_and(|e| Path::new(&e) == file.as_path()) {
                let _ = player.remove_index(i);
            }
        }
        ripper::discard(&file);
    }
}

/// Quitting: stop a rip in progress and delete every "Rip & play" copy.
fn discard_rips_on_exit() {
    if let Some(c) = RIP_CANCEL.with_borrow_mut(|c| c.take()) {
        c.cancel();
    }
    if let Some(dir) = RIP_DEST.with_borrow_mut(|d| d.take()) {
        ripper::discard_dir(&dir);
    }
    for (file, _) in RIP_FILES.with_borrow_mut(std::mem::take) {
        ripper::discard(&file);
    }
}

/// mpv options that follow the audio settings: gapless playback,
/// ReplayGain, exclusive output for bit-perfect mode.
fn audio_options(s: &Settings) -> [(&'static str, &'static str); 6] {
    [
        ("gapless-audio", if s.gapless { "weak" } else { "no" }),
        // Open the next song early, so it starts without a pause.
        ("prefetch-playlist", if s.gapless { "yes" } else { "no" }),
        (
            "replaygain",
            match s.replaygain.as_str() {
                "track" => "track",
                "album" => "album",
                _ => "no",
            },
        ),
        // The limiter in the tone filter catches peaks; mpv's own clipping
        // guard would turn quiet albums down instead.
        ("replaygain-clip", "no"),
        ("audio-exclusive", if s.bit_perfect { "yes" } else { "no" }),
        (
            "audio-spdif",
            match s.passthrough.as_str() {
                "spdif" => "ac3,dts",
                "hdmi" => "ac3,eac3,dts,dts-hd,truehd",
                _ => "",
            },
        ),
    ]
}

/// Equalizer presets: name and gains (dB) for `settings::EQ_BANDS`.
const EQ_PRESETS: &[(&str, [f64; 10])] = &[
    ("Flat", [0.0; 10]),
    (
        "Bass boost",
        [6.0, 5.0, 4.0, 2.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0],
    ),
    (
        "Treble boost",
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 2.0, 4.0, 5.0, 6.0],
    ),
    (
        "Loudness",
        [5.0, 4.0, 2.0, 0.0, -1.0, -1.0, 0.0, 2.0, 4.0, 5.0],
    ),
    (
        "Rock",
        [4.0, 3.0, 1.5, -0.5, -1.5, -0.5, 1.5, 3.0, 3.5, 4.0],
    ),
    ("Pop", [-1.0, 0.0, 1.5, 3.0, 3.5, 2.5, 1.0, 0.0, -0.5, -1.0]),
    ("Jazz", [3.0, 2.0, 1.0, 1.5, -1.0, -1.0, 0.0, 1.5, 2.5, 3.0]),
    (
        "Classical",
        [3.5, 2.5, 1.5, 1.0, -0.5, -0.5, 0.0, 1.5, 2.5, 3.0],
    ),
    (
        "Electronic",
        [5.0, 4.0, 1.5, 0.0, -1.5, 1.0, 0.5, 1.5, 4.0, 4.5],
    ),
    (
        "Vocal",
        [-2.0, -1.5, -0.5, 1.5, 3.5, 3.5, 2.5, 1.0, 0.0, -1.0],
    ),
    (
        "Small speakers",
        [-3.0, 1.0, 4.0, 3.0, 1.0, 0.0, 0.5, 1.5, 1.0, 0.0],
    ),
];

/// Create the player, wire up the UI and hide the startup splash. On failure
/// (usually: libmpv not found) tell the user in a dialog, since a program
/// started from the desktop has no terminal to print to.
fn start_engine(
    ui: &AppWindow,
    slot: &OnceCell<Rc<Player>>,
    video_active: Rc<Cell<bool>>,
    settings: Rc<RefCell<Settings>>,
) {
    let mut config = PlayerConfig::default();
    {
        let s = settings.borrow();
        // Cover art is not drawn as "video"; the UI has its own music view.
        config.set("audio-display", "no");
        config.set(
            "hwdec",
            if s.hardware_decoding {
                "auto-safe"
            } else {
                "no"
            },
        );
        config.set("ytdl-format", &s.ytdl_format());
        for (k, v) in scripts::options(&s) {
            config.set(k, &v);
        }
        // Dev aids: mpv's own detailed log, and any of its options
        // ("scale=bilinear;dither=no").
        if let Ok(file) = std::env::var("SMP_MPV_LOG") {
            config.set("log-file", &file);
        }
        if let Ok(list) = std::env::var("SMP_MPV_OPTIONS") {
            for (k, v) in list.split(';').filter_map(|o| o.split_once('=')) {
                config.set(k.trim(), v.trim());
            }
        }
        for (k, v) in audio_options(&s) {
            config.set(k, v);
        }
        config.set("volume", &s.volume.round().to_string());
        if let Some(ytdlp) = tools::find_ytdlp() {
            log::info!("yt-dlp: {}", ytdlp.display());
            config.set(
                "script-opts",
                &format!("ytdl_hook-ytdl_path={}", ytdlp.display()),
            );
        }
    }

    let weak = ui.as_weak();
    let player = match Player::new(&config, move |event| {
        let _ = weak.upgrade_in_event_loop(move |ui| {
            UI_STATE.with_borrow_mut(|s| {
                if let Some(s) = s.as_mut() {
                    s.apply(&ui, event);
                }
            });
        });
    }) {
        Ok(p) => Rc::new(p),
        Err(e) => {
            log::error!("{e}");
            eprintln!("Super Media Player could not start the playback engine:\n{e}");
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("Super Media Player")
                .set_description(format!(
                    "Super Media Player could not start its playback engine.\n\n{e}"
                ))
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
            let _ = slint::quit_event_loop();
            return;
        }
    };
    let mpv_version = player.get_string("mpv-version").unwrap_or_default();
    let ffmpeg_version = player.get_string("ffmpeg-version").unwrap_or_default();
    log::info!("{mpv_version}, FFmpeg {ffmpeg_version}");
    // "6.1.1-3ubuntu5" → "6.1.1": distro patch levels only add noise.
    let ffmpeg_short = ffmpeg_version.split('-').next().unwrap_or_default();
    ui.set_engine_info(format!("{mpv_version} · FFmpeg {ffmpeg_short}").into());

    UI_STATE.with_borrow_mut(|s| {
        *s = Some(UiState::new(video_active, player.clone(), settings.clone()))
    });
    PLAYER.with_borrow_mut(|p| *p = Some(player.clone()));
    connect_callbacks(ui, &player, settings.clone());
    features::install(ui, &player, settings.clone());
    cast::install(ui, &player, settings.clone());
    dvdmenu::install(ui, &player);
    basics::install(ui, &player, settings.clone());
    playlists::install(ui, &player);
    shortcuts::install(ui, settings.clone());
    opensubs::install(ui, &player, settings.clone());
    library::install(ui, &player, settings.clone());
    collection::install(ui, &player, settings.clone());
    start_meter(ui, &player, settings.clone());
    resume::start(ui, &player, settings.clone());
    pace::install(ui, &player, settings.clone());
    silence::install(ui, &player, settings.clone());
    autocrop::install(ui, &player);
    scripts::install(ui, &player, settings.clone());
    let _ = slot.set(player);
    ui.set_starting(false);
    refresh_tool_status(ui);
    run_system_check(ui, &settings.borrow(), true);
    schedule_auto_update(ui, settings);
    // The renderer creates mpv's render context on the next frame.
    ui.window().request_redraw();
}

fn index_of(list: &[&str], value: &str) -> i32 {
    list.iter().position(|v| *v == value).unwrap_or(0) as i32
}

fn pick(list: &[&str], index: i32) -> String {
    list.get(index.max(0) as usize)
        .unwrap_or(&list[0])
        .to_string()
}

fn settings_to_ui(ui: &AppWindow, s: &Settings) {
    ui.set_s_view_mode(index_of(VIEW_MODES, &s.view_mode));
    ui.set_s_skin(skin_index(&s.skin, &s.view_mode));
    ui.set_s_visualizer(index_of(settings::VISUALIZERS, &s.visualizer));
    ui.set_s_artist_pos(index_of(ARTIST_POSITIONS, &s.artist_image));
    ui.set_s_artist_online(s.artist_image_online);
    ui.set_s_pulse_line(s.pulse_line);
    ui.set_s_lyrics_online(s.lyrics_online);
    ui.set_s_show_lyrics(s.show_lyrics);
    ui.set_s_gapless(s.gapless);
    ui.set_s_crossfade(
        settings::CROSSFADES
            .iter()
            .position(|c| (*c - s.crossfade).abs() < 0.5)
            .unwrap_or(0) as i32,
    );
    ui.set_s_replaygain(index_of(settings::REPLAYGAIN, &s.replaygain));
    ui.set_s_resume(s.resume);
    ui.set_s_remember_pace(s.remember_pace);
    ui.set_s_bit_perfect(s.bit_perfect);
    ui.set_s_eq_enabled(s.eq_enabled);
    ui.set_s_stream_quality(index_of(settings::QUALITIES, &s.stream_quality));
    ui.set_s_hw_decoding(s.hardware_decoding);
    ui.set_s_bluray_decryption(index_of(BLURAY_DECRYPTION, &s.bluray_decryption));
    ui.set_s_window_system(index_of(WINDOW_SYSTEMS, &s.window_system));
    ui.set_s_auto_update(s.auto_update);
    ui.set_s_night_mode(s.night_mode);
    ui.set_s_dialogue_boost(s.dialogue_boost);
    ui.set_s_seek_thumbnails(s.seek_thumbnails);
    ui.set_s_skip_intro(index_of(settings::SKIP_INTRO, &s.skip_intro));
    ui.set_s_sponsor_skip(index_of(settings::SKIP_INTRO, &s.sponsor_skip));
    ui.set_s_sponsor_kinds(index_of(settings::SPONSOR_KINDS, &s.sponsor_kinds));
    ui.set_s_next_episode(s.next_episode);
    ui.set_s_video_enhance(index_of(settings::ENHANCE, &s.video_enhance));
    ui.set_s_smooth_motion(s.smooth_motion);
    ui.set_s_auto_crop(s.auto_crop);
    ui.set_s_tone_map(index_of(settings::TONE_MAPS, &s.hdr_tone_map));
    ui.set_s_hdr_peak(s.hdr_peak_detect);
    ui.set_s_cd_online(s.cd_online);
    ui.set_s_whisper_model(index_of(settings::WHISPER_MODELS, &s.whisper_model));
    ui.set_s_nav_enabled(s.nav_enabled);
    ui.set_s_nav_docked(s.nav_docked);
    ui.set_s_always_on_top(s.always_on_top);
    ui.set_s_title_bar(index_of(settings::TITLE_BARS, &s.title_bar));
    ui.set_s_single_instance(s.single_instance);
    ui.set_s_sub_size(index_of(settings::SUB_SIZES, &s.sub_size));
    ui.set_s_sub_pos(index_of(settings::SUB_POSITIONS, &s.sub_pos));
    ui.set_s_sub_color(index_of(settings::SUB_COLORS, &s.sub_color));
    ui.set_s_sub_style(index_of(settings::SUB_STYLES, &s.sub_style));
    ui.set_s_passthrough(index_of(settings::PASSTHROUGH, &s.passthrough));
    ui.set_s_dvd_menus(s.dvd_menus);
    ui.set_s_sub_auto_sync(s.sub_auto_sync);
    ui.set_s_scripts(s.scripts);
    ui.set_s_mouse_wheel(index_of(settings::MOUSE_WHEEL, &s.mouse_wheel));
    ui.set_s_mouse_double(index_of(settings::MOUSE_DOUBLE, &s.mouse_double));
    ui.set_s_mouse_middle(index_of(settings::MOUSE_MIDDLE, &s.mouse_middle));
}

fn settings_from_ui(ui: &AppWindow, old: &Settings) -> Settings {
    Settings {
        view_mode: pick(VIEW_MODES, ui.get_s_view_mode()),
        skin: chosen_skin(ui),
        visualizer: pick(settings::VISUALIZERS, ui.get_s_visualizer()),
        artist_image: pick(ARTIST_POSITIONS, ui.get_s_artist_pos()),
        artist_image_online: ui.get_s_artist_online(),
        pulse_line: ui.get_s_pulse_line(),
        lyrics_online: ui.get_s_lyrics_online(),
        show_lyrics: ui.get_s_show_lyrics(),
        gapless: ui.get_s_gapless(),
        crossfade: settings::CROSSFADES
            .get(ui.get_s_crossfade().max(0) as usize)
            .copied()
            .unwrap_or(0.0),
        replaygain: pick(settings::REPLAYGAIN, ui.get_s_replaygain()),
        resume: ui.get_s_resume(),
        remember_pace: ui.get_s_remember_pace(),
        bit_perfect: ui.get_s_bit_perfect(),
        eq_enabled: ui.get_s_eq_enabled(),
        eq_gains: old.eq_gains,
        bluray_decryption: pick(BLURAY_DECRYPTION, ui.get_s_bluray_decryption()),
        stream_quality: pick(settings::QUALITIES, ui.get_s_stream_quality()),
        hardware_decoding: ui.get_s_hw_decoding(),
        window_system: pick(WINDOW_SYSTEMS, ui.get_s_window_system()),
        volume: old.volume,
        auto_update: ui.get_s_auto_update(),
        playlist_detached: old.playlist_detached,
        bass: old.bass,
        treble: old.treble,
        night_mode: ui.get_s_night_mode(),
        dialogue_boost: ui.get_s_dialogue_boost(),
        seek_thumbnails: ui.get_s_seek_thumbnails(),
        skip_intro: pick(settings::SKIP_INTRO, ui.get_s_skip_intro()),
        sponsor_skip: pick(settings::SKIP_INTRO, ui.get_s_sponsor_skip()),
        sponsor_kinds: pick(settings::SPONSOR_KINDS, ui.get_s_sponsor_kinds()),
        next_episode: ui.get_s_next_episode(),
        video_enhance: pick(settings::ENHANCE, ui.get_s_video_enhance()),
        smooth_motion: ui.get_s_smooth_motion(),
        auto_crop: ui.get_s_auto_crop(),
        hdr_tone_map: pick(settings::TONE_MAPS, ui.get_s_tone_map()),
        hdr_peak_detect: ui.get_s_hdr_peak(),
        cd_online: ui.get_s_cd_online(),
        whisper_model: pick(settings::WHISPER_MODELS, ui.get_s_whisper_model()),
        milkdrop_seconds: old.milkdrop_seconds,
        nav_enabled: ui.get_s_nav_enabled(),
        nav_docked: ui.get_s_nav_docked(),
        nav_step_ms: old.nav_step_ms,
        nav_turn: old.nav_turn,
        nav_pitch: old.nav_pitch,
        nav_jog_sound: old.nav_jog_sound,
        nav_step_sound: old.nav_step_sound,
        audio_device: basics::device_name(ui.get_s_audio_device())
            .unwrap_or_else(|| old.audio_device.clone()),
        always_on_top: ui.get_s_always_on_top(),
        title_bar: pick(settings::TITLE_BARS, ui.get_s_title_bar()),
        single_instance: ui.get_s_single_instance(),
        sub_size: pick(settings::SUB_SIZES, ui.get_s_sub_size()),
        sub_pos: pick(settings::SUB_POSITIONS, ui.get_s_sub_pos()),
        sub_color: pick(settings::SUB_COLORS, ui.get_s_sub_color()),
        sub_style: pick(settings::SUB_STYLES, ui.get_s_sub_style()),
        passthrough: pick(settings::PASSTHROUGH, ui.get_s_passthrough()),
        dvd_menus: ui.get_s_dvd_menus(),
        mouse_wheel: pick(settings::MOUSE_WHEEL, ui.get_s_mouse_wheel()),
        mouse_double: pick(settings::MOUSE_DOUBLE, ui.get_s_mouse_double()),
        mouse_middle: pick(settings::MOUSE_MIDDLE, ui.get_s_mouse_middle()),
        cast_quality: old.cast_quality.clone(),
        cast_burn_subs: old.cast_burn_subs,
        cast_address: old.cast_address.clone(),
        sub_languages: old.sub_languages.clone(),
        sub_auto_sync: ui.get_s_sub_auto_sync(),
        scripts: ui.get_s_scripts(),
        scripts_off: old.scripts_off.clone(),
        radio_country: old.radio_country.clone(),
    }
}

/// Apply changed settings to the running player and save them.
fn apply_settings(ui: &AppWindow, player: &Player, settings: &RefCell<Settings>) {
    let old = settings.borrow().clone();
    let new = settings_from_ui(ui, &old);
    if new == old {
        return;
    }
    *settings.borrow_mut() = new.clone();
    new.save();

    if new.view_mode != old.view_mode {
        resize_for_view(ui, &new.view_mode);
    }
    if new.skin != old.skin || new.view_mode != old.view_mode {
        apply_skin(ui);
    }
    if new.hardware_decoding != old.hardware_decoding {
        report(
            ui,
            player.set_string(
                "hwdec",
                if new.hardware_decoding {
                    "auto-safe"
                } else {
                    "no"
                },
            ),
        );
    }
    if audio_options(&new) != audio_options(&old) {
        for (k, v) in audio_options(&new) {
            if let Err(e) = player.set_string(k, v) {
                log::warn!("{k}: {e}");
            }
        }
    }
    if new.bit_perfect != old.bit_perfect {
        ui.set_toast(
            if new.bit_perfect {
                "Bit-perfect output on: the tone controls, equalizer, fades and level bars are off. It takes effect from the next song."
            } else {
                "Bit-perfect output off: tone controls, equalizer and fades are back from the next song."
            }
            .into(),
        );
    }
    if new.stream_quality != old.stream_quality {
        report(ui, player.set_string("ytdl-format", &new.ytdl_format()));
    }
    if new.visualizer != old.visualizer
        || new.artist_image != old.artist_image
        || new.artist_image_online != old.artist_image_online
        || new.lyrics_online != old.lyrics_online
    {
        let lookups_changed = new.artist_image_online != old.artist_image_online
            || new.lyrics_online != old.lyrics_online;
        UI_STATE.with_borrow_mut(|s| {
            if let Some(s) = s.as_mut() {
                s.update_visual(ui);
                s.update_artist_image(ui);
                if lookups_changed {
                    s.refresh_music_info(ui);
                }
            }
        });
    }
    basics::settings_changed(ui, player, &old, &new);
    if new.single_instance != old.single_instance || new.scripts != old.scripts {
        ui.set_toast("Saved. This setting takes effect the next time the player starts.".into());
    }
    if new.bluray_decryption != old.bluray_decryption || new.window_system != old.window_system {
        ui.set_toast("Saved. This setting takes effect the next time the player starts.".into());
    }
}

/// Size the window for the view: the skinned players are small, and grow
/// downwards when their playlist is open. Within one skin the user's width
/// and playlist height are kept; only switching skins resets them.
fn resize_for_view(ui: &AppWindow, mode: &str) {
    thread_local! {
        static PLAYER_SIZE: Cell<Option<slint::LogicalSize>> = const { Cell::new(None) };
        static LAST_MODE: RefCell<String> = const { RefCell::new(String::new()) };
    }
    let window = ui.window();
    if ui.get_fullscreen() {
        window.set_fullscreen(false);
        ui.set_fullscreen(false);
        with_player(|p| {
            let _ = p.set_flag("fullscreen", false);
        });
    }
    let scale = window.scale_factor();
    let before_panel = BEFORE_PANEL.take();
    let same_skin = LAST_MODE.with_borrow_mut(|m| std::mem::replace(m, mode.to_owned()) == mode);
    let docked = ui.get_playlist_open() && !playlist_in_own_window(ui, mode);
    if let Some((skin_w, skin_h, max_w)) = skin_size(mode) {
        // Not the size it grew to for a panel: `fit_panel` grows it again.
        let current = before_panel.unwrap_or_else(|| window.size().to_logical(scale));
        // Coming from the full player (bigger than any skin): remember its
        // size for the way back.
        if !same_skin
            && (current.width > 760.0 || current.height > 340.0 + SKIN_PLAYLIST_HEIGHT + 20.0)
        {
            PLAYER_SIZE.with(|s| s.set(Some(current)));
        }
        let pl_h = PLAYLIST_HEIGHT.with(|h| h.get());
        let h = skin_h + if docked { pl_h } else { 0.0 };
        let w = if same_skin && mode != "silver" {
            current.width.max(skin_w)
        } else {
            skin_w.max(current.width.min(max_w))
        };
        window.set_size(slint::LogicalSize::new(w, h));
    } else {
        let size = PLAYER_SIZE
            .with(|s| s.get())
            .unwrap_or(slint::LogicalSize::new(1100.0, 680.0));
        window.set_size(size);
    }
}

/// The classic deck can show its playlist in a separate window instead of
/// under the deck.
fn playlist_in_own_window(ui: &AppWindow, mode: &str) -> bool {
    mode == "classic" && ui.get_playlist_detached()
}

/// "1:02:05" or "3:07".
fn fmt_clock(t: f64) -> String {
    let t = t.max(0.0) as u64;
    if t >= 3600 {
        format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
    } else {
        format!("{}:{:02}", t / 60, t % 60)
    }
}

/// Log an error from a player call and show it as a toast.
fn report(ui: &AppWindow, r: player_core::Result<()>) {
    if let Err(e) = r {
        log::warn!("{e}");
        ui.set_toast(e.to_string().into());
    }
}

fn connect_callbacks(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    // Small helper: clone the player and a weak UI handle into a callback.
    macro_rules! on {
        ($setter:ident, |$ui:ident, $p:ident $(, $arg:ident)*| $body:expr) => {{
            let $p = player.clone();
            let weak = ui.as_weak();
            ui.$setter(move |$($arg),*| {
                let Some($ui) = weak.upgrade() else { return };
                #[allow(unused_variables)]
                let $p = &$p;
                $body
            });
        }};
    }

    on!(on_toggle_pause, |ui, p| {
        if ui.get_idle() {
            open_files_dialog(&ui);
        } else if !cast::toggle_pause(&ui) {
            report(&ui, p.toggle_pause())
        }
    });
    on!(on_stop, |ui, p| report(&ui, p.stop()));
    // A-B repeat: the first press marks A, the second B (then it loops
    // between them), the third clears it.
    on!(on_ab_loop, |ui, p| {
        report(&ui, p.command(&["ab-loop"]));
        let a = p.get_double("ab-loop-a");
        let b = p.get_double("ab-loop-b");
        let t = |x: f64| fmt_clock(x);
        ui.set_toast(
            match (a, b) {
                (Some(a), Some(b)) => {
                    format!("Repeating {} – {}. Press A again to stop.", t(a), t(b))
                }
                (Some(a), None) => format!("Loop start set at {}. Press A again at the end.", t(a)),
                _ => "A-B repeat off.".to_owned(),
            }
            .into(),
        );
    });
    // One frame forward or back (pauses the video).
    on!(on_frame_step, |ui, p, forward| {
        if cast::refuses(&ui, "Stepping by frame") {
            return;
        }
        let cmd = if forward {
            "frame-step"
        } else {
            "frame-back-step"
        };
        report(&ui, p.command(&[cmd]));
    });
    // On a DVD's menu and clips these skip within the disc.
    on!(on_next, |ui, p| if dvdmenu::menu_active() {
        ui.invoke_dvd("skip".into())
    } else {
        report(&ui, p.next())
    });
    on!(on_prev, |ui, p| if dvdmenu::menu_active() {
        ui.invoke_dvd("back".into())
    } else {
        report(&ui, p.prev())
    });
    on!(on_seek, |ui, p, t| {
        if !cast::seek_to(&ui, t as f64) {
            report(&ui, p.seek(Seek::Absolute(t as f64)))
        }
    });
    on!(on_scrub, |ui, p, t| {
        if !cast::seek_to(&ui, t as f64) {
            report(
                &ui,
                p.command(&["seek", &t.to_string(), "absolute+keyframes"]),
            )
        }
    });
    on!(on_seek_relative, |ui, p, t| {
        if !ui.get_idle() && !cast::seek_relative(&ui, t as f64) {
            report(&ui, p.seek(Seek::Relative(t as f64)))
        }
    });
    on!(on_set_volume, |ui, p, v| {
        if cast::set_volume(&ui, v) {
            return;
        }
        report(&ui, p.set_volume(v as f64));
        if ui.get_muted() {
            report(&ui, p.set_mute(false));
        }
    });
    on!(on_toggle_mute, |ui, p| {
        if !cast::toggle_mute(&ui) {
            report(&ui, p.set_mute(!ui.get_muted()))
        }
    });
    on!(on_play_index, |ui, p, i| report(
        &ui,
        p.play_index(i.max(0) as usize)
    ));
    on!(on_remove_index, |ui, p, i| report(
        &ui,
        p.remove_index(i.max(0) as usize)
    ));
    on!(on_clear_playlist, |ui, p| report(&ui, p.stop()));
    on!(on_toggle_shuffle, |ui, p| {
        let on = !ui.get_shuffle();
        report(&ui, p.shuffle(on));
        ui.set_shuffle(on);
    });
    // 0 = off, 1 = whole playlist, 2 = current item
    on!(on_set_repeat, |ui, p, mode| {
        report(&ui, p.set_loop_playlist(mode == 1));
        report(&ui, p.set_loop_file(mode == 2));
    });
    on!(on_select_audio, |ui, p, id| report(
        &ui,
        p.select_track(TrackKind::Audio, Some(id as i64))
    ));
    on!(on_select_subtitle, |ui, p, id| {
        report(
            &ui,
            p.select_track(TrackKind::Subtitle, (id >= 0).then_some(id as i64)),
        )
    });
    on!(on_screenshot, |ui, p| {
        if !ui.get_has_video() {
            return;
        }
        match p.screenshot() {
            Ok(()) => ui.set_toast("Screenshot saved to the desktop".into()),
            Err(e) => report(&ui, Err(e)),
        }
    });
    on!(on_toggle_fullscreen, |ui, p| {
        let fs = !ui.window().is_fullscreen();
        ui.window().set_fullscreen(fs);
        ui.set_fullscreen(fs);
        // For scripts, which ask and change it there.
        let _ = p.set_flag("fullscreen", fs);
    });
    on!(on_open_files, |ui, p| open_files_dialog(&ui));
    on!(on_open_folder, |ui, p| {
        if let Some(dir) = rfd::FileDialog::new()
            .set_title("Open folder")
            .pick_folder()
        {
            open_inputs(&ui, vec![Input::Path(dir)], !ui.get_idle());
        }
    });
    on!(on_open_location, |ui, p, text| {
        let inputs: Vec<Input> = text.lines().filter_map(parse_input).collect();
        open_inputs(&ui, inputs, false);
    });
    on!(on_add_subtitle, |ui, p| {
        if let Some(file) = rfd::FileDialog::new()
            .set_title("Load subtitle")
            .add_filter("Subtitles", SUBTITLE_EXTENSIONS)
            .add_filter("All files", &["*"])
            .pick_file()
        {
            report(&ui, p.add_subtitle(&file.to_string_lossy()));
        }
    });
    on!(on_load_subtitle_path, |ui, p, path| report(
        &ui,
        p.add_subtitle(&path)
    ));
    on!(on_open_disc, |ui, p, kind| open_disc(&ui, &kind));
    on!(on_quit, |ui, p| {
        let _ = ui.hide();
        let _ = slint::quit_event_loop();
    });
    on!(on_install_ytdlp, |ui, p| install_ytdlp(&ui, None));
    on!(on_install_keydb, |ui, p| install_keydb(&ui));
    on!(on_ytdlp_missing, |ui, p, url| {
        let yes = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Info)
            .set_title("yt-dlp is needed")
            .set_description(
                "This link needs yt-dlp, the free tool that reads YouTube and ~1800 other sites.\n\n\
                 Download it now from github.com/yt-dlp (about 35 MB)? The download is checked \
                 against the checksums published with each release.",
            )
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if yes == rfd::MessageDialogResult::Yes {
            install_ytdlp(&ui, Some(url.to_string()));
        }
    });

    {
        let weak = ui.as_weak();
        let settings = settings.clone();
        ui.on_run_check(move || {
            if let Some(ui) = weak.upgrade() {
                run_system_check(&ui, &settings.borrow(), false);
            }
        });
        let weak = ui.as_weak();
        ui.on_update_ffmpeg(move || {
            if let Some(ui) = weak.upgrade() {
                update_ffmpeg(&ui);
            }
        });
        let weak = ui.as_weak();
        ui.on_rip_disc(move || {
            if let Some(ui) = weak.upgrade() {
                rip_disc(&ui);
            }
        });
        let weak = ui.as_weak();
        ui.on_register_makemkv(move |key| {
            if let Some(ui) = weak.upgrade() {
                register_makemkv(&ui, key.to_string());
            }
        });
        ui.set_makemkv_install_command(ripper::FLATPAK_INSTALL.into());
        ui.set_makemkv_update_command(ripper::FLATPAK_UPDATE.into());
        ui.set_makemkv_key_page(ripper::KEY_PAGE.into());
        let weak = ui.as_weak();
        ui.on_refresh_makemkv(move || {
            if let Some(ui) = weak.upgrade() {
                refresh_makemkv(&ui);
            }
        });
        let weak = ui.as_weak();
        ui.on_install_makemkv(move || {
            let Some(ui) = weak.upgrade() else { return };
            run_busy(
                &ui,
                "Installing MakeMKV from Flathub (a few minutes)…",
                ripper::install_flatpak,
                |ui, result| {
                    match result {
                        Ok(()) => ui.set_toast("MakeMKV is installed".into()),
                        Err(e) => ui.set_toast(format!("Couldn't install MakeMKV: {e}").into()),
                    }
                    refresh_makemkv(ui);
                },
            );
        });
        ui.on_open_url(|url| {
            // Only web pages: this hands the address to the desktop's browser.
            if url.starts_with("https://")
                && let Err(e) = open_folder(Path::new(url.as_str()))
            {
                log::warn!("could not open {url}: {e}");
            }
        });
        let weak = ui.as_weak();
        let weak_rf = ui.as_weak();
        ui.on_rip_files(move || {
            if let Some(ui) = weak_rf.upgrade() {
                rip_files(&ui);
            }
        });
        setup_rip_files(ui);
        ui.on_rip_from_drive(move |device, name| {
            if let Some(ui) = weak.upgrade() {
                rip_from(&ui, device.to_string(), name.to_string());
            }
        });
        let weak = ui.as_weak();
        ui.on_play_from_drive(move |device| {
            let Some(ui) = weak.upgrade() else { return };
            let plan = DISC_CHOICES.with_borrow_mut(|c| {
                let i = c.iter().position(|p| p.device == device.as_str())?;
                let plan = c.swap_remove(i);
                c.clear();
                Some(plan)
            });
            match plan {
                Some(plan) => with_player(|p| play_disc(&ui, p, plan)),
                None => log::warn!("drive picker: no disc found for {device:?}"),
            }
        });
        let weak = ui.as_weak();
        ui.on_cancel_busy(move || {
            let Some(ui) = weak.upgrade() else { return };
            if let Some(c) = RIP_CANCEL.with_borrow(|c| c.clone()) {
                c.cancel();
            } else if ui.get_rip_active() {
                // Still looking at the disc: forget the search (MakeMKV or
                // the drive finishes in the background) and free the player.
                RIP_SCAN.set(RIP_SCAN.get() + 1);
                ui.set_rip_active(false);
                ui.set_busy_text("".into());
                ui.set_toast("Cancelled".into());
            }
        });
        let weak = ui.as_weak();
        ui.on_ask_answer(move |yes| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_ask_open(false);
            if let Some(then) = ASK.with_borrow_mut(|a| a.take()) {
                then(&ui, yes);
            }
        });
        let weak = ui.as_weak();
        ui.on_check_action(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            match id.as_str() {
                "install-ytdlp" => install_ytdlp(&ui, None),
                "install-keydb" => install_keydb(&ui),
                "update-ffmpeg" => {
                    ui.set_check_open(false);
                    update_ffmpeg(&ui);
                }
                "open-settings" => {
                    ui.set_check_open(false);
                    ui.invoke_show_settings();
                }
                other => log::warn!("unknown system check action {other}"),
            }
        });
    }
    {
        let p = player.clone();
        let weak = ui.as_weak();
        let settings = settings.clone();
        ui.on_settings_changed(move || {
            if let Some(ui) = weak.upgrade() {
                apply_settings(&ui, &p, &settings);
            }
        });
    }
    // The skinned windows grow to show their playlist; the classic deck's
    // can live in a window of its own instead.
    {
        let weak = ui.as_weak();
        let settings = settings.clone();
        let pl_window = playlist_window(ui, player);
        let last = Cell::new((ui.get_playlist_open(), ui.get_playlist_detached()));
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(150),
            move || {
                let Some(ui) = weak.upgrade() else { return };
                with_player(|p| watch_rips(p));
                let view = settings.borrow().view_mode.clone();
                let now = (ui.get_playlist_open(), ui.get_playlist_detached());
                if now != last.replace(now) && view != "player" {
                    resize_for_view(&ui, &view);
                } else if !fit_panel(&ui) {
                    note_playlist_height(&ui, &view);
                }
                let Some(pw) = &pl_window else { return };
                let show = now.0 && playlist_in_own_window(&ui, &view);
                if show {
                    // Same model as the deck's: a cheap pointer compare when unchanged.
                    pw.set_items(ui.get_playlist());
                    pw.set_playing(!ui.get_idle());
                    if !pw.window().is_visible() {
                        // Open it right under the deck, like a torn-off panel.
                        // (Wayland doesn't let apps place windows; it picks.)
                        let scale = ui.window().scale_factor();
                        let pos = ui.window().position().to_logical(scale);
                        let size = ui.window().size().to_logical(scale);
                        pw.window().set_position(slint::LogicalPosition::new(
                            pos.x,
                            pos.y + size.height + 4.0,
                        ));
                        pw.window()
                            .set_size(slint::LogicalSize::new(size.width, SKIN_PLAYLIST_HEIGHT));
                        let _ = pw.show();
                    }
                } else if pw.window().is_visible() {
                    let _ = pw.hide();
                }
            },
        );
        std::mem::forget(timer);
    }
}

/// The classic deck's detached playlist window (shown by the timer above).
fn playlist_window(ui: &AppWindow, player: &Rc<Player>) -> Option<PlaylistWindow> {
    let pw = match PlaylistWindow::new() {
        Ok(w) => w,
        Err(e) => {
            log::warn!("playlist window: {e}");
            return None;
        }
    };
    attach_drop_handler(pw.window(), ui.as_weak(), true);
    let p = player.clone();
    pw.on_play_index(move |i| {
        let _ = p.play_index(i.max(0) as usize);
    });
    let p = player.clone();
    pw.on_remove_index(move |i| {
        let _ = p.remove_index(i.max(0) as usize);
    });
    let weak = ui.as_weak();
    pw.on_dock(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_playlist_detached(false);
        }
    });
    let weak = ui.as_weak();
    pw.on_open_files(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_open_files();
        }
    });
    // Closing it is the same as the deck's PL button: hide the playlist.
    let weak = ui.as_weak();
    pw.window().on_close_requested(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_playlist_open(false);
        }
        slint::CloseRequestResponse::HideWindow
    });
    PLAYLIST_WIN.with_borrow_mut(|w| *w = Some(pw.as_weak()));
    apply_skin(ui);
    Some(pw)
}

/// Drive the spectrum bars from the music itself (meter.rs), ~30 times a
/// second. Only measures while bars are on screen: music without a video
/// track and without an mpv visualizer.
fn start_meter(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    use slint::Model;
    let model = Rc::new(slint::VecModel::from(vec![0.0f32; meter::BANDS]));
    ui.set_spectrum(model.clone().into());
    let mut meter = meter::Meter::default();
    meter.tone.set(meter::Tone {
        bass: ui.get_bass() as f64,
        treble: ui.get_treble() as f64,
        ..Default::default()
    });
    // The silver deck's knobs: change the sound now, and remember for the
    // next file.
    {
        let (tone, p) = (meter.tone.clone(), player.clone());
        ui.on_set_bass(move |db| {
            let mut t = tone.get();
            t.bass = db as f64;
            tone.set(t);
            meter::set_tone(&p, t.bass, t.treble);
        });
        let (tone, p) = (meter.tone.clone(), player.clone());
        ui.on_set_treble(move |db| {
            let mut t = tone.get();
            t.treble = db as f64;
            tone.set(t);
            meter::set_tone(&p, t.bass, t.treble);
        });
    }
    // The equalizer: gains live in the settings (saved at once), and the
    // playing song hears a change immediately.
    let eq_model = Rc::new(slint::VecModel::from(
        settings
            .borrow()
            .eq_gains
            .iter()
            .map(|g| *g as f32)
            .collect::<Vec<_>>(),
    ));
    ui.set_eq_gains(eq_model.clone().into());
    ui.set_eq_preset_names(
        Rc::new(slint::VecModel::from(
            EQ_PRESETS
                .iter()
                .map(|(n, _)| slint::SharedString::from(*n))
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    {
        let (st, p, m) = (settings.clone(), player.clone(), eq_model.clone());
        ui.on_set_eq_band(move |band, db| {
            let (band, db) = (band.max(0) as usize, (db as f64).clamp(-12.0, 12.0));
            let Some(slot) = st.borrow_mut().eq_gains.get_mut(band).map(|g| {
                *g = (db * 2.0).round() / 2.0;
                *g
            }) else {
                return;
            };
            m.set_row_data(band, slot as f32);
            meter::set_eq_band(&p, band, slot);
            st.borrow().save();
        });
        let (st, p, m) = (settings.clone(), player.clone(), eq_model.clone());
        let weak = ui.as_weak();
        ui.on_eq_preset(move |i| {
            let Some((_, gains)) = EQ_PRESETS.get(i.max(0) as usize) else {
                return;
            };
            let mut s = st.borrow_mut();
            s.eq_gains = *gains;
            // Picking a preset means wanting to hear it.
            s.eq_enabled = true;
            s.save();
            drop(s);
            for (band, g) in gains.iter().enumerate() {
                m.set_row_data(band, *g as f32);
                meter::set_eq_band(&p, band, *g);
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_s_eq_enabled(true);
            }
        });
        let st = settings.clone();
        ui.on_set_eq_enabled(move |on| {
            let mut s = st.borrow_mut();
            s.eq_enabled = on;
            s.save();
        });
    }
    let mut fade_shown = 1.0f64;
    let weak = ui.as_weak();
    let player = player.clone();
    let mut pulse = pulse::Pulse::default();
    let mut last_title = slint::SharedString::default();
    let mut clock_ms = 0f64;
    let mut last_tick = std::time::Instant::now();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(33),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let idle = ui.get_idle();
            // Music is playing: measure it for the bars and the pulse line
            // (which also runs over mpv's visualizers).
            let bars = !idle && !ui.get_has_video();
            let paused = ui.get_paused();
            // Decorative motion moves only while something plays (see
            // Clock in theme.slint), at this timer's 30 Hz.
            let now = std::time::Instant::now();
            if !idle && !paused {
                clock_ms =
                    (clock_ms + now.duration_since(last_tick).as_secs_f64() * 1000.0) % 3_600_000.0;
                ui.global::<Clock>().set_ms(clock_ms as f32);
            }
            last_tick = now;
            // The settings decide the filter chain (meter.rs rebuilds it for
            // the next song when the equalizer or bit-perfect mode changes).
            let (crossfade, has_video) = {
                let s = settings.borrow();
                let mut t = meter.tone.get();
                t.eq_enabled = s.eq_enabled;
                t.eq = s.eq_gains;
                t.bypass = s.bit_perfect;
                t.night = s.night_mode;
                t.dialogue = s.dialogue_boost;
                t.karaoke = ui.get_karaoke();
                t.skip_silence = ui.get_skip_silence()
                    && !s.nav_enabled
                    && !dvdmenu::menu_active()
                    && ui.get_duration() > 0.0
                    && silence::obstacle(&ui, s.bit_perfect).is_none();
                meter.tone.set(t);
                (
                    if s.bit_perfect { 0.0 } else { s.crossfade },
                    ui.get_has_video(),
                )
            };
            // Fades between songs (music only: a film's ending isn't faded).
            let gain = if crossfade > 0.0 && !has_video && !idle {
                let pos = f64::from(ui.get_position());
                let duration = f64::from(ui.get_duration());
                let count = player.get_double("playlist-count").unwrap_or(0.0);
                let index = player.get_double("playlist-pos").unwrap_or(0.0);
                let next = index + 1.0 < count || ui.get_repeat_mode() != 0;
                meter::fade_gain(crossfade, pos, duration, next)
            } else {
                1.0
            };
            let mut t = meter.tone.get();
            t.fade = gain;
            meter.tone.set(t);
            if (gain - fade_shown).abs() > 0.01 || (gain == 1.0 && fade_shown != 1.0) {
                meter::set_fade(&player, gain);
                fade_shown = gain;
            }
            if let Some(levels) = meter.tick(&player, bars, has_video, paused) {
                for (i, v) in levels.into_iter().enumerate().take(model.row_count()) {
                    model.set_row_data(i, v);
                }
            }
            // Beat, loudness and the pulse line (pulse.rs) for the UI's visuals.
            let title = ui.get_media_title();
            if title != last_title {
                pulse.clear();
                last_title = title;
            }
            pulse.tick(meter.heard().filter(|_| bars && !paused));
            if milkdrop::active() {
                milkdrop::set_levels(meter.heard().filter(|_| !paused));
                milkdrop::set_preset_seconds(settings.borrow().milkdrop_seconds);
            }
            ui.set_beat(pulse.beat());
            ui.set_energy(pulse.energy());
            if bars && ui.get_s_pulse_line() {
                let aspect = ui.get_pulse_aspect();
                let view_h = pulse::HISTORY as f32 * if aspect > 0.0 { aspect } else { 0.15 };
                ui.set_pulse_path(pulse.path(view_h).into());
                ui.set_pulse_head(pulse.head());
            }
        },
    );
    std::mem::forget(timer);
}

/// Run the system check (doctor.rs) on a worker thread and show the
/// findings. `quiet`: the startup check, which only speaks up (in a toast)
/// about real problems.
fn run_system_check(ui: &AppWindow, settings: &Settings, quiet: bool) {
    if !quiet {
        ui.set_check_running(true);
    }
    let settings = settings.clone();
    let engine = ui.get_engine_info().to_string();
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let facts = doctor::gather(&settings, engine);
        let items = doctor::findings(&facts);
        for i in &items {
            if i.level >= doctor::Level::Warning {
                log::info!("system check: {}: {}", i.title, i.detail);
            }
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let problems = items
                .iter()
                .filter(|i| i.level == doctor::Level::Problem)
                .count();
            ui.set_check_summary(doctor::summary(&items).into());
            ui.set_check_report(doctor::report(&items).into());
            let model: Vec<CheckItem> = items
                .into_iter()
                .map(|i| {
                    let (id, label) = i.action.id_and_label();
                    CheckItem {
                        level: i.level as i32,
                        title: i.title.into(),
                        detail: i.detail.into(),
                        action: id.into(),
                        action_label: label.into(),
                        command: i.command.into(),
                    }
                })
                .collect();
            ui.set_check_items(std::rc::Rc::new(slint::VecModel::from(model)).into());
            ui.set_check_running(false);
            if quiet && problems > 0 && !ui.get_check_open() {
                ui.set_toast(
                    format!(
                        "The system check found {problems} problem{}: see Tools → System check",
                        if problems == 1 { "" } else { "s" }
                    )
                    .into(),
                );
            }
        });
    });
}

/// Status lines for the Settings panel.
fn refresh_tool_status(ui: &AppWindow) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let ytdlp = match tools::find_ytdlp() {
            Some(p) => match tools::ytdlp_version(&p) {
                Some(v) => format!("Version {v} · {}", p.display()),
                None => format!("Found at {}, but it does not run", p.display()),
            },
            None => "Not installed: YouTube and similar links will not play".into(),
        };
        let keydb =
            match settings::aacs_keydb().and_then(|p| std::fs::metadata(&p).ok().map(|m| (p, m))) {
                Some((p, m)) => {
                    let age = m
                        .modified()
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .map(|d| d.as_secs() / 86_400)
                        .unwrap_or(0);
                    format!("{} ({} MB, {age} days old)", p.display(), m.len() >> 20)
                }
                None => "Not installed: most commercial Blu-rays will not play".into(),
            };
        let lib = |names: &[&str]| {
            if disc::library_present(names) {
                "installed"
            } else {
                "missing"
            }
        };
        let disc_status = format!(
            "DVD decryption (libdvdcss): {} · Blu-ray AACS (libaacs): {} · BD+ (libbdplus): {}",
            lib(&["libdvdcss.so.2", "libdvdcss-2.dll", "libdvdcss.2.dylib"]),
            lib(&["libaacs.so.0", "libaacs.dll", "libaacs.0.dylib"]),
            lib(&["libbdplus.so.0", "libbdplus.dll", "libbdplus.0.dylib"]),
        );
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_ytdlp_status(ytdlp.into());
            ui.set_keydb_status(keydb.into());
            ui.set_disc_status(disc_status.into());
        });
    });
}

/// Run `work` on a worker thread while the UI shows `busy`, then `done` on
/// the UI thread with its result.
fn run_busy<T: Send + 'static>(
    ui: &AppWindow,
    busy: &str,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&AppWindow, T) + Send + 'static,
) {
    if !ui.get_busy_text().is_empty() {
        ui.set_toast("Please wait for the current task to finish".into());
        return;
    }
    ui.set_busy_text(busy.into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = work();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_busy_text("".into());
            done(&ui, result);
        });
    });
}

/// Point mpv at the yt-dlp to use now (after an install or update). Its
/// ytdl_hook rereads its options, so this works without a restart.
fn use_current_ytdlp(ui: &AppWindow) {
    if let Some(path) = tools::find_ytdlp() {
        with_player(|p| {
            let opt = format!("ytdl_hook-ytdl_path={}", path.display());
            report(
                ui,
                p.command(&["change-list", "script-opts", "append", &opt]),
            );
        });
    }
}

/// Keep yt-dlp and the key database current in the background: a check
/// shortly after startup, then every few hours while the player stays open.
/// `tools::auto_update` itself limits real checks to once a week / month.
fn schedule_auto_update(ui: &AppWindow, settings: Rc<RefCell<Settings>>) {
    let weak = ui.as_weak();
    let check = move || {
        let Some(ui) = weak.upgrade() else { return };
        // Not while the user is installing something by hand.
        if !settings.borrow().auto_update || !ui.get_busy_text().is_empty() {
            return;
        }
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let result = tools::auto_update();
            if result.ytdlp.is_none() && !result.keydb {
                return;
            }
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let mut news = Vec::new();
                if let Some(v) = &result.ytdlp {
                    use_current_ytdlp(&ui);
                    news.push(format!("yt-dlp updated to {v}"));
                }
                if result.keydb {
                    news.push("Blu-ray key database updated".to_owned());
                }
                ui.set_toast(news.join(" · ").into());
                refresh_tool_status(&ui);
            });
        });
    };
    let check = Rc::new(check);
    let first = check.clone();
    slint::Timer::single_shot(Duration::from_secs(20), move || first());
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_secs(6 * 3600),
        move || check(),
    );
    std::mem::forget(timer);
}

/// Install or update yt-dlp, then (when `retry` is given) play that link again.
fn install_ytdlp(ui: &AppWindow, retry: Option<String>) {
    run_busy(
        ui,
        "Getting yt-dlp…",
        tools::update_ytdlp,
        move |ui, result| {
            match result {
                Ok(msg) => {
                    ui.set_toast(msg.into());
                    use_current_ytdlp(ui);
                    if let Some(url) = retry.as_deref().filter(|u| !u.is_empty()) {
                        with_player(|p| report(ui, p.load(url, LoadMode::Replace)));
                    }
                }
                Err(e) => ui.set_toast(e.into()),
            }
            refresh_tool_status(ui);
        },
    );
}

fn install_keydb(ui: &AppWindow) {
    let yes = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Info)
        .set_title("Blu-ray key database")
        .set_description(format!(
            "Commercial Blu-rays are encrypted. libaacs decrypts them with a community key \
             database (FindVUK, about 25 MB), saved to ~/.config/aacs/KEYDB.cfg.\n\n\
             The file comes from {} over plain HTTP; the player checks that it looks like a key \
             database before saving it. Whether using it is allowed depends on the laws where \
             you live.\n\nDownload it now?",
            tools::KEYDB_URL
        ))
        .set_buttons(rfd::MessageButtons::YesNo)
        .show();
    if yes != rfd::MessageDialogResult::Yes {
        return;
    }
    run_busy(
        ui,
        "Downloading the Blu-ray key database…",
        tools::install_keydb,
        |ui, result| {
            match result {
                Ok(p) => ui.set_toast(format!("Key database saved to {}", p.display()).into()),
                Err(e) => ui.set_toast(e.into()),
            }
            refresh_tool_status(ui);
        },
    );
}

/// Paths and links dropped on the window. X11 delivers one event per file,
/// so collect them for a moment and open them together.
fn install_drop_handler(ui: &AppWindow) {
    attach_drop_handler(ui.window(), ui.as_weak(), false);
}

/// Files dropped on `window` play, or join the playlist when it is showing
/// (`always_add`: the detached playlist window). winit doesn't say where in
/// the window they were dropped, so the playlist being open decides.
fn attach_drop_handler(window: &slint::Window, weak: slint::Weak<AppWindow>, always_add: bool) {
    use slint::winit_030::winit::event::{ElementState, MouseButton, WindowEvent};
    use slint::winit_030::{EventResult, WinitWindowAccessor};
    let pending: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let mouse = Cell::new(slint::LogicalPosition::default());
    window.on_winit_window_event(move |_, event| {
        let Some(ui) = weak.upgrade() else {
            return EventResult::Propagate;
        };
        if !always_add {
            frame::window_event(&ui, event);
        }
        match event {
            WindowEvent::CursorMoved { position, .. } if !always_add => {
                let scale = f64::from(ui.window().scale_factor());
                mouse.set(slint::LogicalPosition::new(
                    (position.x / scale) as f32,
                    (position.y / scale) as f32,
                ));
            }
            // The skins' menu. Slint would open it at the mouse, where its
            // sub-menus may have no room (see show-skin-menu).
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if !always_add && ui.get_skin_alone() => {
                ui.invoke_show_skin_menu(mouse.get().x, mouse.get().y);
                return EventResult::PreventDefault;
            }
            WindowEvent::HoveredFile(_) if !always_add => ui.set_drop_hover(true),
            WindowEvent::HoveredFileCancelled => ui.set_drop_hover(false),
            WindowEvent::DroppedFile(path) => {
                ui.set_drop_hover(false);
                let first = pending.borrow().is_empty();
                pending.borrow_mut().push(path.clone());
                if first {
                    let pending = pending.clone();
                    let weak = weak.clone();
                    slint::Timer::single_shot(Duration::from_millis(120), move || {
                        let paths = std::mem::take(&mut *pending.borrow_mut());
                        if let Some(ui) = weak.upgrade() {
                            let add = always_add || ui.get_playlist_open();
                            open_inputs(&ui, paths.into_iter().map(Input::Path).collect(), add);
                        }
                    });
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });
}

fn open_files_dialog(ui: &AppWindow) {
    let Some(files) = rfd::FileDialog::new()
        .set_title("Open media")
        .add_filter("Media files", MEDIA_EXTENSIONS)
        .add_filter("All files", &["*"])
        .pick_files()
    else {
        return;
    };
    // Replace when idle, otherwise queue after the current playlist.
    open_inputs(
        ui,
        files.into_iter().map(Input::Path).collect(),
        !ui.get_idle(),
    );
}

/// Open files, folders, disc folders and links. Folders are scanned for
/// media; a disc (folder, image, or a VOB inside VIDEO_TS) plays as a disc.
/// With `append`, items are queued after the current playlist.
fn open_inputs(ui: &AppWindow, inputs: Vec<Input>, append: bool) {
    let mut items: Vec<String> = Vec::new();
    let mut disc_plan: Option<DiscPlan> = None;
    let mut missing = Vec::new();
    for input in inputs {
        match input {
            Input::Url(u) => items.push(u),
            Input::Path(p) if !p.exists() => missing.push(p.display().to_string()),
            Input::Path(p) if playlists::is_playlist(&p) => match playlists::read(&p) {
                Ok(list) => items.extend(list),
                Err(e) => missing.push(format!("{} ({e})", p.display())),
            },
            Input::Path(p) => {
                if let Some(plan) = disc::plan_path(&p) {
                    disc_plan.get_or_insert(plan);
                } else if p.is_dir() {
                    let found = collect_media(&p);
                    if found.is_empty() {
                        // e.g. a folder of movie folders, each holding a VIDEO_TS
                        if let Some(plan) = find_disc_folder(&p) {
                            disc_plan.get_or_insert(plan);
                        }
                    }
                    items.extend(found.into_iter().map(|f| f.to_string_lossy().into_owned()));
                } else {
                    items.push(p.to_string_lossy().into_owned());
                }
            }
        }
    }
    if !missing.is_empty() && items.is_empty() && disc_plan.is_none() {
        ui.set_toast(format!("Not found: {}", missing.join(", ")).into());
        return;
    }
    with_player(|player| {
        if items.is_empty() {
            match disc_plan {
                Some(plan) => play_disc(ui, player, plan),
                None => ui.set_toast("No playable media found there".into()),
            }
            return;
        }
        if disc_plan.is_some() {
            ui.set_toast("Disc folders were skipped: open them on their own to play them".into());
        }
        let was_idle = ui.get_idle();
        let mut mode = if append && !was_idle {
            LoadMode::AppendPlay
        } else {
            LoadMode::Replace
        };
        if mode == LoadMode::Replace {
            disc::reset_cache(player);
        }
        for item in &items {
            if let Err(e) = player.load(item, mode) {
                log::warn!("{e}");
            }
            mode = LoadMode::Append;
        }
        if items.len() > 1 || (append && !was_idle) {
            ui.set_toast(
                match items.len() {
                    1 => "Added to the playlist".to_string(),
                    n => format!("{n} items added to the playlist"),
                }
                .into(),
            );
        }
    });
}

/// Media files in `dir` and its subfolders, in natural order (track 2 before
/// track 10), folder by folder. Skips hidden entries, disc structures and
/// symlinked folders (which could loop).
fn collect_media(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            if ft.is_dir() {
                let upper = name.to_uppercase();
                if depth < 12
                    && !matches!(
                        upper.as_str(),
                        "VIDEO_TS" | "AUDIO_TS" | "BDMV" | "CERTIFICATE"
                    )
                {
                    dirs.push(path);
                }
            } else if is_folder_media(&path) {
                files.push(path);
            }
        }
        files.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
        out.extend(files);
        if out.len() >= MAX_FOLDER_FILES {
            out.truncate(MAX_FOLDER_FILES);
            break;
        }
        // Reverse so the stack pops folders in natural order.
        dirs.sort_by(|a, b| natural_cmp(&b.to_string_lossy(), &a.to_string_lossy()));
        stack.extend(dirs.into_iter().map(|p| (p, depth + 1)));
    }
    out
}

fn is_folder_media(p: &Path) -> bool {
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    MEDIA_EXTENSIONS.contains(&ext.as_str()) && !SKIP_IN_FOLDERS.contains(&ext.as_str())
}

/// The first DVD/Blu-ray folder directly inside `dir`.
fn find_disc_folder(dir: &Path) -> Option<DiscPlan> {
    let mut subdirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    subdirs.iter().find_map(|d| disc::plan_path(d))
}

/// Compare strings so that embedded numbers sort by value ("2" < "10").
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (na, nb) = (take(&mut a), take(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

thread_local! {
    /// "DVD, main film": the next DVD starts without its menu.
    static DVD_FILM_ONLY: Cell<bool> = const { Cell::new(false) };
    /// The Blu-ray drive played last, for another try (see [`play_bluray`]).
    static LAST_BLURAY: RefCell<Option<DiscPlan>> = const { RefCell::new(None) };
}

fn play_disc(ui: &AppWindow, player: &Player, plan: DiscPlan) {
    log::info!("playing {} from {}", plan.kind.name(), plan.device);
    if plan.kind == DiscKind::Bluray && plan.device.starts_with("/dev/") {
        LAST_BLURAY.set(Some(plan.clone()));
        play_bluray(ui, "Opening the Blu-ray…");
    } else {
        if plan.kind == DiscKind::Bluray {
            LAST_BLURAY.set(None);
        }
        start_disc(ui, player, &plan);
    }
}

/// Play the Blu-ray in [`LAST_BLURAY`], once the drive's reading mode is
/// known (bluray.rs; it takes a few seconds).
fn play_bluray(ui: &AppWindow, busy: &str) {
    let Some(mut plan) = LAST_BLURAY.with_borrow(|p| p.clone()) else {
        return;
    };
    let device = plan.device.clone();
    run_busy(
        ui,
        busy,
        move || bluray::source(&device),
        move |ui, source| {
            plan.device = source;
            with_player(|p| start_disc(ui, p, &plan));
        },
    );
}

/// A disc that didn't open gets one more try: the Blu-ray drive may have
/// changed how it reads since (an eject, MakeMKV), or the disc was still
/// spinning up.
pub(crate) fn retry_disc(ui: &AppWindow, url: &str) {
    if url.starts_with("bd://") && LAST_BLURAY.with_borrow(|p| p.is_some()) {
        play_bluray(ui, "Couldn't read the disc yet; trying again…");
        return;
    }
    with_player(|p| {
        if let Err(e) = p.load(url, LoadMode::Replace) {
            log::warn!("disc retry: {e}");
        }
    });
}

/// Hand a disc to mpv (or to its menu).
fn start_disc(ui: &AppWindow, player: &Player, plan: &DiscPlan) {
    UI_STATE.with_borrow_mut(|s| {
        if let Some(s) = s.as_mut() {
            // Explains a failure to open, if it comes to that.
            s.set_next_file_hint(plan.warning.clone());
        }
    });
    if plan.kind == DiscKind::Dvd
        && plan.url == "dvd://"
        && !plan.device.is_empty()
        && ui.get_s_dvd_menus()
        && !DVD_FILM_ONLY.take()
        && dvdmenu::available()
    {
        dvdmenu::start(ui, &plan.device, None);
    } else {
        report(ui, plan.apply(player));
    }
    if let Some(note) = &plan.note {
        ui.set_toast(note.clone().into());
    } else {
        if let Some(warning) = &plan.warning {
            log::warn!("{warning}");
        }
        // Which drive, for computers with more than one.
        if !plan.drive.is_empty() {
            ui.set_toast(
                format!(
                    "Playing {} from the {}",
                    plan.kind.with_article(),
                    plan.drive
                )
                .into(),
            );
        }
    }
}

fn open_disc(ui: &AppWindow, kind: &str) {
    // "rip-cd": find the audio CD as for playing it, then offer to rip it.
    let rip_cd = kind == "rip-cd";
    if matches!(kind, "dvd" | "dvd-film" | "auto") {
        DVD_FILM_ONLY.set(kind == "dvd-film");
    }
    let preferred = match kind {
        "dvd" | "dvd-film" => Some(DiscKind::Dvd),
        "bluray" => Some(DiscKind::Bluray),
        "cd" | "rip-cd" => Some(DiscKind::AudioCd),
        "auto" => None,
        "image" => {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Open disc image")
                .add_filter("Disc images", &["iso", "img"])
                .pick_file()
            {
                open_inputs(ui, vec![Input::Path(path)], false);
            }
            return;
        }
        "folder" => {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title("Open DVD or Blu-ray folder (the one holding VIDEO_TS or BDMV)")
                .pick_folder()
            {
                match disc::plan_path(&dir).or_else(|| find_disc_folder(&dir)) {
                    Some(plan) => with_player(|p| play_disc(ui, p, plan)),
                    None => ui.set_toast("That folder has no VIDEO_TS or BDMV folder in it".into()),
                }
            }
            return;
        }
        _ => return,
    };
    if !ui.get_busy_text().is_empty() {
        ui.set_toast("Please wait for the current task to finish".into());
        return;
    }
    // Finding the disc can take half a minute (a drive spinning up, or one
    // that never gets ready): show the disc card, which names each drive as
    // it is checked, and lets Cancel forget the search.
    let scan = RIP_SCAN.get() + 1;
    RIP_SCAN.set(scan);
    ui.set_busy_text("Reading the disc…".into());
    show_disc_card(
        ui,
        "READING THE DISC",
        "Looking for the disc…",
        "",
        "Checking your drives…",
    );
    let weak = ui.as_weak();
    let cd_online = ui.get_s_cd_online();
    std::thread::spawn(move || {
        let status_ui = weak.clone();
        let report_ui = weak.clone();
        let plan = disc::plan_drive(preferred, move |msg| {
            log::info!("{msg}");
            let _ = status_ui.upgrade_in_event_loop(move |ui| {
                if RIP_SCAN.get() == scan {
                    ui.set_rip_stage(msg.into());
                }
            });
        });
        // Song names for audio CDs (cached after the first time).
        let plan = plan.map(|mut plans| {
            for p in plans.iter_mut().filter(|p| p.cd.is_some()) {
                let _ = report_ui.upgrade_in_event_loop(move |ui| {
                    if RIP_SCAN.get() == scan {
                        ui.set_rip_stage("Looking up the CD's song names…".into());
                    }
                });
                disc::name_cd(p, cd_online);
            }
            plans
        });
        // Only MakeMKV can read a disc the drive refuses: offer it, on that
        // drive, if MakeMKV is installed.
        let refused = plan
            .as_ref()
            .err()
            .is_some_and(|e| disc::is_refused_error(e));
        let refusing = (refused && ripper::tool().is_some())
            .then(|| {
                disc::drive_reports().into_iter().find(|d| {
                    matches!(
                        d.state,
                        disc::DriveState::Refuses | disc::DriveState::NotReady
                    )
                })
            })
            .flatten();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if RIP_SCAN.get() != scan {
                log::info!("disc search finished after it was cancelled; ignored");
                return;
            }
            ui.set_busy_text("".into());
            ui.set_rip_active(false);
            if rip_cd {
                match plan.map(|ps| ps.into_iter().find(|p| p.cd.is_some())) {
                    Ok(Some(p)) => confirm_cd_rip(&ui, p),
                    Ok(None) => ui.set_toast("No audio CD in the drives".into()),
                    Err(msg) => ui.set_toast(msg.into()),
                }
                return;
            }
            match plan {
                Ok(mut plans) if plans.len() == 1 => {
                    let plan = plans.remove(0);
                    with_player(|p| play_disc(&ui, p, plan));
                }
                Ok(plans) => pick_disc(&ui, plans),
                Err(msg) => match refusing {
                    Some(drive) => {
                        let toast = msg.clone();
                        ask(
                            &ui,
                            "This disc can't be read directly",
                            &format!(
                                "{msg}\n\nMakeMKV may still read it: copy the main movie from the {} \
                                 temporarily with MakeMKV, and play it when done? (The copy is deleted after playing.)",
                                drive.label
                            ),
                            "Try MakeMKV",
                            "Not now",
                            move |ui, yes| {
                                if yes {
                                    rip_from(ui, drive.device, drive.label);
                                } else {
                                    ui.set_toast(toast.into());
                                }
                            },
                        );
                    }
                    None if refused => {
                        ask(&ui, "This disc can't be read", &msg, "OK", "", |_, _| {});
                    }
                    None => ui.set_toast(msg.into()),
                },
            }
        });
    });
}

/// Ask before ripping the audio CD in `plan` to FLAC, saying what and where.
fn confirm_cd_rip(ui: &AppWindow, plan: DiscPlan) {
    let Some(cd) = plan.cd.clone() else { return };
    let id = cd::disc_id(&cd.toc);
    let dir = cd::rip_folder(&cd::music_dir(), cd.album.as_ref(), id.as_deref());
    let secs: u64 = cd.tracks.iter().map(|t| u64::from(t.frames) / 75).sum();
    let songs = format!(
        "{} songs, {}",
        cd.tracks.len(),
        ripper::format_duration(secs)
    );
    let what = match &cd.album {
        Some(a) if !a.title.is_empty() && !a.artist.is_empty() => {
            format!("\"{}\" by {} ({songs})", a.title, a.artist)
        }
        Some(a) if !a.title.is_empty() => format!("\"{}\" ({songs})", a.title),
        _ => format!("this CD ({songs}; no song names were found, so they are numbered)"),
    };
    let drive = if plan.drive.is_empty() {
        "drive".to_owned()
    } else {
        plan.drive.clone()
    };
    let mut text = format!(
        "Rip {what} from the {drive} to FLAC files (lossless) in\n{}\n\nEach song is read with full \
         error correction: a CD takes about 5 to 15 minutes, a scratched one longer. You can cancel \
         at any time; the songs already done are kept.",
        dir.display()
    );
    if dir.is_dir() {
        text += "\n\nThat folder already exists: songs with the same names are replaced.";
    }
    ask(
        ui,
        "Rip audio CD",
        &text,
        "Rip",
        "Cancel",
        move |ui, yes| {
            if yes {
                start_cd_rip(
                    ui,
                    plan.device.clone(),
                    drive.clone(),
                    cd.clone(),
                    dir.clone(),
                );
            }
        },
    );
}

fn start_cd_rip(ui: &AppWindow, device: String, drive: String, cd: disc::CdDisc, dir: PathBuf) {
    if !ui.get_busy_text().is_empty() {
        ui.set_toast("Please wait for the current task to finish".into());
        return;
    }
    // Playing the CD while ripping it would make the drive seek back and forth.
    with_player(|p| {
        if p.get_string("path")
            .is_some_and(|path| path.contains("cdda://"))
        {
            let _ = p.stop();
        }
    });
    let cancel = ripper::Cancel::default();
    RIP_CANCEL.with_borrow_mut(|c| *c = Some(cancel.clone()));
    let title = cd
        .album
        .as_ref()
        .filter(|a| !a.title.is_empty())
        .map(|a| a.title.clone())
        .unwrap_or_else(|| "Audio CD".to_owned());
    ui.set_busy_text("Ripping the CD…".into());
    show_disc_card(ui, "RIPPING THE CD", &title, &drive, "Starting…");
    ui.set_rip_percent(0.0);
    log::info!("ripping the CD in {device} to {}", dir.display());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let frames: Vec<u64> = cd.tracks.iter().map(|t| u64::from(t.frames)).collect();
        let total = frames.iter().sum::<u64>().max(1);
        let names: Vec<String> = cd
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                cd.album
                    .as_ref()
                    .map(|a| a.track_title(i, t.number))
                    .unwrap_or_else(|| format!("Track {}", t.number))
            })
            .collect();
        let started = std::time::Instant::now();
        let progress_ui = weak.clone();
        let first_lba = cd.toc.iter().find(|e| !e.2).map_or(0, |e| e.1);
        let result = cd::rip(
            &device,
            &cd.tracks,
            first_lba,
            cd.album.as_ref(),
            cd.cover.as_deref(),
            &dir,
            cancel.flag(),
            |i, f| {
                let done = frames[..i].iter().sum::<u64>() as f64 + f64::from(f) * frames[i] as f64;
                let part = (done / total as f64).clamp(0.0, 1.0);
                let elapsed = started.elapsed().as_secs_f64();
                let eta = if part > 0.02 {
                    time_left((elapsed * (1.0 - part) / part) as u64)
                } else {
                    "working out the time left…".to_owned()
                };
                let stage = format!("Song {} of {}: {}", i + 1, frames.len(), names[i]);
                let _ = progress_ui.upgrade_in_event_loop(move |ui| {
                    ui.set_rip_percent(part as f32);
                    ui.set_rip_stage(stage.into());
                    ui.set_rip_eta(eta.into());
                });
            },
        );
        let _ = weak.upgrade_in_event_loop(move |ui| {
            RIP_CANCEL.with_borrow_mut(|c| *c = None);
            ui.set_busy_text("".into());
            ui.set_rip_active(false);
            match result {
                Ok(report) => {
                    log::info!("CD ripped: {} songs, {} sectors corrected, {} skipped", report.files.len(), report.corrected, report.skipped);
                    let mut text = format!("{} songs saved as FLAC in\n{}", report.files.len(), dir.display());
                    if report.skipped > 0 {
                        text += &format!(
                            "\n\nThe drive couldn't read every part of these songs, even after many tries, so they may \
                             have a short click or gap: {}. Cleaning the disc and ripping them again can help.",
                            report.damaged.join(", ")
                        );
                    } else if report.corrected > 0 {
                        text += "\n\nSome sectors needed reading again (a worn disc), but every song came out exact.";
                    } else {
                        text += "\n\nEvery sector read cleanly: the files hold exactly the CD's sound.";
                    }
                    ask(&ui, "CD ripped", &text, "Open folder", "Close", move |ui, yes| {
                        if yes && let Err(e) = open_folder(&dir) {
                            ui.set_toast(format!("Couldn't open the folder: {e}").into());
                        }
                    });
                }
                Err(e) if e == "Cancelled" => {
                    ui.set_toast(format!("Rip cancelled; the songs already done are in {}", dir.display()).into());
                }
                Err(e) => ui.set_toast(format!("The rip stopped: {e}").into()),
            }
        });
    });
}

thread_local! {
    /// The discs the drive picker offers to play (see [`pick_disc`]).
    static DISC_CHOICES: RefCell<Vec<DiscPlan>> = const { RefCell::new(Vec::new()) };
}

/// Discs in more than one drive: ask which to play, instead of guessing.
fn pick_disc(ui: &AppWindow, plans: Vec<DiscPlan>) {
    let choices: Vec<DriveChoice> = plans
        .iter()
        .map(|p| {
            let what = if p.warning.as_deref() == Some(disc::UHD_HELP) {
                "an Ultra HD Blu-ray".to_owned()
            } else {
                p.kind.with_article()
            };
            DriveChoice {
                device: p.device.clone().into(),
                name: if p.drive.is_empty() {
                    p.device.clone()
                } else {
                    p.drive.clone()
                }
                .into(),
                detail: format!("Holds {what}").into(),
            }
        })
        .collect();
    DISC_CHOICES.with_borrow_mut(|c| *c = plans);
    ui.set_drive_choices(Rc::new(slint::VecModel::from(choices)).into());
    ui.set_drive_picker_play(true);
    ui.invoke_show_drive_picker();
}

/// Show the disc card (rip.slint) in "working, length unknown" mode.
fn show_disc_card(ui: &AppWindow, heading: &str, title: &str, drive: &str, stage: &str) {
    ui.set_rip_heading(heading.into());
    ui.set_rip_title(title.into());
    ui.set_rip_drive(drive.into());
    ui.set_rip_percent(-1.0);
    ui.set_rip_stage(stage.into());
    ui.set_rip_eta("".into());
    ui.set_rip_active(true);
}

/// What a MakeMKV disc scan is for: Rip & play (a temporary copy of the
/// main title) or Rip disc to video files (named files to keep).
#[derive(Clone, Copy, PartialEq)]
enum RipMode {
    Play,
    Files,
}

thread_local! {
    static RIP_MODE: Cell<RipMode> = const { Cell::new(RipMode::Play) };
    /// The Rip-to-files panel's plan and what it needs to start.
    static RIP_PLAN: RefCell<Option<RipFiles>> = const { RefCell::new(None) };
}

struct RipFiles {
    tool: ripper::Tool,
    device: String,
    drive: String,
    plan: discrip::Plan,
    /// Where the show's or film's folder goes (Videos unless changed).
    base: PathBuf,
}

thread_local! {
    /// Stops the running rip (Cancel on the rip card).
    static RIP_CANCEL: RefCell<Option<ripper::Cancel>> = const { RefCell::new(None) };
    /// A rip is waiting for the MakeMKV key: (device, drive name). It starts
    /// once the key is in.
    static RIP_AFTER_KEY: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

/// Why a rip couldn't start.
enum RipStart {
    NeedsKey,
    Message(String),
}

/// What a drive holds, in a few words, for the drive picker.
fn drive_contents(state: &disc::DriveState) -> &'static str {
    match state {
        disc::DriveState::Refuses => "a disc it can't read by itself (MakeMKV may)",
        disc::DriveState::NotReady => "a disc it isn't ready to read yet",
        disc::DriveState::Disc { uhd: true, .. } => "an Ultra HD Blu-ray",
        disc::DriveState::Disc {
            kind: Some(DiscKind::Bluray),
            ..
        } => "a Blu-ray",
        disc::DriveState::Disc {
            kind: Some(DiscKind::Dvd),
            ..
        } => "a DVD",
        disc::DriveState::Disc {
            kind: Some(DiscKind::AudioCd),
            ..
        } => "an audio CD",
        disc::DriveState::Disc { .. } => "a disc",
        _ => "no disc",
    }
}

/// File → Play disc → Rip & play with MakeMKV. With one drive holding a
/// disc, rip from it; with several, ask which (the picker calls
/// [`rip_from`]).
fn rip_disc(ui: &AppWindow) {
    RIP_MODE.set(RipMode::Play);
    find_rip_drive(ui);
}

/// File → Play disc → Rip disc to video files: as [`rip_disc`], then the
/// Rip-to-files panel instead of Rip & play.
fn rip_files(ui: &AppWindow) {
    RIP_MODE.set(RipMode::Files);
    find_rip_drive(ui);
}

fn find_rip_drive(ui: &AppWindow) {
    // Audio CDs have their own ripper (to FLAC), not MakeMKV. (Read here:
    // the search runs on another thread.)
    let files = RIP_MODE.get() == RipMode::Files;
    run_busy(
        ui,
        "Looking for discs…",
        move || {
            disc::drive_reports()
                .into_iter()
                .filter(|d| {
                    matches!(
                        d.state,
                        disc::DriveState::Refuses
                            | disc::DriveState::NotReady
                            | disc::DriveState::Disc { .. }
                    ) && !(files
                        && matches!(
                            d.state,
                            disc::DriveState::Disc {
                                kind: Some(DiscKind::AudioCd),
                                ..
                            }
                        ))
                })
                .collect::<Vec<_>>()
        },
        |ui, drives| match drives.as_slice() {
            [] => ui.set_toast("There is no disc in any drive".into()),
            [d] => rip_from(ui, d.device.clone(), d.label.clone()),
            _ => {
                let choices: Vec<DriveChoice> = drives
                    .iter()
                    .map(|d| DriveChoice {
                        device: d.device.clone().into(),
                        name: d.label.clone().into(),
                        detail: format!("Holds {}", drive_contents(&d.state)).into(),
                    })
                    .collect();
                ui.set_drive_choices(Rc::new(slint::VecModel::from(choices)).into());
                ui.set_drive_picker_play(false);
                ui.invoke_show_drive_picker();
            }
        },
    );
}

/// What to do with the answer to the in-app question.
type Answer = Box<dyn FnOnce(&AppWindow, bool)>;

thread_local! {
    /// The pending question of the in-app ask card.
    static ASK: RefCell<Option<Answer>> = RefCell::new(None);
    /// Bumped to forget a MakeMKV disc scan that was cancelled.
    static RIP_SCAN: Cell<u64> = const { Cell::new(0) };
}

/// Ask a question inside the player (a separate dialog window can end up
/// hidden behind it, so it looks as if nothing happened). `then` gets the
/// answer. An empty `cancel` shows only the OK button.
fn ask(
    ui: &AppWindow,
    title: &str,
    text: &str,
    ok: &str,
    cancel: &str,
    then: impl FnOnce(&AppWindow, bool) + 'static,
) {
    ASK.with_borrow_mut(|a| *a = Some(Box::new(then)));
    ui.set_ask_title(title.into());
    ui.set_ask_text(text.into());
    ui.set_ask_ok_label(ok.into());
    ui.set_ask_cancel_label(cancel.into());
    ui.set_ask_open(true);
}

/// Read the titles of the disc in `device` with MakeMKV, then confirm and
/// rip its main movie. `drive`: the drive's name for messages. MakeMKV takes
/// about a minute to scan a disc, so the rip card shows it working (and
/// Cancel forgets the scan).
fn rip_from(ui: &AppWindow, device: String, drive: String) {
    if !ui.get_busy_text().is_empty() {
        ui.set_toast("Please wait for the current task to finish".into());
        return;
    }
    let scan = RIP_SCAN.get() + 1;
    RIP_SCAN.set(scan);
    ui.set_busy_text("Reading the disc with MakeMKV…".into());
    show_disc_card(
        ui,
        "MAKEMKV IS READING THE DISC",
        "Reading the disc…",
        &drive,
        "MakeMKV is looking for the movie on the disc. This takes about a minute.",
    );
    log::info!("asking MakeMKV for the titles on the disc in the {drive} ({device})");
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = (|| -> Result<_, RipStart> {
            let tool = ripper::tool().ok_or_else(|| {
                RipStart::Message(format!(
                    "MakeMKV isn't installed (or doesn't start): see Tools → MakeMKV (key & help), or run: {}",
                    ripper::FLATPAK_INSTALL
                ))
            })?;
            let info = ripper::disc_info(&tool, &device).map_err(|e| match e {
                ripper::RipError::NeedsKey => RipStart::NeedsKey,
                e => RipStart::Message(e.to_string()),
            })?;
            Ok((tool, info))
        })();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if RIP_SCAN.get() != scan {
                log::info!("MakeMKV disc scan finished after it was cancelled; ignored");
                return;
            }
            ui.set_busy_text("".into());
            ui.set_rip_active(false);
            match result {
                Ok((tool, info)) => {
                    log::info!(
                        "MakeMKV found {} title(s) on \"{}\" in the {drive}",
                        info.titles.len(),
                        info.name
                    );
                    match RIP_MODE.get() {
                        RipMode::Play => confirm_rip(&ui, tool, device, drive, info),
                        RipMode::Files => open_rip_files(&ui, tool, device, drive, info),
                    }
                }
                Err(RipStart::NeedsKey) => {
                    log::info!("MakeMKV needs its key");
                    RIP_AFTER_KEY.with_borrow_mut(|r| *r = Some((device, drive)));
                    ui.set_toast("MakeMKV needs its free beta key before it reads discs".into());
                    ui.invoke_show_makemkv_key();
                }
                Err(RipStart::Message(m)) => {
                    log::warn!("MakeMKV couldn't read the disc: {m}");
                    // MakeMKV's messages name the device ("/dev/sr1"): say which drive.
                    let m = m.replace(device.as_str(), &format!("the {drive}"));
                    ask(
                        &ui,
                        "MakeMKV couldn't read the disc",
                        &m,
                        "OK",
                        "",
                        |_, _| {},
                    );
                }
            }
        });
    });
}

// ---- Rip disc to video files (discrip.rs) ----

/// Open the Rip-to-files panel for the disc MakeMKV just read. A series'
/// episode names are looked up (when online lookups are on) while it shows.
fn open_rip_files(
    ui: &AppWindow,
    tool: ripper::Tool,
    device: String,
    drive: String,
    info: ripper::DiscInfo,
) {
    if info.titles.is_empty() {
        ui.set_toast("MakeMKV found no titles on the disc".into());
        return;
    }
    let label = discrip::disc_label(&info);
    let plan = discrip::Plan::new(&info, &label, None);
    let series = plan.series;
    let name = plan.name.clone();
    RIP_PLAN.with_borrow_mut(|p| {
        *p = Some(RipFiles {
            tool,
            device,
            drive,
            plan,
            base: discrip::videos_dir(),
        })
    });
    ui.set_ripfiles_name(name.into());
    refresh_rip_files(ui);
    ui.set_ripfiles_open(true);
    if series && ui.get_s_cd_online() {
        lookup_rip_names(ui);
    }
}

/// Show the plan in the panel (all but the name field, which the user may
/// be typing in).
fn refresh_rip_files(ui: &AppWindow) {
    RIP_PLAN.with_borrow(|r| {
        let Some(r) = r else { return };
        let p = &r.plan;
        let rows: Vec<RipRow> = p
            .items
            .iter()
            .map(|i| RipRow {
                rip: i.rip,
                file: i.file.clone().into(),
                detail: format!(
                    "Title {} · {} · {} · {} chapters",
                    i.title.id + 1,
                    ripper::format_duration(i.title.seconds),
                    ripper::format_size(i.title.bytes),
                    i.title.chapters
                )
                .into(),
            })
            .collect();
        ui.set_ripfiles_rows(Rc::new(slint::VecModel::from(rows)).into());
        ui.set_ripfiles_series(p.series);
        ui.set_ripfiles_season(p.season as i32);
        ui.set_ripfiles_first(p.first as i32);
        ui.set_ripfiles_summary(p.summary().into());
        let folder = p.folder(&r.base);
        ui.set_ripfiles_folder(folder.display().to_string().into());
        let chosen: Vec<&discrip::Item> = p.items.iter().filter(|i| i.rip).collect();
        let bytes: u64 = chosen.iter().map(|i| i.title.bytes).sum();
        let free = ripper::free_bytes(&folder);
        let room = free.is_none_or(|f| f > bytes + bytes / 50);
        let mut total = match chosen.len() {
            0 => "Choose the titles to rip.".to_owned(),
            1 => format!("1 title, {}", ripper::format_size(bytes)),
            n => format!("{n} titles, {}", ripper::format_size(bytes)),
        };
        if let Some(f) = free {
            total += &format!(" · {} free there", ripper::format_size(f));
        }
        if !room {
            total += ": not enough space";
        }
        total += ". MakeMKV copies them as they are (every audio track, subtitle and chapter).";
        ui.set_ripfiles_total(total.into());
        ui.set_ripfiles_can_start(!chosen.is_empty() && room);
    });
}

/// Ask TVmaze for the show in the name field, then name the episodes.
fn lookup_rip_names(ui: &AppWindow) {
    let name = ui.get_ripfiles_name().to_string();
    if name.trim().is_empty() {
        return;
    }
    ui.set_ripfiles_summary(format!("Looking up \"{name}\" on TVmaze…").into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let show = discrip::lookup(&name);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let found = RIP_PLAN.with_borrow_mut(|r| {
                let r = r.as_mut()?;
                match show {
                    Ok(Some(show)) => {
                        r.plan.name = show.name.clone();
                        r.plan.show = Some(show);
                        r.plan.rename();
                        Some(Ok(r.plan.name.clone()))
                    }
                    Ok(None) => Some(Err(format!("TVmaze doesn't know \"{name}\": check the name, or keep the numbered names."))),
                    Err(e) => Some(Err(format!("Couldn't look the names up ({e}); the episodes are numbered."))),
                }
            });
            match found {
                Some(Ok(n)) => {
                    ui.set_ripfiles_name(n.into());
                    refresh_rip_files(&ui);
                }
                Some(Err(msg)) => {
                    refresh_rip_files(&ui);
                    ui.set_ripfiles_summary(msg.into());
                }
                None => {}
            }
        });
    });
}

fn setup_rip_files(ui: &AppWindow) {
    let edit = |ui: &AppWindow, f: &dyn Fn(&mut RipFiles)| {
        RIP_PLAN.with_borrow_mut(|r| {
            if let Some(r) = r {
                f(r);
                r.plan.rename();
            }
        });
        refresh_rip_files(ui);
    };
    let weak = ui.as_weak();
    ui.on_ripfiles_renamed(move || {
        let Some(ui) = weak.upgrade() else { return };
        let name = ui.get_ripfiles_name().to_string();
        edit(&ui, &|r| r.plan.name = name.clone());
    });
    let weak = ui.as_weak();
    ui.on_ripfiles_season_by(move |d| {
        let Some(ui) = weak.upgrade() else { return };
        edit(&ui, &|r| {
            r.plan.season = (r.plan.season as i32 + d).clamp(1, 99) as u32
        });
    });
    let weak = ui.as_weak();
    ui.on_ripfiles_first_by(move |d| {
        let Some(ui) = weak.upgrade() else { return };
        edit(&ui, &|r| {
            r.plan.first = (r.plan.first as i32 + d).clamp(1, 999) as u32
        });
    });
    let weak = ui.as_weak();
    ui.on_ripfiles_lookup(move || {
        if let Some(ui) = weak.upgrade() {
            lookup_rip_names(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_ripfiles_toggle(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        edit(&ui, &|r| {
            if let Some(item) = r.plan.items.get_mut(i as usize) {
                item.rip = !item.rip;
            }
        });
    });
    let weak = ui.as_weak();
    ui.on_ripfiles_choose_folder(move || {
        let Some(ui) = weak.upgrade() else { return };
        let start = RIP_PLAN
            .with_borrow(|r| r.as_ref().map(|r| r.base.clone()))
            .unwrap_or_else(discrip::videos_dir);
        if let Some(dir) = rfd::FileDialog::new()
            .set_title("Rip into which folder?")
            .set_directory(&start)
            .pick_folder()
        {
            edit(&ui, &|r| r.base = dir.clone());
        }
    });
    let weak = ui.as_weak();
    ui.on_ripfiles_start(move || {
        let Some(ui) = weak.upgrade() else { return };
        let Some(r) = RIP_PLAN.with_borrow_mut(Option::take) else {
            return;
        };
        ui.set_ripfiles_open(false);
        start_rip_files(&ui, r);
    });
}

/// Rip the chosen titles one after another, each straight to its name.
fn start_rip_files(ui: &AppWindow, r: RipFiles) {
    if !ui.get_busy_text().is_empty() {
        ui.set_toast("Please wait for the current task to finish".into());
        return;
    }
    let folder = r.plan.folder(&r.base);
    let items: Vec<discrip::Item> = r.plan.items.iter().filter(|i| i.rip).cloned().collect();
    let cancel = ripper::Cancel::default();
    RIP_CANCEL.with_borrow_mut(|c| *c = Some(cancel.clone()));
    ui.set_busy_text("Ripping with MakeMKV…".into());
    show_disc_card(
        ui,
        "RIPPING WITH MAKEMKV",
        &r.plan.name,
        &r.drive,
        "Starting MakeMKV…",
    );
    ui.set_rip_percent(0.0);
    log::info!(
        "ripping {} titles from {} to {}",
        items.len(),
        r.device,
        folder.display()
    );
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let total: u64 = items.iter().map(|i| i.title.bytes.max(1)).sum();
        let started = std::time::Instant::now();
        let mut done_bytes = 0u64;
        let mut saved = Vec::new();
        let mut failure = None;
        for (n, item) in items.iter().enumerate() {
            let heading = format!("RIPPING {} OF {} WITH MAKEMKV", n + 1, items.len());
            let title = item.file.trim_end_matches(".mkv").to_owned();
            let _ = weak.upgrade_in_event_loop({
                let (heading, title) = (heading.clone(), title.clone());
                move |ui| {
                    ui.set_rip_heading(heading.into());
                    ui.set_rip_title(title.into());
                }
            });
            let progress_ui = weak.clone();
            let size = item.title.bytes.max(1);
            let before = done_bytes;
            let result = ripper::rip(
                &r.tool,
                &r.device,
                &item.title,
                &folder,
                &cancel,
                move |p| {
                    let part =
                        (before as f64 + size as f64 * f64::from(p.percent) / 100.0) / total as f64;
                    let elapsed = started.elapsed().as_secs_f64();
                    let eta = if part > 0.02 {
                        time_left((elapsed * (1.0 - part) / part) as u64)
                    } else {
                        "working out the time left…".to_owned()
                    };
                    let _ = progress_ui.upgrade_in_event_loop(move |ui| {
                        ui.set_rip_percent(part as f32);
                        if !p.stage.is_empty() {
                            ui.set_rip_stage(p.stage.into());
                        }
                        ui.set_rip_eta(eta.into());
                    });
                },
            );
            match result {
                Ok(file) => {
                    let target = free_path(&folder.join(&item.file));
                    let named = match std::fs::rename(&file, &target) {
                        Ok(()) => target,
                        Err(e) => {
                            log::warn!(
                                "couldn't rename {} to {}: {e}",
                                file.display(),
                                target.display()
                            );
                            file
                        }
                    };
                    log::info!("ripped title {} to {}", item.title.id, named.display());
                    saved.push(named);
                }
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
            done_bytes += size;
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            RIP_CANCEL.with_borrow_mut(|c| *c = None);
            ui.set_busy_text("".into());
            ui.set_rip_active(false);
            let what = |n: usize| {
                if n == 1 {
                    "1 file".to_owned()
                } else {
                    format!("{n} files")
                }
            };
            match failure {
                None => {
                    let text = format!("{} saved in\n{}", what(saved.len()), folder.display());
                    ask(
                        &ui,
                        "Disc ripped",
                        &text,
                        "Open folder",
                        "Close",
                        move |ui, yes| {
                            if yes && let Err(e) = open_folder(&folder) {
                                ui.set_toast(format!("Couldn't open the folder: {e}").into());
                            }
                        },
                    );
                }
                Some(ripper::RipError::Cancelled) => ui.set_toast(
                    format!(
                        "Rip cancelled; {} already done are in {}",
                        what(saved.len()),
                        folder.display()
                    )
                    .into(),
                ),
                Some(ripper::RipError::NeedsKey) => {
                    ui.set_toast("MakeMKV needs its free beta key before it reads discs".into());
                    ui.invoke_show_makemkv_key();
                }
                Some(e) => {
                    let mut text = e.to_string();
                    if !saved.is_empty() {
                        text += &format!(
                            "\n\n{} done before that are in {}.",
                            what(saved.len()),
                            folder.display()
                        );
                    }
                    ask(&ui, "The rip didn't finish", &text, "OK", "", |_, _| {});
                }
            }
        });
    });
}

/// `path`, or "name (2).mkv" … when a file of that name is already there.
fn free_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_owned();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| path.to_owned())
}

/// "about 12 min left"
fn time_left(secs: u64) -> String {
    match secs {
        0..60 => "less than a minute left".into(),
        60..3600 => format!("about {} min left", secs.div_ceil(60)),
        _ => format!("about {} h {} min left", secs / 3600, secs / 60 % 60),
    }
}

fn confirm_rip(
    ui: &AppWindow,
    tool: ripper::Tool,
    device: String,
    drive: String,
    info: ripper::DiscInfo,
) {
    let Some(title) = info.main_title().cloned() else {
        ui.set_toast("MakeMKV found no titles on the disc".into());
        return;
    };
    let name = if title.name.is_empty() {
        info.name.clone()
    } else {
        title.name.clone()
    };
    let Some(dest) = ripper::destination(&info.name) else {
        ui.set_toast("No home folder to save the copy in".into());
        return;
    };
    let free = ripper::free_bytes(&dest);
    if let Some(free) = free.filter(|&f| title.bytes > 0 && f < title.bytes + (title.bytes / 50)) {
        ask(
            ui,
            "Not enough space",
            &format!(
                "\"{name}\" needs {}, and {} has only {} free. Free some space and try again.",
                ripper::format_size(title.bytes),
                dest.display(),
                ripper::format_size(free)
            ),
            "OK",
            "",
            |_, _| {},
        );
        return;
    }
    let text = format!(
        "Copy \"{name}\" ({}, {}) from the {drive} to\n{}\n\nIt starts playing when the copy is \
         done. A DVD takes 10 to 30 minutes, an Ultra HD movie 20 to 60; you can cancel at any time.\n\n\
         The copy is temporary: it is deleted when playback ends, when you play something else, or \
         when you close the player.",
        ripper::format_duration(title.seconds),
        ripper::format_size(title.bytes),
        dest.display()
    );
    ask(
        ui,
        "Rip & play",
        &text,
        "Rip & play",
        "Cancel",
        move |ui, yes| {
            if yes {
                start_rip(ui, tool, device, drive, name, title, dest);
            } else {
                log::info!("rip of \"{name}\" not started");
            }
        },
    );
}

fn start_rip(
    ui: &AppWindow,
    tool: ripper::Tool,
    device: String,
    drive: String,
    name: String,
    title: ripper::Title,
    dest: PathBuf,
) {
    if !ui.get_busy_text().is_empty() {
        ui.set_toast("Please wait for the current task to finish".into());
        return;
    }
    let cancel = ripper::Cancel::default();
    RIP_CANCEL.with_borrow_mut(|c| *c = Some(cancel.clone()));
    // Holds off other disc tasks; the rip card shows the progress.
    ui.set_busy_text("Ripping with MakeMKV…".into());
    ui.set_rip_heading("RIPPING WITH MAKEMKV".into());
    ui.set_rip_title(name.into());
    ui.set_rip_drive(drive.clone().into());
    ui.set_rip_percent(0.0);
    ui.set_rip_stage("Starting MakeMKV…".into());
    ui.set_rip_eta("".into());
    ui.set_rip_active(true);
    log::info!(
        "ripping title {} from the {drive} ({device}) to {}",
        title.id,
        dest.display()
    );
    RIP_DEST.with_borrow_mut(|d| *d = Some(dest.clone()));
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let progress_ui = weak.clone();
        let started = std::time::Instant::now();
        let result = ripper::rip(&tool, &device, &title, &dest, &cancel, move |p| {
            let elapsed = started.elapsed().as_secs();
            let eta = if p.percent >= 2 {
                time_left(elapsed * u64::from(100 - p.percent) / u64::from(p.percent))
            } else {
                "working out the time left…".to_owned()
            };
            let _ = progress_ui.upgrade_in_event_loop(move |ui| {
                ui.set_rip_percent(p.percent as f32 / 100.0);
                if !p.stage.is_empty() {
                    ui.set_rip_stage(p.stage.into());
                }
                ui.set_rip_eta(eta.into());
            });
        });
        let _ = weak.upgrade_in_event_loop(move |ui| {
            RIP_CANCEL.with_borrow_mut(|c| *c = None);
            let dest = RIP_DEST.with_borrow_mut(|d| d.take());
            ui.set_busy_text("".into());
            ui.set_rip_active(false);
            if result.is_err()
                && let Some(dest) = &dest
            {
                // ripper::rip removed its file; drop the disc folder too.
                let _ = std::fs::remove_dir(dest);
            }
            match result {
                Ok(file) => {
                    log::info!("ripped to {}", file.display());
                    RIP_FILES.with_borrow_mut(|f| f.push((file.clone(), false)));
                    open_inputs(&ui, vec![Input::Path(file.clone())], false);
                    ui.set_toast("Playing the copy. It is deleted when playback ends.".into());
                }
                Err(ripper::RipError::Cancelled) => {
                    ui.set_toast("Rip cancelled; the partial copy was deleted".into())
                }
                Err(ripper::RipError::NeedsKey) => {
                    RIP_AFTER_KEY.with_borrow_mut(|r| *r = Some((device, drive)));
                    ui.set_toast("MakeMKV needs its free beta key before it reads discs".into());
                    ui.invoke_show_makemkv_key();
                }
                Err(e) => {
                    log::warn!("rip failed: {e}");
                    ask(
                        &ui,
                        "The rip didn't finish",
                        &e.to_string(),
                        "OK",
                        "",
                        |_, _| {},
                    );
                }
            }
        });
    });
}

/// Tools → Update FFmpeg: say which FFmpeg is running and where it comes
/// from, and update it when it comes from the system's packages.
fn update_ffmpeg(ui: &AppWindow) {
    let version = PLAYER
        .with_borrow(|p| p.as_ref().and_then(|p| p.get_string("ffmpeg-version")))
        .unwrap_or_default();
    run_busy(
        ui,
        "Checking FFmpeg…",
        || {
            let source = engine::source();
            let package = match &source {
                engine::Source::System(lib) => engine::installed_package(lib),
                _ => None,
            };
            (source, package, engine::pending_upgrades())
        },
        move |ui, (source, package, pending)| {
            let short = engine::short_version(&version).to_owned();
            let major = short.split('.').next().unwrap_or_default().to_owned();
            match source {
                engine::Source::Unknown => {
                    ui.set_toast(
                        "The playback engine isn't running yet: try again in a moment".into(),
                    );
                }
                engine::Source::Bundled(lib) => ask(
                    ui,
                    "FFmpeg",
                    &format!(
                        "This copy of Super Media Player carries its own FFmpeg {short} ({}). It is \
                         updated together with the player: install a newer release to get a newer \
                         FFmpeg.",
                        lib.parent()
                            .map(|d| d.display().to_string())
                            .unwrap_or_default()
                    ),
                    "OK",
                    "",
                    |_, _| {},
                ),
                engine::Source::System(lib) => {
                    let Some(pending) = pending else {
                        ask(
                            ui,
                            "FFmpeg",
                            &format!(
                                "FFmpeg {short} comes from your system ({}). Update it with your \
                                 system's software updater.",
                                lib.display()
                            ),
                            "OK",
                            "",
                            |_, _| {},
                        );
                        return;
                    };
                    let from = package
                        .map(|(p, v)| format!("the package {p} {v}"))
                        .unwrap_or_else(|| lib.display().to_string());
                    let waiting = if pending.is_empty() {
                        "No update was waiting at your system's last check; Check & update looks \
                         again."
                            .to_owned()
                    } else {
                        format!(
                            "Updates waiting: {}.",
                            pending
                                .iter()
                                .map(|u| format!("{} {}", u.package, u.available))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    let text = format!(
                        "Super Media Player uses FFmpeg {short} from your system ({from}).\n\n\
                         {waiting}\n\nCheck & update refreshes your system's package lists and \
                         installs any newer FFmpeg and libmpv packages; it asks for your password \
                         once. Your system stays on FFmpeg {major}.x with its security fixes: a \
                         newer FFmpeg needs a libmpv built for it, which comes with a newer player \
                         release."
                    );
                    ask(
                        ui,
                        "FFmpeg updates",
                        &text,
                        "Check & update",
                        "Close",
                        |ui, yes| {
                            if yes {
                                run_busy(
                                    ui,
                                    "Updating FFmpeg (enter your password when asked)…",
                                    engine::update_system,
                                    |ui, result| match result {
                                        Ok(true) => ask(
                                            ui,
                                            "FFmpeg updated",
                                            "The new FFmpeg is used after a restart of the player.",
                                            "Restart now",
                                            "Later",
                                            |_, yes| {
                                                if yes {
                                                    restart_player();
                                                }
                                            },
                                        ),
                                        Ok(false) => ui.set_toast("FFmpeg is up to date".into()),
                                        Err(e) => ask(ui, "FFmpeg update", &e, "OK", "", |_, _| {}),
                                    },
                                );
                            }
                        },
                    );
                }
            }
        },
    );
}

/// Start a fresh copy of the player (same arguments and environment) and
/// quit this one.
fn restart_player() {
    match std::env::current_exe() {
        Ok(exe) => match std::process::Command::new(exe)
            .args(std::env::args().skip(1))
            .spawn()
        {
            Ok(_) => {
                let _ = slint::quit_event_loop();
            }
            Err(e) => log::warn!("could not restart: {e}"),
        },
        Err(e) => log::warn!("could not restart: {e}"),
    }
}

/// Fill in the MakeMKV panel's status (runs MakeMKV once, off the UI thread).
fn refresh_makemkv(ui: &AppWindow) {
    ui.set_makemkv_status("".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let installed = ripper::tool().is_some();
        let (ready, text) = ripper::status();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_makemkv_installed(installed);
            ui.set_makemkv_ready(ready);
            ui.set_makemkv_status(text.into());
        });
    });
}

/// Tools → MakeMKV (key & help) → Register: hand the key to MakeMKV, then start a rip that was
/// waiting for it.
fn register_makemkv(ui: &AppWindow, key: String) {
    run_busy(
        ui,
        "Registering MakeMKV…",
        move || {
            let tool = ripper::tool().ok_or("MakeMKV isn't installed: see Tools → System check")?;
            ripper::register(&tool, &key)
        },
        |ui, result| match result {
            Ok(()) => {
                ui.set_toast("MakeMKV accepted the key".into());
                refresh_makemkv(ui);
                if let Some((device, drive)) = RIP_AFTER_KEY.with_borrow_mut(|r| r.take()) {
                    ui.set_makemkv_open(false);
                    if device.is_empty() {
                        rip_disc(ui);
                    } else {
                        rip_from(ui, device, drive);
                    }
                }
            }
            Err(e) => {
                RIP_AFTER_KEY.with_borrow_mut(|r| *r = None);
                ui.set_toast(e.to_string().into());
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pasted_locations() {
        assert_eq!(
            parse_input("https://youtu.be/x"),
            Some(Input::Url("https://youtu.be/x".into()))
        );
        assert_eq!(
            parse_input("file:///home/me/My%20Music/a.flac"),
            Some(Input::Path("/home/me/My Music/a.flac".into()))
        );
        assert_eq!(
            parse_input("  '/nonexistent/x y.mkv' "),
            Some(Input::Path("/nonexistent/x y.mkv".into()))
        );
        assert_eq!(parse_input("   "), None);
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%e2%82%ac"), "€");
    }

    #[test]
    fn natural_order() {
        let mut v = vec![
            "Track 10.mp3",
            "track 2.mp3",
            "Track 1.mp3",
            "b.mp3",
            "a01.mp3",
        ];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v,
            [
                "a01.mp3",
                "b.mp3",
                "Track 1.mp3",
                "track 2.mp3",
                "Track 10.mp3"
            ]
        );
    }

    #[test]
    fn folder_scan_skips_playlists_and_disc_folders() {
        let root = std::env::temp_dir().join(format!("smp-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("CD2")).unwrap();
        std::fs::create_dir_all(root.join("VIDEO_TS")).unwrap();
        for f in [
            "10.flac",
            "2.flac",
            "list.m3u",
            "cover.jpg",
            "CD2/1.flac",
            "VIDEO_TS/VTS_01_1.VOB",
            ".hidden.mp3",
        ] {
            std::fs::write(root.join(f), b"").unwrap();
        }
        let names: Vec<String> = collect_media(&root)
            .iter()
            .map(|p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["2.flac", "10.flac", "CD2/1.flac"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod menu_tests {
    /// The skins' menu (a copy, since Slint can't share menu definitions)
    /// must offer the same entries as the menu bar, minus the shortcut
    /// labels context menus can't show.
    #[test]
    fn skin_menu_matches_menu_bar() {
        let ui = include_str!("../ui/app.slint");
        let normalize = |s: &str| -> Vec<String> {
            let mut s = s.to_owned();
            while let Some(i) = s.find(" shortcut: @keys(") {
                let end = s[i..].find(");").map_or(s.len(), |e| i + e + 2);
                s.replace_range(i..end, "");
            }
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let bar_start = ui.find("MenuBar {").expect("menu bar") + "MenuBar {".len();
        let mut depth = 1;
        let mut end = bar_start;
        for (i, c) in ui[bar_start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                end = bar_start + i;
                break;
            }
        }
        let bar = normalize(&ui[bar_start..end]);
        let a = ui
            .find("// ---- skin menu entries ----")
            .expect("skin menu start");
        let b = ui
            .find("// ---- end of skin menu entries ----")
            .expect("skin menu end");
        let skin = normalize(&ui[a + "// ---- skin menu entries ----".len()..b]);
        assert_eq!(
            bar, skin,
            "update the skin menu in app.slint to match the menu bar"
        );
    }
}

#[cfg(test)]
mod rip_tests {
    #[test]
    fn time_left_reads_naturally() {
        assert_eq!(super::time_left(30), "less than a minute left");
        assert_eq!(super::time_left(61), "about 2 min left");
        assert_eq!(super::time_left(3 * 3600 + 5 * 60), "about 3 h 5 min left");
    }
}
