//! Rip & play: discs this player can't read itself (an Ultra HD Blu-ray in
//! a drive whose LibreDrive mode only MakeMKV can switch on) are copied with
//! MakeMKV's command-line tool, `makemkvcon`, to an .mkv file, which then
//! plays like any other.
//!
//! `makemkvcon -r` ("robot" mode, makemkv.com/developers) prints one record
//! per line, `KIND:field,field,...`, with quoted strings:
//! - `TINFO:title,attribute,code,"value"` / `CINFO:attribute,code,"value"`:
//!   disc and title information (attribute ids below);
//! - `PRGV:current,total,max`: progress; `PRGT`/`PRGC`: what is in progress;
//! - `MSG:code,flags,count,"message",...`: messages.
//!
//! MakeMKV reads discs only while registered (its beta key is free and
//! public); [`register`] passes a key to its own `reg` command. Everything
//! here blocks: run it off the UI thread.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::doctor;

// Attribute ids (MakeMKV's apdefs.h).
const AP_NAME: u32 = 2;
const AP_CHAPTERS: u32 = 8;
const AP_DURATION: u32 = 9;
const AP_SIZE_BYTES: u32 = 11;
const AP_OUTPUT_FILE: u32 = 27;
const AP_VOLUME_NAME: u32 = 32;

/// Flathub's MakeMKV, kept current by MakeMKV's own releases.
const FLATPAK_ID: &str = "com.makemkv.MakeMKV";

/// How to run makemkvcon: a program plus leading arguments (the Flatpak
/// one runs through `flatpak run`).
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    program: PathBuf,
    args: Vec<String>,
    /// For messages: "MakeMKV (Flatpak)".
    pub label: &'static str,
    /// MakeMKV's settings file for this install (holds the key).
    settings: Option<PathBuf>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

impl Tool {
    fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        c
    }

    fn flatpak() -> Self {
        Tool {
            program: PathBuf::from("flatpak"),
            // The disc drive and the Videos folder, even if the app's own
            // permissions are narrower.
            args: [
                "run",
                "--device=all",
                "--filesystem=xdg-videos",
                "--command=makemkvcon",
                FLATPAK_ID,
            ]
            .map(str::to_owned)
            .to_vec(),
            label: "MakeMKV (Flatpak)",
            // The Flatpak shares the real ~/.MakeMKV (its permissions say so).
            settings: home().map(|h| h.join(".MakeMKV/settings.conf")),
        }
    }
}

pub fn flatpak_installed() -> bool {
    Command::new("flatpak")
        .args(["info", FLATPAK_ID])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The makemkvcon to run: a working system install, else the Flatpak (MakeMKV
/// keeps it current), else the snap (often out of date: an expired MakeMKV
/// version refuses even a valid beta key).
pub fn tool() -> Option<Tool> {
    if doctor::makemkv_works() {
        return Some(Tool {
            program: PathBuf::from("makemkvcon"),
            args: Vec::new(),
            label: "MakeMKV",
            settings: home().map(|h| h.join(".MakeMKV/settings.conf")),
        });
    }
    if flatpak_installed() {
        return Some(Tool::flatpak());
    }
    let snap = Path::new("/snap/bin/makemkvcon");
    snap.exists().then(|| Tool {
        program: snap.to_owned(),
        args: Vec::new(),
        label: "MakeMKV (snap)",
        settings: home().map(|h| h.join("snap/makemkv/current/.MakeMKV/settings.conf")),
    })
}

/// The command that installs the Flatpak MakeMKV.
pub const FLATPAK_INSTALL: &str = "flatpak install -y flathub com.makemkv.MakeMKV";
/// Updating it, when MakeMKV says its version is too old.
pub const FLATPAK_UPDATE: &str = "flatpak update -y com.makemkv.MakeMKV";
/// MakeMKV's forum post with the current free beta key.
pub const KEY_PAGE: &str = "https://forum.makemkv.com/forum/viewtopic.php?t=1053";

/// Install the Flatpak MakeMKV for this user only, which needs no
/// administrator password (adds Flathub for the user if needed). Downloads
/// a few hundred MB the first time.
pub fn install_flatpak() -> Result<(), String> {
    let run = |args: &[&str]| -> Result<(), String> {
        let out = Command::new("flatpak")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run flatpak ({e}): install Flatpak first"))?;
        if out.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed");
            Err(format!("flatpak {}: {}", args[0], last.trim()))
        }
    };
    run(&[
        "remote-add",
        "--user",
        "--if-not-exists",
        "flathub",
        "https://dl.flathub.org/repo/flathub.flatpakrepo",
    ])?;
    run(&["install", "--user", "-y", "--noninteractive", "flathub", FLATPAK_ID])
}

/// The Flatpak MakeMKV's version, e.g. "2.0.0".
fn flatpak_version() -> Option<String> {
    let out = Command::new("flatpak").args(["info", FLATPAK_ID]).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("Version:").map(|v| v.trim().to_owned()))
}

/// Asks MakeMKV for nothing: it starts, says whether it is registered, and
/// leaves the drives alone. Without `--noscan` (or with `info disc:9999`) it
/// looks into every drive, which switches a Blu-ray drive to raw reads until
/// the disc is ejected (see bluray.rs).
const CHECK_ARGS: &[&str] = &["-r", "--cache=1", "--noscan", "info", "iso:/nonexistent.iso"];

/// What the MakeMKV help panel shows at the top: which MakeMKV is used and
/// whether it is ready. Blocking (runs MakeMKV once). `(ready, text)`.
pub fn status() -> (bool, String) {
    let Some(tool) = tool() else {
        return (false, "MakeMKV isn't installed yet: start with step 1.".into());
    };
    let version = (tool.label == "MakeMKV (Flatpak)")
        .then(flatpak_version)
        .flatten()
        .map(|v| format!(" {v}"))
        .unwrap_or_default();
    let name = format!("{}{version}", tool.label);
    let out = tool
        .command()
        .args(CHECK_ARGS)
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) if String::from_utf8_lossy(&o.stdout).lines().any(needs_key) => (
            false,
            format!("{name} is installed but not registered (or too old): do steps 2 and 3."),
        ),
        Ok(_) => (true, format!("{name} is installed and registered: ready for Rip & play.")),
        Err(e) => (false, format!("{name} is installed but doesn't start: {e}")),
    }
}

#[derive(Debug, PartialEq)]
pub enum RipError {
    /// MakeMKV refuses to read discs until it gets a (new) beta key.
    NeedsKey,
    /// The drive is still trying to load the disc (MakeMKV drive state 3).
    StillLoading,
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for RipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RipError::NeedsKey => write!(f, "MakeMKV needs its registration key (or a newer version)"),
            RipError::StillLoading => write!(
                f,
                "The drive keeps trying to load the disc but never manages to: it spins without \
                 reading it, so MakeMKV can't open it either. Check the disc (label up, clean). If \
                 a regular Blu-ray works in this drive, the drive can't read this kind of disc: an \
                 Ultra HD Blu-ray needs a UHD-friendly drive (see Tools → System check)."
            ),
            RipError::Cancelled => write!(f, "cancelled"),
            RipError::Failed(m) => write!(f, "{m}"),
        }
    }
}

/// One title (a movie, an episode, an extra) on the disc.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Title {
    pub id: u32,
    pub name: String,
    pub seconds: u64,
    pub bytes: u64,
    pub chapters: u32,
    /// The file name MakeMKV will write, e.g. "title_t00.mkv".
    pub output: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiscInfo {
    pub name: String,
    pub titles: Vec<Title>,
}

impl DiscInfo {
    /// The main movie: the longest title (the largest when equally long).
    pub fn main_title(&self) -> Option<&Title> {
        self.titles.iter().max_by_key(|t| (t.seconds, t.bytes))
    }
}

/// Split a robot-mode record's fields: commas separate, double quotes
/// enclose strings, and a backslash or doubled quote escapes a quote.
fn fields(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            '\\' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// "2:13:44" → seconds.
fn duration_secs(s: &str) -> u64 {
    s.split(':')
        .try_fold(0u64, |acc, p| p.trim().parse::<u64>().ok().map(|n| acc * 60 + n))
        .unwrap_or(0)
}

/// MakeMKV saying it won't work until registered: "This application version
/// is too old" (an expired beta, MSG 5021) or an expired evaluation.
fn needs_key(line: &str) -> bool {
    let l = line.to_lowercase();
    line.starts_with("MSG:5021,")
        || (line.starts_with("MSG:")
            && (l.contains("too old") || l.contains("evaluation period") || l.contains("registration key")))
}

/// Parse `makemkvcon -r info` output for the disc in `device`.
fn parse_info(out: &str, device: &str) -> Result<DiscInfo, RipError> {
    let mut info = DiscInfo::default();
    let mut errors = Vec::new();
    // DRV:index,state,...,"drive","disc","/dev/srN". States: 0 no disc,
    // 1 tray open, 2 disc in, 3 still loading the disc.
    let mut drive_state: Option<u32> = None;
    for line in out.lines() {
        if needs_key(line) {
            return Err(RipError::NeedsKey);
        }
        let Some((kind, rest)) = line.split_once(':') else { continue };
        let f = fields(rest);
        match kind {
            "CINFO" if f.len() >= 3 => {
                let attr: u32 = f[0].parse().unwrap_or(0);
                if attr == AP_NAME || (attr == AP_VOLUME_NAME && info.name.is_empty()) {
                    info.name = f[2].clone();
                }
            }
            "TINFO" if f.len() >= 4 => {
                let (Ok(id), Ok(attr)) = (f[0].parse::<u32>(), f[1].parse::<u32>()) else {
                    continue;
                };
                let i = match info.titles.iter().position(|t| t.id == id) {
                    Some(i) => i,
                    None => {
                        info.titles.push(Title { id, ..Title::default() });
                        info.titles.len() - 1
                    }
                };
                let t = &mut info.titles[i];
                let v = &f[3];
                match attr {
                    AP_NAME => t.name = v.clone(),
                    AP_CHAPTERS => t.chapters = v.parse().unwrap_or(0),
                    AP_DURATION => t.seconds = duration_secs(v),
                    AP_SIZE_BYTES => t.bytes = v.parse().unwrap_or(0),
                    AP_OUTPUT_FILE => t.output = v.clone(),
                    _ => {}
                }
            }
            "DRV" if f.len() >= 7 && f[6] == device => {
                drive_state = f[1].parse().ok();
            }
            // Messages flagged as errors (flags bit 1) explain an empty result.
            "MSG" if f.len() >= 4 && f[1].parse::<u32>().is_ok_and(|fl| fl & 2 != 0) => {
                errors.push(f[3].clone());
            }
            _ => {}
        }
    }
    const CANT_READ: &str = "Check the disc (label up, clean). If a regular Blu-ray works in \
        this drive, the drive can't read this kind of disc: an Ultra HD Blu-ray needs a \
        UHD-friendly drive (see Tools → System check).";
    if info.titles.is_empty() {
        match drive_state {
            Some(0) => {
                return Err(RipError::Failed(format!(
                    "MakeMKV sees no disc in {device} either: the drive doesn't recognise it. {CANT_READ}"
                )));
            }
            Some(1) => {
                return Err(RipError::Failed(format!(
                    "The tray of {device} is open: close it and try again."
                )));
            }
            Some(3) => return Err(RipError::StillLoading),
            _ => {}
        }
    }
    if info.titles.is_empty() {
        return Err(RipError::Failed(match errors.last() {
            Some(e) => format!("MakeMKV found no titles on the disc: {e}"),
            None => "MakeMKV found no titles on the disc".into(),
        }));
    }
    Ok(info)
}

/// Read the disc in `device` ("/dev/sr1"): its name and titles. A drive
/// still loading the disc (just inserted, spinning up) gets one more go
/// after a pause.
pub fn disc_info(tool: &Tool, device: &str) -> Result<DiscInfo, RipError> {
    let run = || -> Result<DiscInfo, RipError> {
        let out = tool
            .command()
            .args(["-r", "--cache=1", "info", &format!("dev:{device}")])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| RipError::Failed(format!("could not run MakeMKV: {e}")))?;
        parse_info(&String::from_utf8_lossy(&out.stdout), device)
    };
    match run() {
        Err(RipError::StillLoading) => {
            log::info!("{device}: still loading the disc; asking MakeMKV again in 15 s");
            std::thread::sleep(std::time::Duration::from_secs(15));
            run()
        }
        r => r,
    }
}

/// `settings` (a MakeMKV settings.conf) with `app_Key` set to `key`, other
/// lines kept.
fn with_key(settings: &str, key: &str) -> String {
    let line = format!("app_Key = \"{key}\"");
    let mut found = false;
    let mut out: Vec<String> = settings
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("app_Key") {
                found = true;
                line.clone()
            } else {
                l.to_owned()
            }
        })
        .collect();
    if !found {
        if out.is_empty() {
            out.push("# MakeMKV settings file".into());
        }
        out.push(line);
    }
    out.join("\n") + "\n"
}

/// Give MakeMKV a registration key. A purchased key goes through MakeMKV's
/// own `reg` command; the free beta key ("T-…") isn't accepted there ("Key
/// not found or invalid"): MakeMKV reads it from `app_Key` in its settings
/// file, as its Help → Register does. Then check MakeMKV no longer asks.
pub fn register(tool: &Tool, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.len() < 20 || key.chars().any(|c| c.is_whitespace() || c == '"') {
        return Err("That doesn't look like a MakeMKV key".into());
    }
    let via_reg = !key.starts_with("T-")
        && tool
            .command()
            .args(["reg", key])
            .stdin(Stdio::null())
            .output()
            .is_ok_and(|o| {
                o.status.success()
                    && !String::from_utf8_lossy(&o.stdout).to_lowercase().contains("invalid")
            });
    if !via_reg {
        let path = tool.settings.clone().ok_or("no home folder")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::write(&path, with_key(&old, key))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        log::info!("MakeMKV key written to {}", path.display());
    }
    // An expired or unregistered MakeMKV says so first.
    let out = tool
        .command()
        .args(CHECK_ARGS)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run MakeMKV: {e}"))?;
    if String::from_utf8_lossy(&out.stdout).lines().any(needs_key) {
        return Err(format!(
            "{} still refuses to work with that key. Check you copied all of it, and that this \
             MakeMKV is current (an outdated one rejects new beta keys): {FLATPAK_INSTALL}",
            tool.label
        ));
    }
    Ok(())
}

/// Progress of a running rip.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    /// 0..=100
    pub percent: u32,
    /// What MakeMKV is doing, e.g. "Saving to MKV file".
    pub stage: String,
}

/// Lets the UI stop a running rip.
#[derive(Clone, Default)]
pub struct Cancel(Arc<Mutex<Option<Child>>>, Arc<std::sync::atomic::AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.1.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(child) = self.0.lock().ok().as_mut().and_then(|c| c.as_mut()) {
            let _ = child.kill();
        }
    }
    /// The flag [`Cancel::cancel`] sets, for work that isn't a MakeMKV run.
    pub fn flag(&self) -> &std::sync::atomic::AtomicBool {
        &self.1
    }
    fn cancelled(&self) -> bool {
        self.1.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Rip `title` from `device` into `dest`, reporting progress, and return
/// the file written. A cancelled or failed rip leaves no partial file.
pub fn rip(
    tool: &Tool,
    device: &str,
    title: &Title,
    dest: &Path,
    cancel: &Cancel,
    mut progress: impl FnMut(Progress),
) -> Result<PathBuf, RipError> {
    std::fs::create_dir_all(dest)
        .map_err(|e| RipError::Failed(format!("{}: {e}", dest.display())))?;
    // Files from earlier rips into the same folder are older than this; only
    // what this rip wrote may be cleaned up or reported.
    let started = std::time::SystemTime::now() - std::time::Duration::from_secs(2);
    let mut child = tool.command()
        .args(["-r", "--progress=-same", "--cache=1", "mkv"])
        .arg(format!("dev:{device}"))
        .arg(title.id.to_string())
        .arg(dest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| RipError::Failed(format!("could not run MakeMKV: {e}")))?;
    let stdout = child.stdout.take().expect("piped stdout");
    if let Ok(mut slot) = cancel.0.lock() {
        *slot = Some(child);
    }

    // Just the file name: the name comes from the disc, via MakeMKV.
    let expected = Path::new(&title.output).file_name().map(|n| dest.join(n));
    let mut state = Progress::default();
    let mut errors = Vec::new();
    let mut key_problem = false;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if needs_key(&line) {
            key_problem = true;
        }
        let Some((kind, rest)) = line.split_once(':') else { continue };
        let f = fields(rest);
        match kind {
            "PRGV" if f.len() >= 3 => {
                let (total, max) = (f[1].parse::<u64>().unwrap_or(0), f[2].parse::<u64>().unwrap_or(0));
                if let Some(pct) = (total * 100).checked_div(max) {
                    let pct = pct.min(100) as u32;
                    if pct != state.percent {
                        state.percent = pct;
                        progress(state.clone());
                    }
                }
            }
            "PRGT" | "PRGC" if f.len() >= 3 => {
                if kind == "PRGT" || state.stage.is_empty() {
                    state.stage = f[2].clone();
                    progress(state.clone());
                }
            }
            "MSG" if f.len() >= 4 && f[1].parse::<u32>().is_ok_and(|fl| fl & 2 != 0) => {
                errors.push(f[3].clone());
            }
            _ => {}
        }
    }
    let status = cancel
        .0
        .lock()
        .ok()
        .and_then(|mut c| c.take())
        .and_then(|mut c| c.wait().ok());

    let this_rip = |p: &PathBuf| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= started)
    };
    let written = expected
        .filter(this_rip)
        .or_else(|| newest_mkv(dest).filter(this_rip));
    let cleanup = || {
        if let Some(p) = &written {
            let _ = std::fs::remove_file(p);
        }
    };
    if cancel.cancelled() {
        cleanup();
        return Err(RipError::Cancelled);
    }
    if key_problem {
        cleanup();
        return Err(RipError::NeedsKey);
    }
    match (status.is_some_and(|s| s.success()), written) {
        (true, Some(p)) => Ok(p),
        (_, w) => {
            if let Some(p) = &w {
                let _ = std::fs::remove_file(p);
            }
            Err(RipError::Failed(match errors.last() {
                Some(e) => format!("MakeMKV couldn't copy the disc: {e}"),
                None => "MakeMKV couldn't copy the disc".into(),
            }))
        }
    }
}

fn newest_mkv(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("mkv")))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// Where "Rip & play" copies go while they play:
/// ~/Videos/Super Media Player/Rip & play (temporary). A plain folder in the
/// home directory, which the MakeMKV snap is allowed to write. Everything in
/// it belongs to the player and is deleted once played ([`discard`],
/// [`clean_leftovers`]).
pub fn temp_root() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let videos = videos_dir(&home).unwrap_or_else(|| home.join("Videos"));
    Some(videos.join("Super Media Player").join("Rip & play (temporary)"))
}

/// The folder one disc is ripped into: [`temp_root`]/<disc name>.
pub fn destination(disc_name: &str) -> Option<PathBuf> {
    let safe: String = disc_name
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' })
        .collect();
    let safe = safe.trim().trim_matches('.').to_owned();
    let safe = if safe.is_empty() { "Disc".to_owned() } else { safe };
    Some(temp_root()?.join(safe))
}

/// Delete a "Rip & play" copy, and its disc folder once empty. Anything
/// outside [`temp_root`] is left alone.
pub fn discard(file: &Path) {
    let Some(root) = temp_root() else { return };
    if !file.starts_with(&root) || file.components().any(|c| c == std::path::Component::ParentDir) {
        return;
    }
    match std::fs::remove_file(file) {
        Ok(()) => log::info!("deleted the rip-and-play copy {}", file.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("could not delete {}: {e}", file.display()),
    }
    if let Some(dir) = file.parent().filter(|d| *d != root) {
        let _ = std::fs::remove_dir(dir); // only succeeds when empty
    }
}

/// Delete a whole disc folder under [`temp_root`] (a rip stopped by quitting).
pub fn discard_dir(dir: &Path) {
    let Some(root) = temp_root() else { return };
    if dir.starts_with(&root) && dir != root && !dir.components().any(|c| c == std::path::Component::ParentDir) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Delete copies left behind by a player that crashed or was killed.
/// Files written to in the last `min_age` are spared: another running
/// player may be ripping or playing them.
pub fn clean_leftovers(min_age: std::time::Duration) {
    let Some(root) = temp_root() else { return };
    let Ok(dirs) = std::fs::read_dir(&root) else { return };
    let old = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= min_age)
    };
    for dir in dirs.flatten().map(|e| e.path()) {
        if dir.is_dir() {
            for f in std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()) {
                if f.is_file() && old(&f) {
                    discard(&f);
                }
            }
            let _ = std::fs::remove_dir(&dir);
        } else if old(&dir) {
            let _ = std::fs::remove_file(&dir);
        }
    }
}

/// XDG_VIDEOS_DIR from ~/.config/user-dirs.dirs, when set.
fn videos_dir(home: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(home.join(".config/user-dirs.dirs")).ok()?;
    let line = text.lines().find(|l| l.starts_with("XDG_VIDEOS_DIR="))?;
    let v = line.split_once('=')?.1.trim().trim_matches('"');
    let v = v.replace("$HOME", &home.to_string_lossy());
    (!v.is_empty()).then(|| PathBuf::from(v))
}

/// Free bytes on the file system holding `dir` (or its nearest existing
/// parent), from `df`.
pub fn free_bytes(dir: &Path) -> Option<u64> {
    let mut d = dir;
    while !d.exists() {
        d = d.parent()?;
    }
    let out = Command::new("df").args(["-B1", "--output=avail"]).arg(d).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().nth(1)?.trim().parse().ok()
}

/// "2:13:44"
pub fn format_duration(secs: u64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// "58.3 GB"
pub fn format_size(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = r#"MSG:1005,0,1,"MakeMKV v1.18.3 linux(x64-release) started","%1 started","MakeMKV v1.18.3 linux(x64-release)"
DRV:0,2,999,12,"BD-RE HL-DT-ST BD-RE  WH14NS40 1.00","MY_MOVIE","/dev/sr1"
TCOUT:3
CINFO:1,6209,"Blu-ray disc"
CINFO:2,0,"My Movie"
CINFO:32,0,"MY_MOVIE"
TINFO:0,2,0,"My Movie"
TINFO:0,8,0,"24"
TINFO:0,9,0,"2:13:44"
TINFO:0,11,0,"61543256064"
TINFO:0,27,0,"My Movie_t00.mkv"
TINFO:1,2,0,"Trailer"
TINFO:1,9,0,"0:02:31"
TINFO:1,11,0,"1132462080"
TINFO:1,27,0,"My Movie_t01.mkv"
TINFO:2,9,0,"2:13:44"
TINFO:2,11,0,"60000000000"
"#;

    #[test]
    fn parses_titles_and_picks_the_movie() {
        let info = parse_info(INFO, "/dev/sr1").unwrap();
        assert_eq!(info.name, "My Movie");
        assert_eq!(info.titles.len(), 3);
        let main = info.main_title().unwrap();
        // Same length as title 2, but larger: the full movie.
        assert_eq!(main.id, 0);
        assert_eq!(main.seconds, 2 * 3600 + 13 * 60 + 44);
        assert_eq!(main.chapters, 24);
        assert_eq!(main.output, "My Movie_t00.mkv");
        assert_eq!(format_duration(main.seconds), "2:13:44");
        assert_eq!(format_size(main.bytes), "61.5 GB");
    }

    #[test]
    fn expired_makemkv_asks_for_a_key() {
        let out = "MSG:1005,0,1,\"MakeMKV v1.18.3 started\"\nMSG:5021,131332,1,\"This application version is too old.  Please download the latest version at http://www.makemkv.com/ or enter a registration key to continue using the current version.\",\"x\"\n";
        assert_eq!(parse_info(out, "/dev/sr1"), Err(RipError::NeedsKey));
    }

    #[test]
    fn empty_disc_reports_makemkv_error() {
        let out = "MSG:2003,2,1,\"Error 'Scsi error - MEDIUM ERROR' occurred\",\"x\"\n";
        match parse_info(out, "/dev/sr1") {
            Err(RipError::Failed(m)) => assert!(m.contains("MEDIUM ERROR"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_drive_is_explained() {
        // What MakeMKV 2.0.0 printed for the user's drives (2026-11-22).
        let out = "MSG:1005,0,1,\"MakeMKV v2.0.0 linux(x64-release) started\",\"%1 started\"\n\
            DRV:0,0,999,0,\"BD-RE HL-DT-ST BD-RE  WH14NS40 1.00 K84D1TE0008\",\"\",\"/dev/sr1\"\n\
            DRV:1,2,999,0,\"DVD+R-DL TSSTcorp DVD+-RW SH-216DB F100\",\"\",\"/dev/sr0\"\n\
            MSG:5010,0,0,\"Failed to open disc\",\"Failed to open disc\"\n";
        match parse_info(out, "/dev/sr1") {
            Err(RipError::Failed(m)) => assert!(m.contains("sees no disc in /dev/sr1"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn loading_drive_is_explained() {
        // MakeMKV 2.0.0 with the user's UHD disc, later on (2026-11-23).
        let out = "DRV:0,3,999,0,\"BD-RE HL-DT-ST BD-RE  WH14NS40 1.00\",\"\",\"/dev/sr1\"\n\
            MSG:5010,0,0,\"Failed to open disc\",\"Failed to open disc\"\n";
        assert_eq!(parse_info(out, "/dev/sr1"), Err(RipError::StillLoading));
        assert!(RipError::StillLoading.to_string().contains("keeps trying to load the disc"));
    }

    #[test]
    fn key_goes_into_settings() {
        let key = "T-abcdefghijklmnopqrstuvwxyz0123456789";
        assert_eq!(with_key("", key), format!("# MakeMKV settings file\napp_Key = \"{key}\"\n"));
        let old = "# x\napp_DestinationDir = \"/home/u\"\napp_Key = \"\"\nsdf_Stop = \"\"\n";
        let new = with_key(old, key);
        assert!(new.contains(&format!("app_Key = \"{key}\"")));
        assert!(new.contains("app_DestinationDir") && new.contains("sdf_Stop"));
        assert_eq!(new.matches("app_Key").count(), 1);
    }

    #[test]
    fn field_splitting() {
        assert_eq!(fields(r#"0,2,0,"A, \"quoted\" name""#), ["0", "2", "0", "A, \"quoted\" name"]);
        assert_eq!(fields(r#"1,"x""y""#), ["1", "x\"y"]);
        assert_eq!(duration_secs("1:02:03"), 3723);
        assert_eq!(duration_secs("garbage"), 0);
    }

    #[test]
    fn destination_is_a_safe_plain_folder() {
        let d = destination("My/Movie: Part*2").unwrap();
        assert!(d.ends_with("Rip & play (temporary)/My_Movie_ Part_2"), "{}", d.display());
        assert!(destination("..").unwrap().ends_with("Rip & play (temporary)/Disc"));
    }

    #[test]
    fn discard_only_touches_the_temporary_folder() {
        let root = temp_root().unwrap();
        // Outside it (and a sneaky path that only looks inside): left alone.
        let outside = std::env::temp_dir().join(format!("smp-keep-{}.mkv", std::process::id()));
        std::fs::write(&outside, b"x").unwrap();
        discard(&outside);
        discard(&root.join("..").join(outside.file_name().unwrap()));
        assert!(outside.exists());
        let _ = std::fs::remove_file(&outside);
    }
}

#[cfg(test)]
mod machine {
    /// Ask this computer's MakeMKV about the disc in /dev/sr1:
    /// `cargo test -p super-media-player ripper_this_machine -- --ignored --nocapture`.
    #[test]
    #[ignore = "uses this machine's MakeMKV and drive"]
    fn ripper_this_machine() {
        let Some(tool) = super::tool() else {
            eprintln!("no MakeMKV");
            return;
        };
        println!("tool: {tool:?}");
        match super::disc_info(&tool, "/dev/sr1") {
            Ok(info) => println!("{} titles, main: {:?}", info.titles.len(), info.main_title()),
            Err(e) => println!("error: {e} ({e:?})"),
        }
    }
}

#[cfg(test)]
mod tool_tests {
    use super::*;

    #[test]
    fn flatpak_tool_runs_makemkvcon_with_drive_access() {
        let t = Tool::flatpak();
        let c = t.command();
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(c.get_program(), "flatpak");
        assert_eq!(args.last().map(String::as_str), Some(FLATPAK_ID));
        assert!(args.contains(&"--command=makemkvcon".to_owned()));
        assert!(args.contains(&"--device=all".to_owned()));
    }
}
