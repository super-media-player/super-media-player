//! Your own mpv Lua scripts, from the `scripts` folder beside the settings.
//!
//! mpv runs them, each in its own thread. Three things are the player's:
//!
//! - **Their home.** Scripts look for their options (`script-opts`), modules
//!   and files in mpv's configuration folder. Any `mpv.conf` there would win
//!   over the options the player needs (even `vo`), so mpv gets a folder of
//!   its own that only links to the user's folders ([`prepare_home`]).
//! - **Their keys.** mpv has no keyboard here. A key some script bound is
//!   handed over ([`wanted`]); the rest never reaches mpv. As in mpv, a
//!   *forced* binding (a menu while it is open) beats everything; an ordinary
//!   one never takes a key the player uses ([`RESERVED`]).
//! - **Misbehaviour.** A script that never returns would hold up opening
//!   files and hang the player when it quits: see [`opening`] and
//!   [`limit_shutdown`].
//!
//! mpv can't unload a script, so one that is switched off runs until the
//! player closes.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use player_core::{Player, Value};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::settings::{self, Settings};
use crate::{AppWindow, ScriptRow};

/// What scripts find in mpv's configuration folder.
#[cfg(unix)]
const LINKED: &[&str] = &["scripts", "script-opts", "script-modules", "fonts"];

/// Palette and shortcut ids are `script:<binding>`.
pub const ACTION: &str = "script:";

/// How long a local file may take to open before a script is suspected.
const SLOW_OPEN: Duration = Duration::from_secs(8);
/// How long quitting waits for scripts.
const SHUTDOWN: Duration = Duration::from_secs(4);

/// Names mpv's own clients have; a script of that name gets a number.
const TAKEN: &[&str] = &[
    "main", "osc", "stats", "console", "ytdl_hook", "auto_profiles", "select", "positioning", "commands", "context_menu",
];

/// A binding for anything that types a character.
const ANY: &str = "ANY_UNICODE";

/// The player's own keys, which a script's ordinary bindings leave alone.
const RESERVED: &[&str] = &[
    "Space", "K", "J", "F", "F11", "Esc", "Enter", "Tab", "Shift+Tab", "M", "N", "P", "L", "E", "A", "C", "S", "I", "Z", "X", "V", "[", "]",
    ".", ",", "Home", "PgUp", "PgDn", "Left", "Right", "Up", "Down", "Shift+Left", "Shift+Right", "Shift+Up", "Shift+Down",
    "Ctrl+Left", "Ctrl+Right", "Ctrl+Shift+Left", "Ctrl+Shift+Right", "Alt+Left", "Alt+Right", "Alt+Up", "Alt+Down",
    "Alt+Backspace", "Alt++", "Alt+=", "Alt+-", "Alt+_", "Ctrl++", "Ctrl+=", "Ctrl+-", "Ctrl+_", "F1",
    "Ctrl+O", "Ctrl+Shift+O", "Ctrl+U", "Ctrl+V", "Ctrl+D", "Ctrl+Shift+D", "Ctrl+T", "Ctrl+K", "Ctrl+J", "Ctrl+P", "Ctrl+I",
    "Ctrl+M", "Ctrl+Shift+M", "Ctrl+L", "Ctrl+Shift+L", "Ctrl+S", "Ctrl+B", "Ctrl+Q", "Ctrl+F",
];

/// The player's key names (shortcuts.rs) and mpv's.
const KEYS: &[(&str, &str)] = &[
    ("Left", "LEFT"),
    ("Right", "RIGHT"),
    ("Up", "UP"),
    ("Down", "DOWN"),
    ("PgUp", "PGUP"),
    ("PgDn", "PGDWN"),
    ("Home", "HOME"),
    ("End", "END"),
    ("Insert", "INS"),
    ("Delete", "DEL"),
    ("Backspace", "BS"),
    ("Enter", "ENTER"),
    ("Tab", "TAB"),
    ("Esc", "ESC"),
    ("Space", "SPACE"),
    ("#", "SHARP"),
];

const README: &str = "\
Super Media Player: your own mpv scripts
========================================

Put Lua scripts made for mpv here: a file \"name.lua\", or a folder with a
\"main.lua\" inside. Then use Settings > Scripts > Look again, or restart.

Scripts run with your rights, like any program: only add ones you trust.

- A script's options go in the folder next to this one:
  script-opts/name.conf, with lines such as  key=value
- Modules for scripts: script-modules, also next to this folder.
- Scripts can use any key the player doesn't (Help > Keyboard shortcuts
  lists the player's). What a script put on a taken key is still in the
  command palette (Ctrl+K), and Settings > Shortcuts can give it another.
- Text a script shows is drawn on the picture, so there is none while music
  plays. Mouse buttons don't reach scripts. mpv.conf and input.conf aren't
  read.
- A script that is changed or switched off keeps running as it was until the
  player is restarted.

An example, hello.lua:

    mp.add_key_binding(\"h\", \"hello\", function()
        mp.osd_message(\"Playing \" .. mp.get_property(\"media-title\", \"nothing\"))
    end)
";

#[derive(Clone, Debug, PartialEq)]
enum Status {
    Off,
    Running,
    Failed(String),
}

#[derive(Clone, Debug)]
struct Script {
    path: PathBuf,
    /// The file's or folder's name, as it is shown and remembered.
    file: String,
    /// What mpv calls it.
    name: String,
    status: Status,
}

/// A key some script asked for.
#[derive(Clone, Debug, PartialEq)]
struct Binding {
    /// One key, or several to press after each other ("g-p").
    keys: Vec<String>,
    owner: String,
    /// What it is called: "hello/greet".
    action: String,
    forced: bool,
}

thread_local! {
    static SCRIPTS: RefCell<Vec<Script>> = const { RefCell::new(Vec::new()) };
    /// Scripts were on when the player started.
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// Something was opened since the player started.
    static STARTED: Cell<bool> = const { Cell::new(false) };
    /// The keys scripts hold down now, and the last one given to them.
    static HELD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static LAST: RefCell<String> = const { RefCell::new(String::new()) };
    static SLOW: RefCell<slint::Timer> = RefCell::new(slint::Timer::default());
    static LATER: RefCell<slint::Timer> = RefCell::new(slint::Timer::default());
}

/// The folder scripts are loaded from.
pub fn user_dir() -> Option<PathBuf> {
    settings::config_dir().map(|d| d.join("scripts"))
}

/// mpv's configuration folder: links to what scripts need, and nothing else
/// of the user's.
#[cfg(unix)]
fn prepare_home(home: &Path, config: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    for name in LINKED {
        let (link, target) = (home.join(name), config.join(name));
        match std::fs::symlink_metadata(&link) {
            Ok(m) if !m.file_type().is_symlink() => continue,
            Ok(_) if std::fs::read_link(&link).is_ok_and(|t| t == target) => continue,
            Ok(_) => std::fs::remove_file(&link)?,
            Err(_) => {}
        }
        std::os::unix::fs::symlink(&target, &link)?;
    }
    // Nothing the player makes; mpv would obey it.
    for name in ["mpv.conf", "config"] {
        if home.join(name).exists() {
            return Err(std::io::Error::other(format!("{} must not be there", home.join(name).display())));
        }
    }
    Ok(())
}

/// mpv options for running scripts, set before it starts.
pub fn options(s: &Settings) -> Vec<(&'static str, String)> {
    if s.scripts { options_in(settings::data_dir().map(|d| d.join("mpv")), settings::config_dir()) } else { Vec::new() }
}

#[cfg_attr(not(unix), allow(unused_variables, unused_mut))]
fn options_in(home: Option<PathBuf>, config: Option<PathBuf>) -> Vec<(&'static str, String)> {
    let mut out = vec![
        // They are loaded one by one, so each can be switched off.
        ("load-scripts", "no".to_owned()),
        // Ordinary script bindings count as "default" ones; mpv's own stay off.
        ("input-default-bindings", "yes".to_owned()),
        ("input-builtin-bindings", "no".to_owned()),
    ];
    #[cfg(unix)]
    if let (Some(home), Some(config)) = (home, config) {
        match (prepare_home(&home, &config), home.to_str()) {
            (Ok(()), Some(dir)) => {
                out.push(("config", "yes".to_owned()));
                out.push(("config-dir", dir.to_owned()));
                // With a configuration folder mpv would start caching and
                // resuming there by itself.
                for option in ["gpu-shader-cache", "icc-cache", "resume-playback"] {
                    out.push((option, "no".to_owned()));
                }
            }
            (Err(e), _) => log::warn!("scripts run without their options: {e}"),
            _ => {}
        }
    }
    out
}

/// The scripts in `dir`, by name.
fn find(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let hidden = p.file_name().is_none_or(|n| n.to_string_lossy().starts_with('.'));
            let lua = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lua"));
            !hidden && p.to_str().is_some() && if p.is_dir() { p.join("main.lua").is_file() } else { lua && p.is_file() }
        })
        .collect();
    out.sort();
    out
}

/// What mpv will call the script in `file`, when `others` run already.
fn mpv_name(file: &str, others: &[String]) -> String {
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    let base: String = stem.chars().flat_map(|c| {
        // mpv replaces byte by byte.
        let n = if c.is_ascii_alphanumeric() { 1 } else { c.len_utf8() };
        std::iter::repeat_n(if c.is_ascii_alphanumeric() { c } else { '_' }, n)
    }).collect();
    let taken = |n: &str| TAKEN.contains(&n) || others.iter().any(|o| o == n);
    if !taken(&base) {
        return base;
    }
    (2..).map(|i| format!("{base}{i}")).find(|n| !taken(n)).unwrap_or(base)
}

fn is_off(s: &Settings, file: &str) -> bool {
    s.scripts_off.split('/').any(|f| f == file)
}

fn set_off(s: &mut Settings, file: &str, off: bool) {
    let mut list: Vec<&str> = s.scripts_off.split('/').filter(|f| !f.is_empty() && *f != file).collect();
    if off {
        list.push(file);
    }
    s.scripts_off = list.join("/");
}

fn start(player: &Player, script: &mut Script) {
    let Some(path) = script.path.to_str() else { return };
    script.status = match player.command(&["load-script", path]) {
        Ok(()) => {
            log::info!("script {}", script.file);
            Status::Running
        }
        Err(e) => {
            log::warn!("script {}: {e}", script.file);
            Status::Failed("this playback engine can't run Lua scripts".into())
        }
    };
}

/// Look through the folder: start what is new (or had stopped) and switched
/// on, and forget what is gone and not running.
fn rescan(player: &Player, s: &Settings) {
    let found = user_dir().map(|d| find(&d)).unwrap_or_default();
    SCRIPTS.with_borrow_mut(|list| {
        list.retain(|x| x.status == Status::Running || found.contains(&x.path));
        for path in found {
            let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let running = list.iter().position(|x| x.path == path);
            if running.is_some_and(|i| list[i].status == Status::Running) {
                continue;
            }
            let others: Vec<String> =
                list.iter().filter(|x| x.status == Status::Running).map(|x| x.name.clone()).collect();
            let mut script = Script { path, name: mpv_name(&file, &others), file, status: Status::Off };
            if ACTIVE.get() && !is_off(s, &script.file) {
                start(player, &mut script);
            }
            match running {
                Some(i) => list[i] = script,
                None => list.push(script),
            }
        }
        list.sort_by(|a, b| a.path.cmp(&b.path));
    });
}

// -- keys ------------------------------------------------------------------

/// "Ctrl+Shift+Left" → "Shift+Ctrl+LEFT", "Shift+A" → "A": as mpv writes it
/// in `input-bindings`. `combo` is shortcuts.rs's name for a key press.
fn mpv_key(combo: &str) -> String {
    let (mut ctrl, mut alt, mut shift, mut meta) = (false, false, false, false);
    let mut base = combo;
    while let Some((m, rest)) = base.split_once('+').filter(|(_, r)| !r.is_empty()) {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "meta" => meta = true,
            _ => break,
        }
        base = rest;
    }
    let mut chars = base.chars();
    let letter = chars.next().filter(|c| c.is_alphabetic() && chars.next().is_none());
    let base = if let Some(c) = letter {
        // The letter's case says whether Shift is held.
        let up = std::mem::take(&mut shift);
        if up { c.to_uppercase().collect() } else { c.to_lowercase().collect() }
    } else {
        KEYS.iter().find(|(ours, _)| *ours == base).map_or_else(|| base.to_owned(), |(_, k)| (*k).to_owned())
    };
    let mut out = String::new();
    for (on, name) in [(shift, "Shift+"), (ctrl, "Ctrl+"), (alt, "Alt+"), (meta, "Meta+")] {
        if on {
            out += name;
        }
    }
    out + &base
}

/// mpv's name for a key, as the player writes it: "Ctrl+Shift+Left".
fn shown_key(key: &str) -> String {
    let mut base = key;
    let mut mods = Vec::new();
    while let Some((m, rest)) = base.split_once('+').filter(|(m, r)| !r.is_empty() && ["Shift", "Ctrl", "Alt", "Meta"].contains(m)) {
        mods.push(m);
        base = rest;
    }
    let mut chars = base.chars();
    let letter = chars.next().filter(|c| c.is_alphabetic() && chars.next().is_none());
    let base = if let Some(c) = letter {
        if c.is_uppercase() {
            mods.push("Shift");
        }
        c.to_uppercase().collect()
    } else {
        KEYS.iter().find(|(_, k)| *k == base).map_or_else(|| base.to_owned(), |(ours, _)| (*ours).to_owned())
    };
    let mut out = String::new();
    for m in ["Ctrl", "Alt", "Shift", "Meta"] {
        if mods.contains(&m) {
            out += m;
            out.push('+');
        }
    }
    out + &base
}

fn reserved(key: &str) -> bool {
    RESERVED.iter().any(|r| mpv_key(r) == key)
}

/// "g-p" is g and then p; "Ctrl+-" and "-" are one key.
fn sequence(key: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = key;
    while let Some(i) = rest.char_indices().skip(1).find(|&(i, c)| c == '-' && !rest[..i].ends_with('+') && i + 1 < rest.len()).map(|(i, _)| i) {
        out.push(rest[..i].to_owned());
        rest = &rest[i + 1..];
    }
    out.push(rest.to_owned());
    out
}

fn bindings_from(list: &Value) -> Vec<Binding> {
    list.as_array()
        .iter()
        .filter_map(|b| {
            let owner = b.str_field("owner").filter(|o| !o.is_empty())?;
            // "nonscalable script-binding hello/greet"
            let cmd = b.str_field("cmd")?;
            let action = cmd.split_once("script-binding ").map(|(_, a)| a.trim().trim_matches('"').to_owned())?;
            // Sections of a script that are switched off (a closed menu).
            if b.get("priority").and_then(Value::as_i64).is_some_and(|p| p < 0) {
                return None;
            }
            Some(Binding { keys: sequence(&b.str_field("key")?), owner, action, forced: !b.bool_field("is_weak") })
        })
        .collect()
}

fn bindings() -> Vec<Binding> {
    if SCRIPTS.with_borrow(|s| s.iter().all(|x| x.status != Status::Running)) {
        return Vec::new();
    }
    let mut value = None;
    crate::with_player(|p| value = p.get_value("input-bindings"));
    value.map(|v| bindings_from(&v)).unwrap_or_default()
}

/// How much scripts want a key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wanted {
    No,
    /// If nothing else of the user's is on it.
    Ordinary,
    /// Before anything else.
    Forced,
}

/// `combo` as shortcuts.rs names it.
pub fn wanted(combo: &str) -> Wanted {
    let key = mpv_key(combo);
    let w = LAST.with_borrow(|last| decide(&key, last, &bindings()));
    if w == Wanted::No {
        LAST.with_borrow_mut(String::clear);
    }
    w
}

fn decide(key: &str, last: &str, bindings: &[Binding]) -> Wanted {
    let mut ordinary = false;
    // A key that types something, for scripts that take text.
    let typed = key.chars().count() == 1 || key == "SPACE" || key == "SHARP";
    for b in bindings {
        // The next key of a sequence that has begun.
        let follows = b.keys.windows(2).any(|w| w[0] == last && w[1] == key);
        let hit = b.keys[0] == key || (b.keys[0] == ANY && typed);
        if b.forced && (hit || follows) || follows {
            return Wanted::Forced;
        }
        ordinary |= hit && !reserved(key);
    }
    if ordinary { Wanted::Ordinary } else { Wanted::No }
}

/// Give a key press to the scripts. mpv repeats a held key by itself.
pub fn press(combo: &str) {
    let key = mpv_key(combo);
    let repeat = HELD.with_borrow_mut(|h| {
        let repeat = h.contains(&key);
        if !repeat {
            h.push(key.clone());
        }
        repeat
    });
    if repeat {
        return;
    }
    crate::with_player(|p| {
        if let Err(e) = p.command(&["keydown", &key]) {
            log::warn!("{e}");
        }
    });
    LAST.with_borrow_mut(|l| *l = key);
}

/// A key was let go (any key: a lost release must not leave one held).
pub fn release() {
    if HELD.with_borrow_mut(|h| std::mem::take(h).is_empty()) {
        return;
    }
    crate::with_player(|p| {
        // All keys. Without the empty name, mpv 0.37 crashes.
        let _ = p.command(&["keyup", ""]);
    });
}

// -- actions (command palette, shortcuts) ------------------------------------

/// What scripts can do: id, title, key.
pub fn actions() -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    for b in bindings() {
        let id = format!("{ACTION}{}", b.action);
        // Keys a menu puts up while it is open, and mpv's own helpers.
        let menu = b.forced && b.keys.len() == 1 && (b.keys[0] == ANY || reserved(&b.keys[0]));
        let of_user = SCRIPTS.with_borrow(|s| s.iter().any(|x| x.name == b.owner));
        if menu || !of_user || out.iter().any(|o| o.0 == id) {
            continue;
        }
        let key = if b.keys.len() == 1 && !b.forced && reserved(&b.keys[0]) { String::new() } else { shown(&b) };
        out.push((id, title(&b.action), key));
    }
    out
}

fn shown(b: &Binding) -> String {
    b.keys.iter().map(|k| shown_key(k)).collect::<Vec<_>>().join(", ")
}

/// "hello/greet" → "Script hello: greet".
pub fn title(action: &str) -> String {
    match action.strip_prefix(ACTION).unwrap_or(action).split_once('/') {
        Some((script, what)) => format!("Script {script}: {what}"),
        None => format!("Script: {action}"),
    }
}

/// Is this id ("script:hello/greet") one that could be run?
pub fn is_action(id: &str) -> bool {
    id.strip_prefix(ACTION).is_some_and(|a| !a.is_empty() && !a.contains(char::is_control))
}

pub fn run(ui: &AppWindow, action: &str) {
    crate::with_player(|p| {
        // Fails when the script isn't running.
        if let Err(e) = p.command(&["script-binding", action]) {
            log::warn!("{e}");
            ui.set_toast(format!("{}: that script isn't running (Settings → Scripts)", title(action)).into());
        }
    });
}

// -- what mpv says -----------------------------------------------------------

/// "Lua error: .../scripts/bad.lua:1: syntax error" → "bad.lua, line 1: syntax error".
fn short_error(text: &str) -> String {
    let t = text.trim().trim_start_matches("Lua error:").trim();
    let t = t.lines().next().unwrap_or(t);
    let Some((place, what)) = t.split_once(": ") else { return t.to_owned() };
    let file = place.rsplit('/').next().unwrap_or(place);
    match file.rsplit_once(':') {
        Some((f, line)) if line.chars().all(|c| c.is_ascii_digit()) => format!("{f}, line {line}: {what}"),
        _ => t.to_owned(),
    }
}

/// A line of mpv's log: a script's end shows as a fatal error under its name.
pub fn log_line(ui: &AppWindow, level: &str, prefix: &str, text: &str) {
    // After the error comes its traceback, at the same level.
    if level != "fatal" || text.trim().is_empty() {
        return;
    }
    let file = SCRIPTS.with_borrow_mut(|list| {
        let s = list.iter_mut().find(|s| s.name == prefix && s.status == Status::Running)?;
        s.status = Status::Failed(short_error(text));
        Some(s.file.clone())
    });
    if let Some(file) = file {
        ui.set_toast(format!("The script {file} stopped: {}", short_error(text)).into());
        show(ui);
    }
}

/// A file is being opened, or (`None`) no longer is.
pub fn opening(ui: &AppWindow, path: Option<&str>) {
    let local = path.is_some_and(|p| !p.contains("://"));
    if path.is_some() {
        STARTED.set(true);
    }
    let running = SCRIPTS.with_borrow(|s| s.iter().any(|x| x.status == Status::Running));
    if !local || !running {
        SLOW.with_borrow(slint::Timer::stop);
        return;
    }
    wait(ui);
}

/// The playlist has something in it. A script that never returns from the
/// start on keeps mpv from even beginning to open it.
pub fn queued(ui: &AppWindow) {
    let waiting = SLOW.with_borrow(slint::Timer::running);
    if !STARTED.get() && !waiting && SCRIPTS.with_borrow(|s| s.iter().any(|x| x.status == Status::Running)) {
        wait(ui);
    }
}

fn wait(ui: &AppWindow) {
    let weak = ui.as_weak();
    SLOW.with_borrow(|t| {
        t.start(slint::TimerMode::SingleShot, SLOW_OPEN, move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_toast(
                    "Still opening. A script may be holding it up: switch it off in Settings → Scripts, then restart the player.".into(),
                );
            }
        });
    });
}

/// Quitting: mpv waits for its scripts, so one that never returns would keep
/// the player running without a window. Everything is saved by now.
pub fn limit_shutdown() {
    if SCRIPTS.with_borrow(|s| s.iter().all(|x| x.status != Status::Running)) {
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(SHUTDOWN);
        log::warn!("a script didn't stop; leaving without it");
        std::process::exit(0);
    });
}

// -- the settings page -------------------------------------------------------

fn detail(s: &Script, off: bool, bindings: &[Binding]) -> String {
    match &s.status {
        Status::Failed(e) => format!("Stopped: {e}"),
        Status::Running if off => "Still running; off after a restart".into(),
        Status::Running if !s.path.exists() => "Removed; runs until the player is closed".into(),
        Status::Off if !off => "Runs after a restart".into(),
        Status::Off => "Off".into(),
        Status::Running => {
            let keys: Vec<String> = bindings
                .iter()
                .filter(|b| b.owner == s.name && !b.forced)
                .map(|b| {
                    let what = b.action.split_once('/').map_or(b.action.as_str(), |(_, w)| w);
                    let free = b.keys.len() > 1 || !reserved(&b.keys[0]);
                    format!("{} ({what}{})", shown(b), if free { "" } else { "; taken by the player" })
                })
                .collect();
            if keys.is_empty() { "Running".into() } else { format!("Running. Keys: {}", keys.join(", ")) }
        }
    }
}

fn show(ui: &AppWindow) {
    let list = bindings();
    let off = ui.get_scripts_off();
    let rows: Vec<ScriptRow> = SCRIPTS.with_borrow(|scripts| {
        scripts
            .iter()
            .map(|s| {
                let on = !off.split('/').any(|f| f == s.file);
                ScriptRow {
                    file: s.file.as_str().into(),
                    detail: detail(s, !on, &list).into(),
                    on,
                    problem: matches!(s.status, Status::Failed(_)),
                }
            })
            .collect()
    });
    ui.set_script_rows(ModelRc::new(VecModel::from(rows)));
    crate::shortcuts::show_actions(ui);
}

/// Show the page again once scripts have had time to bind their keys.
fn show_later(ui: &AppWindow) {
    let weak = ui.as_weak();
    LATER.with_borrow(|t| {
        t.start(slint::TimerMode::SingleShot, Duration::from_millis(800), move || {
            if let Some(ui) = weak.upgrade() {
                show(&ui);
            }
        });
    });
}

fn open_user_dir() -> std::io::Result<PathBuf> {
    let dir = user_dir().ok_or_else(|| std::io::Error::other("no config folder"))?;
    std::fs::create_dir_all(&dir)?;
    if let Some(d) = settings::config_dir() {
        std::fs::create_dir_all(d.join("script-opts"))?;
    }
    std::fs::write(dir.join("README.txt"), README)?;
    crate::open_folder(&dir)?;
    Ok(dir)
}

pub fn install(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    ACTIVE.set(settings.borrow().scripts);
    ui.set_scripts_off(settings.borrow().scripts_off.as_str().into());
    rescan(player, &settings.borrow());
    show(ui);
    show_later(ui);

    let weak = ui.as_weak();
    let player = Rc::downgrade(player);
    ui.on_scripts_action(move |what, file| {
        let (Some(ui), Some(player)) = (weak.upgrade(), player.upgrade()) else { return };
        match what.as_str() {
            "folder" => match open_user_dir() {
                Ok(dir) => ui.set_toast(format!("Scripts folder: {} (README.txt there shows how)", dir.display()).into()),
                Err(e) => ui.set_toast(format!("Couldn't open the scripts folder: {e}").into()),
            },
            "refresh" => {
                rescan(&player, &settings.borrow());
                let n = SCRIPTS.with_borrow(Vec::len);
                ui.set_toast(match n {
                    0 => "No scripts in the folder".into(),
                    1 => "1 script in the folder".into(),
                    n => format!("{n} scripts in the folder").into(),
                });
                show_later(&ui);
            }
            "toggle" => {
                let off = !is_off(&settings.borrow(), &file);
                set_off(&mut settings.borrow_mut(), &file, off);
                settings.borrow().save();
                ui.set_scripts_off(settings.borrow().scripts_off.as_str().into());
                if off {
                    let running = SCRIPTS.with_borrow_mut(|l| {
                        let s = l.iter_mut().find(|s| s.file == file.as_str());
                        s.is_some_and(|s| {
                            if matches!(s.status, Status::Failed(_)) {
                                s.status = Status::Off;
                            }
                            s.status == Status::Running
                        })
                    });
                    if running {
                        ui.set_toast(format!("{file} stops when the player is closed").into());
                    }
                } else {
                    rescan(&player, &settings.borrow());
                    show_later(&ui);
                }
            }
            _ => {}
        }
        show(&ui);
    });
    ui.on_key_let_go(release);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(key: &str, action: &str, forced: bool) -> Binding {
        let owner = action.split('/').next().unwrap().to_owned();
        Binding { keys: sequence(key), owner, action: action.into(), forced }
    }

    #[test]
    fn key_names() {
        for (ours, mpv) in [
            ("A", "a"),
            ("Shift+A", "A"),
            ("Ctrl+Shift+Y", "Ctrl+Y"),
            ("Ctrl+Shift+Left", "Shift+Ctrl+LEFT"),
            ("Alt+Shift+Left", "Shift+Alt+LEFT"),
            ("Space", "SPACE"),
            ("#", "SHARP"),
            ("+", "+"),
            ("Ctrl++", "Ctrl++"),
            ("Ctrl+-", "Ctrl+-"),
            ("Alt+F5", "Alt+F5"),
            ("Shift+Ä", "Ä"),
        ] {
            assert_eq!(mpv_key(ours), mpv, "{ours}");
            assert_eq!(mpv_key(&shown_key(mpv)), mpv, "{mpv}");
        }
        assert_eq!(shown_key("Ctrl+Y"), "Ctrl+Shift+Y");
        assert_eq!(shown_key("Shift+Ctrl+LEFT"), "Ctrl+Shift+Left");
    }

    #[test]
    fn sequences() {
        assert_eq!(sequence("g-p"), ["g", "p"]);
        assert_eq!(sequence("-"), ["-"]);
        assert_eq!(sequence("Ctrl+-"), ["Ctrl+-"]);
        assert_eq!(sequence("Ctrl+a-b"), ["Ctrl+a", "b"]);
    }

    #[test]
    fn who_gets_a_key() {
        let b = [
            binding("g", "a/go", false),
            binding("SPACE", "a/space", false),
            binding("y-n", "a/seq", false),
            binding("ENTER", "menu/ok", true),
        ];
        assert_eq!(decide("g", "", &b), Wanted::Ordinary);
        assert_eq!(decide("h", "", &b), Wanted::No);
        // The player's own key stays the player's, unless a menu is open.
        assert_eq!(decide("SPACE", "", &b), Wanted::No);
        assert_eq!(decide("ENTER", "", &b), Wanted::Forced);
        // "n" is the player's, but not in the middle of a sequence.
        assert_eq!(decide("y", "", &b), Wanted::Ordinary);
        assert_eq!(decide("n", "", &b), Wanted::No);
        assert_eq!(decide("n", "y", &b), Wanted::Forced);
        // A script that takes text gets what types, not Ctrl+Q.
        let text = [binding(ANY, "console/text", true)];
        assert_eq!(decide("k", "", &text), Wanted::Forced);
        assert_eq!(decide("SPACE", "", &text), Wanted::Forced);
        assert_eq!(decide("Ctrl+q", "", &text), Wanted::No);
        assert_eq!(decide("ESC", "", &text), Wanted::No);
    }

    /// Every key app.slint acts on is in `RESERVED`.
    #[test]
    fn the_players_keys_are_reserved() {
        let src = include_str!("../ui/app.slint");
        for part in src.split("@keys(").skip(1) {
            let k = part.split(')').next().unwrap().replace("Control", "Ctrl").replace(' ', "");
            assert!(RESERVED.contains(&k.as_str()), "{k}");
        }
        for part in src.split("e.text == ").skip(1) {
            let k = part.split([' ', ')', ';']).next().unwrap();
            let ours = match k {
                "Key.PageUp" => "PgUp",
                "Key.PageDown" => "PgDn",
                "Key.Escape" => "Esc",
                "Key.Return" => "Enter",
                "\"" => "Space",
                k => k.trim_start_matches("Key.").trim_end_matches("Arrow").trim_matches('"'),
            };
            let ours = if ours.len() == 1 { ours.to_uppercase() } else { ours.to_owned() };
            let there = RESERVED.iter().any(|r| *r == ours || ["Ctrl+", "Ctrl+Shift+", "Alt+"].iter().any(|m| *r == format!("{m}{ours}")));
            assert!(there, "{k}");
        }
    }

    #[test]
    fn names_as_in_mpv() {
        assert_eq!(mpv_name("hello.lua", &[]), "hello");
        assert_eq!(mpv_name("my script-2.lua", &[]), "my_script_2");
        assert_eq!(mpv_name("folder", &[]), "folder");
        assert_eq!(mpv_name("é.lua", &[]), "__");
        assert_eq!(mpv_name("stats.lua", &[]), "stats2");
        assert_eq!(mpv_name("a b.lua", &["a_b".into(), "a_b2".into()]), "a_b3");
    }

    #[test]
    fn errors_read_well() {
        assert_eq!(
            short_error("Lua error: .../super-media-player/scripts/bad.lua:1: syntax error near 'is'"),
            "bad.lua, line 1: syntax error near 'is'"
        );
        assert_eq!(short_error("Could not read file.\n"), "Could not read file.");
    }

    #[test]
    fn switching_off() {
        let mut s = Settings::default();
        set_off(&mut s, "a, b.lua", true);
        set_off(&mut s, "c", true);
        assert!(is_off(&s, "a, b.lua") && is_off(&s, "c") && !is_off(&s, "b.lua") && !is_off(&s, ""));
        set_off(&mut s, "a, b.lua", false);
        assert_eq!(s.scripts_off, "c");
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("smp-scripts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_scripts() {
        let d = temp("find");
        for f in ["b.lua", "a.LUA", "README.txt", ".hidden.lua", "c.js"] {
            std::fs::write(d.join(f), "").unwrap();
        }
        std::fs::create_dir_all(d.join("folder")).unwrap();
        std::fs::write(d.join("folder/main.lua"), "").unwrap();
        std::fs::create_dir_all(d.join("not.lua")).unwrap();
        let names: Vec<String> = find(&d).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.LUA", "b.lua", "folder"]);
        assert!(find(&d.join("missing")).is_empty());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_home_without_mpv_conf() {
        let d = temp("home");
        let (home, config) = (d.join("data/mpv"), d.join("config"));
        std::fs::create_dir_all(config.join("scripts")).unwrap();
        std::fs::write(config.join("mpv.conf"), "vo=null").unwrap();
        prepare_home(&home, &config).unwrap();
        // Again, and after the settings folder moved.
        prepare_home(&home, &config).unwrap();
        let moved = d.join("moved");
        prepare_home(&home, &moved).unwrap();
        assert_eq!(std::fs::read_link(home.join("scripts")).unwrap(), moved.join("scripts"));
        assert!(!home.join("mpv.conf").exists());
        std::fs::write(home.join("mpv.conf"), "").unwrap();
        assert!(prepare_home(&home, &config).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Real scripts in a real mpv: options, modules, keys, errors.
    #[test]
    fn scripts_in_mpv() {
        use player_core::{PlayerConfig, PlayerEvent};
        use std::time::Instant;
        let d = temp("mpv");
        let (home, config) = (d.join("data"), d.join("config"));
        std::fs::create_dir_all(config.join("scripts/folder")).unwrap();
        std::fs::create_dir_all(config.join("script-opts")).unwrap();
        std::fs::write(config.join("mpv.conf"), "volume=11\n").unwrap();
        std::fs::write(config.join("script-opts/my_script.conf"), "word=option\n").unwrap();
        std::fs::write(
            config.join("scripts/my script.lua"),
            "local o = {word='none'}\nrequire('mp.options').read_options(o, 'my_script')\n\
             mp.set_property('user-data/word', o.word)\n\
             mp.add_key_binding('Ctrl+Shift+LEFT', 'go', function() mp.set_property('user-data/go', 'yes') end)\n\
             mp.add_forced_key_binding('ENTER', 'ok', function() end)\n",
        )
        .unwrap();
        std::fs::write(config.join("scripts/folder/main.lua"), "mp.set_property('user-data/folder', require('other'))").unwrap();
        std::fs::write(config.join("scripts/folder/other.lua"), "return 'module'").unwrap();
        std::fs::write(config.join("scripts/bad.lua"), "this is not lua").unwrap();
        let mut cfg = PlayerConfig::headless();
        for (k, v) in options_in(Some(home), Some(config.clone())) {
            cfg.set(k, &v);
        }
        cfg.set("volume", "77");
        let (tx, rx) = std::sync::mpsc::channel();
        let Ok(player) = Player::new(&cfg, move |e| {
            let _ = tx.send(e);
        }) else {
            eprintln!("skipped: no libmpv");
            return;
        };
        for f in find(&config.join("scripts")) {
            player.command(&["load-script", f.to_str().unwrap()]).unwrap();
        }
        let until = |what: &str, f: &dyn Fn() -> bool| {
            let t = Instant::now();
            while !f() {
                assert!(t.elapsed() < Duration::from_secs(5), "{what}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let text = |p: &str| player.get_string(p).unwrap_or_default().trim_matches('"').to_owned();
        let list = || bindings_from(&player.get_value("input-bindings").unwrap_or_default());
        until("option", &|| text("user-data/word") == "option");
        until("module", &|| text("user-data/folder") == "module");
        until("bindings", &|| list().len() == 2);
        // Not from the user's mpv.conf.
        assert_eq!(player.get_double("volume"), Some(77.0));

        let b = list();
        let go = b.iter().find(|b| b.action == "my_script/go").unwrap();
        assert_eq!((go.owner.as_str(), go.forced), (mpv_name("my script.lua", &[]).as_str(), false));
        assert!(b.iter().any(|b| b.action == "my_script/ok" && b.forced && b.keys == ["ENTER"]));
        let key = mpv_key("Ctrl+Shift+Left");
        assert_eq!(go.keys, std::slice::from_ref(&key));
        player.command(&["keydown", &key]).unwrap();
        player.command(&["keyup", ""]).unwrap();
        until("key", &|| text("user-data/go") == "yes");

        until("error", &|| rx.try_iter().any(|e| matches!(e, PlayerEvent::Log { level, prefix, .. } if level == "fatal" && prefix == "bad")));
        drop(player);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
