//! System check (Tools → System check…): looks at what the player depends on
//! (decryption libraries, MakeMKV, the Blu-ray key database, yt-dlp, the
//! optical drives, the window system) and turns it into findings, each with
//! an explanation and, where there is one, a fix: a button the app can do
//! itself, or a command to copy.
//!
//! [`gather`] collects the facts (blocking: run it off the UI thread);
//! [`findings`] is plain logic over them, so it is unit-tested.

use std::path::PathBuf;
use std::time::Duration;

use crate::disc::{self, DiscKind, DriveReport, DriveState};
use crate::settings::{self, Settings};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok = 0,
    Info = 1,
    Warning = 2,
    Problem = 3,
}

/// What to do when the player runs as administrator.
pub const ELEVATED_HINT: &str = "Windows blocks dragging files onto the player while it runs as \
     administrator. Close it and start it again from the Start menu or a shortcut.";

/// Something the app can fix itself, shown as a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    InstallYtdlp,
    DownloadKeydb,
    OpenSettings,
    UpdateFfmpeg,
}

impl Action {
    /// (id handed back by the UI, button label)
    pub fn id_and_label(self) -> (&'static str, &'static str) {
        match self {
            Action::None => ("", ""),
            Action::InstallYtdlp => ("install-ytdlp", "Install yt-dlp"),
            Action::DownloadKeydb => ("install-keydb", "Download key database"),
            Action::OpenSettings => ("open-settings", "Open Settings"),
            Action::UpdateFfmpeg => ("update-ffmpeg", "Update FFmpeg"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub level: Level,
    pub title: String,
    pub detail: String,
    pub action: Action,
    /// Shell commands that fix it, for the user to copy.
    pub command: String,
}

impl Finding {
    fn new(level: Level, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level,
            title: title.into(),
            detail: detail.into(),
            action: Action::None,
            command: String::new(),
        }
    }
    fn action(mut self, a: Action) -> Self {
        self.action = a;
        self
    }
    fn command(mut self, c: impl Into<String>) -> Self {
        self.command = c.into();
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MakeMkv {
    NotFound,
    Works,
    /// `makemkvcon` is there but doesn't start; the loader's message.
    Broken(String),
}

/// Everything the check looks at.
#[derive(Clone, Debug)]
pub struct Facts {
    /// "mpv 0.37.0 · FFmpeg 6.1.1"
    pub engine: String,
    /// yt-dlp's path and version (None: found but doesn't run).
    pub ytdlp: Option<(PathBuf, Option<String>)>,
    pub dvdcss: bool,
    pub aacs: bool,
    pub bdplus: bool,
    pub mmbd: bool,
    pub makemkv: MakeMkv,
    /// MakeMKV installed as a snap, whose library other programs can't use.
    pub makemkv_snap: bool,
    /// MakeMKV from Flathub (current versions; used for Rip & play).
    pub makemkv_flatpak: bool,
    /// Key database: path, size in bytes, age in days.
    pub keydb: Option<(PathBuf, u64, u64)>,
    pub settings: Settings,
    /// Running on a Wayland desktop.
    pub wayland: bool,
    /// Running as administrator (Windows).
    pub elevated: bool,
    pub drives: Vec<DriveReport>,
    /// LibreDrive status per device, where MakeMKV could read it.
    pub libredrive: Vec<(String, LibreDrive)>,
    /// FFmpeg/libmpv package updates waiting, when FFmpeg comes from the
    /// system's packages (as of the last package-list refresh).
    pub ffmpeg_updates: Option<usize>,
}

const DVDCSS_FIX: &str = "sudo apt install libdvd-pkg && sudo dpkg-reconfigure libdvd-pkg";
const AACS_FIX: &str = "sudo apt install libaacs0 libbdplus0";
/// Building MakeMKV from source puts libmmbd where the app finds it.
const MAKEMKV_FIX: &str = "sudo apt install build-essential pkg-config libc6-dev libssl-dev \
libexpat1-dev libavcodec-dev libgl1-mesa-dev qtbase5-dev zlib1g-dev
# Download makemkv-oss-<version>.tar.gz and makemkv-bin-<version>.tar.gz from makemkv.com/download, then:
cd makemkv-oss-<version> && ./configure && make && sudo make install
cd ../makemkv-bin-<version> && make && sudo make install";

/// Collect the facts. Blocking (loads libraries, runs `makemkvcon`, reads
/// the drives); `engine` comes from the running player.
pub fn gather(settings: &Settings, engine: String) -> Facts {
    let lib = disc::library_present;
    let mut facts = Facts {
        engine,
        ytdlp: crate::tools::find_ytdlp().map(|p| {
            let v = crate::tools::ytdlp_version(&p);
            (p, v)
        }),
        dvdcss: lib(&["libdvdcss.so.2", "libdvdcss-2.dll", "libdvdcss.2.dylib"]),
        aacs: lib(&["libaacs.so.0", "libaacs.dll", "libaacs.0.dylib"]),
        bdplus: lib(&["libbdplus.so.0", "libbdplus.dll", "libbdplus.0.dylib"]),
        mmbd: lib(&["libmmbd.so.0", "libmmbd.dll", "libmmbd.dylib"]),
        makemkv: makemkv_state(),
        makemkv_flatpak: cfg!(target_os = "linux") && crate::ripper::flatpak_installed(),
        makemkv_snap: cfg!(target_os = "linux")
            && std::path::Path::new("/snap/bin/makemkv").exists(),
        keydb: settings::aacs_keydb().and_then(|p| {
            let m = std::fs::metadata(&p).ok()?;
            let age = m
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map_or(0, |d| d.as_secs() / 86_400);
            Some((p, m.len(), age))
        }),
        settings: settings.clone(),
        wayland: cfg!(target_os = "linux")
            && std::env::var_os("WAYLAND_DISPLAY").is_some()
            && !(settings.window_system == "x11" && std::env::var_os("DISPLAY").is_some()),
        elevated: crate::process::elevated(),
        drives: Vec::new(),
        libredrive: Vec::new(),
        ffmpeg_updates: matches!(crate::engine::source(), crate::engine::Source::System(_))
            .then(crate::engine::pending_upgrades)
            .flatten()
            .map(|u| u.len()),
    };
    // Only for Blu-ray drives in trouble (or holding a UHD disc): it talks
    // to the drive for a few seconds.
    facts.drives = disc::drive_reports();
    for d in &facts.drives {
        let wants = d.blu_ray
            && matches!(
                d.state,
                DriveState::Refuses | DriveState::Disc { uhd: true, .. }
            );
        if wants && let Some(info) = libredrive_info(&facts.makemkv, facts.makemkv_snap, &d.device)
        {
            facts.libredrive.push((d.device.clone(), info));
        }
    }
    facts
}

/// Whether the system's `makemkvcon` starts (not a stale build).
pub fn makemkv_works() -> bool {
    makemkv_state() == MakeMkv::Works
}

/// Run `makemkvcon` with no arguments (it prints its usage and exits) to
/// see whether it starts. A stale build fails to load its libraries.
fn makemkv_state() -> MakeMkv {
    let Ok(mut child) = crate::process::command("makemkvcon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    else {
        return MakeMkv::NotFound;
    };
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            // Still running: it started, which is all we want to know.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return MakeMkv::Works;
            }
        }
    }
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut err);
    }
    match err
        .lines()
        .find(|l| l.contains("error while loading shared libraries"))
    {
        Some(line) => MakeMkv::Broken(line.trim().to_owned()),
        None => MakeMkv::Works,
    }
}

/// LibreDrive (UHD-friendly) firmware known for a drive model on one
/// hardware platform, from the MakeMKV forum's flashing guide: (model,
/// platform, firmware to flash, its file). The platform matters: the same
/// model name exists on different chips (the WH14NS40 on MT1939 and MT1959),
/// and firmware for the wrong one can break the drive.
const UHD_FIRMWARE: &[(&str, &str, &str, &str)] = &[(
    "WH14NS40",
    "MT1959",
    "WH16NS60 1.02MK",
    "HL-DT-ST-BD-RE_WH16NS60-1.02-NM00100-211810291936.bin",
)];

/// Where the guide has the user put sdf.bin; the system check reads the
/// drive's LibreDrive status with it. A plain folder, since the MakeMKV snap
/// can't read hidden ones.
fn sdf_bin() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("uhd-flash").join("sdf.bin"))
}

/// A drive's LibreDrive information, from `makemkvcon f --info`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct LibreDrive {
    /// "Enabled", "Possible, not yet enabled", "Not possible", …
    pub status: String,
    /// "MT1939"
    pub platform: String,
    /// "Original (unpatched)", …
    pub firmware_type: String,
}

impl LibreDrive {
    fn enabled(&self) -> bool {
        self.status.to_lowercase().starts_with("enabled")
    }
    fn possible(&self) -> bool {
        self.status.to_lowercase().starts_with("possible")
    }
}

/// Parse `makemkvcon f --info` output: each field is a "<code>:<name>" line
/// followed by a "<code>:<value>" line.
///
/// The output can hold two blocks: "[Drive Specific SDF]", what MakeMKV
/// actually achieves with this drive (it can switch LibreDrive on at run
/// time, even on unpatched firmware), and "[Identification SDF]", a generic
/// guess from the model that may say "Possible, not yet enabled" for the
/// same drive. The drive-specific block wins when present.
fn parse_libredrive(out: &str) -> Option<LibreDrive> {
    let all: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let specific = all
        .iter()
        .position(|l| l.starts_with("[Drive Specific SDF] Embedded Info Strings"));
    let lines: Vec<&str> = match specific {
        Some(start) => all[start + 1..]
            .iter()
            .take_while(|l| !l.starts_with('['))
            .copied()
            .collect(),
        None => all,
    };
    let field = |name: &str| -> String {
        lines
            .iter()
            .position(|l| l.split_once(':').is_some_and(|(_, n)| n == name))
            .and_then(|i| lines.get(i + 1))
            .and_then(|v| v.split_once(':'))
            .map(|(_, v)| v.trim().to_owned())
            .unwrap_or_default()
    };
    let info = LibreDrive {
        status: field("Status"),
        platform: field("Drive platform"),
        firmware_type: field("Firmware type"),
    };
    (!info.status.is_empty()).then_some(info)
}

/// Ask MakeMKV's firmware tool (read-only; it needs no registration) for
/// `dev`'s LibreDrive status. Needs sdf.bin in ~/uhd-flash.
fn libredrive_info(makemkv: &MakeMkv, snap: bool, dev: &str) -> Option<LibreDrive> {
    let sdf = sdf_bin().filter(|p| p.exists())?;
    let tool = if *makemkv == MakeMkv::Works {
        "makemkvcon"
    } else if snap {
        "/snap/bin/makemkvcon"
    } else {
        return None;
    };
    let mut child = crate::process::command(tool)
        .args(["f", "-d", dev, "-f"])
        .arg(&sdf)
        .arg("--info")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    while child.try_wait().ok()?.is_none() {
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut out = String::new();
    use std::io::Read;
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_libredrive(&out)
}

const UHD_GUIDE: &str = "forum.makemkv.com: \"Ultimate UHD Drives Flashing Guide\" (topic 19634) \
and the firmware pack (topic 19113)";

/// Step-by-step instructions for making drive `d` read Ultra HD Blu-rays,
/// as (steps, commands to copy). `snap`: MakeMKV is installed as a snap,
/// which can do the job when the source build is missing or broken.
fn uhd_steps(
    d: &DriveReport,
    makemkv: &MakeMkv,
    snap: bool,
    info: Option<&LibreDrive>,
) -> (String, String) {
    let dev = &d.device;
    // Name a firmware file only once the check has shown the platform.
    let known = info.and_then(|i| {
        UHD_FIRMWARE.iter().find(|(model, platform, _, _)| {
            d.model.to_uppercase().contains(model) && i.platform.eq_ignore_ascii_case(platform)
        })
    });
    let use_snap = snap && *makemkv != MakeMkv::Works;
    let tool = if use_snap {
        "/snap/bin/makemkvcon"
    } else {
        "makemkvcon"
    };

    // Already LibreDrive-capable: nothing to flash.
    if let Some(i) = info.filter(|i| i.enabled()) {
        let steps = format!(
            "MakeMKV reports LibreDrive: Enabled ({}, firmware {}). This drive can already read \
             UHD discs, without any firmware change: don't flash it. With unpatched firmware, \
             LibreDrive works only while MakeMKV itself reads the disc, so play or rip UHD discs \
             through MakeMKV:\n\
             1. In this player: File → Play disc → Rip & play with MakeMKV. It copies the main movie \
             to your Videos folder with a progress bar, plays it, then deletes the copy.\n\
             2. Or play directly: Settings → Blu-ray decryption → MakeMKV, then restart this app \
             (needs MakeMKV installed from source, not the snap).\n\
             MakeMKV reads discs only while registered: its free beta key is posted publicly in \
             \"MakeMKV is free while in beta\" on forum.makemkv.com (no account or payment); \
             paste it in Tools → MakeMKV (key & help), which also walks you through installing MakeMKV.",
            if i.platform.is_empty() {
                "unknown platform"
            } else {
                &i.platform
            },
            if i.firmware_type.is_empty() {
                "unknown"
            } else {
                &i.firmware_type
            },
        );
        return (steps, String::new());
    }
    if info.is_some_and(|i| !i.possible()) {
        let i = info.expect("checked");
        return (
            format!(
                "MakeMKV reports LibreDrive: {} ({}). This drive can't read UHD discs, and no \
                 firmware changes that: use a drive from the UHD-friendly list on forum.makemkv.com \
                 (external USB drives work too).",
                i.status, i.platform
            ),
            String::new(),
        );
    }

    let mut steps = Vec::new();
    if use_snap {
        steps.push(
            "Get the MakeMKV snap ready (it can check and flash the drive; this app doesn't need \
             MakeMKV afterwards). Open it once with /snap/bin/makemkv: if it says the version is too \
             old, register it (Help → Register) with the free beta key posted in \"MakeMKV is free \
             while in beta\" on forum.makemkv.com, or update it (sudo snap refresh makemkv). Then \
             allow it to write to the drive (first command below)."
                .to_owned(),
        );
    } else {
        match makemkv {
            MakeMkv::Works => {}
            MakeMkv::Broken(_) => steps.push(
                "Fix MakeMKV first: its makemkvcon doesn't start (see the MakeMKV item). \
                 It is the tool that checks and flashes the drive."
                    .to_owned(),
            ),
            MakeMkv::NotFound => steps.push(
                "Install MakeMKV (from makemkv.com, or: sudo snap install makemkv): its makemkvcon \
                 checks and flashes the drive."
                    .to_owned(),
            ),
        }
    }
    steps.push(
        "Download sdf.bin (the firmware commands need it) into a folder of its own, ~/uhd-flash, and \
         run the rest of the commands from there (see the commands below)."
            .to_owned(),
    );
    steps.push(
        "Check the drive (safe, read-only). Look for LibreDrive in the output (or in the MakeMKV \
         window's Info panel, with a disc in): \"Enabled\" means it already works and the disc \
         itself is the problem; \"Possible\" means a firmware update will fix it; \"Not possible\" \
         means this drive can't read UHD discs: use a drive from the UHD-friendly list instead."
            .to_owned(),
    );
    steps
        .push("Back up the drive's current firmware, and keep the file somewhere safe.".to_owned());
    match known {
        Some((_, platform, name, file)) => steps.push(format!(
            "Download the {name} firmware ({file}) into the same folder. {UHD_GUIDE} currently names \
             it for your drive on the {platform} platform the check reported: check there first, as \
             the advice changes."
        )),
        None => steps.push(format!(
            "Find your drive in {UHD_GUIDE} and download the firmware it names for exactly your \
             drive model AND the \"Drive platform\" the check reports (one model name can exist on \
             different chips; firmware for the wrong one breaks the drive), into the same folder."
        )),
    }
    steps.push(
        "Flash it: in the commands, remove the # in front of the flash line and run only that line. \
         Eject any disc, close every program using the drive, use stable power, and don't interrupt \
         it even if it seems stuck. Use \"main\" for stock firmware that isn't encrypted, \"enc\" when \
         the check reports encrypted firmware (\"Possible (with patched firmware)\"). Flashing voids \
         the warranty, and a wrong file or an interrupted flash can break the drive for good."
            .to_owned(),
    );
    steps.push(
        "Shut the computer down completely, start it again, and run the check again: LibreDrive \
         should now say \"Enabled\". Then use Play disc → Blu-ray."
            .to_owned(),
    );
    let text = steps
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {s}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");

    let file = known.map_or("<firmware file>.bin", |k| k.3);
    let mut commands = String::new();
    if use_snap {
        commands += "# Let the MakeMKV snap write to the drive (needed to flash):\n\
                     sudo snap connect makemkv:optical-write\n";
    }
    commands += "# Get sdf.bin into a working folder (every command below runs from there):\n\
                 mkdir -p ~/uhd-flash && cd ~/uhd-flash && wget -N https://www.makemkv.com/sdf.bin\n";
    commands += &format!(
        "# Check (read-only):\n\
         {tool} f -d {dev} -f sdf.bin --info\n\
         # Back up the current firmware:\n\
         {tool} f -d {dev} -f sdf.bin dump auto\n\
         # Flash: switched off with # so pasting this block can't run it. After the backup and\n\
         # reading the guide, remove the # and run only this line (\"enc\" for encrypted stock firmware):\n\
         # {tool} f -d {dev} -f sdf.bin rawflash main -i {file}"
    );
    (text, commands)
}

/// The findings, worst first.
pub fn findings(f: &Facts) -> Vec<Finding> {
    use Level::*;
    let mut out = Vec::new();

    if !f.engine.is_empty() {
        out.push(match f.ffmpeg_updates {
            Some(n) if n > 0 => Finding::new(
                Warning,
                "FFmpeg updates are waiting",
                format!(
                    "{} is running; {n} newer FFmpeg/libmpv package(s) are available from your system \
                     (security and format fixes).",
                    f.engine
                ),
            )
            .action(Action::UpdateFfmpeg),
            _ => Finding::new(Ok, "Playback engine", f.engine.clone()),
        });
    }

    // ---- online video ----
    out.push(match &f.ytdlp {
        Some((p, Some(v))) => {
            Finding::new(Ok, "yt-dlp (YouTube and other sites)", format!("Version {v} · {}", p.display()))
        }
        Some((p, None)) => Finding::new(
            Warning,
            "yt-dlp doesn't run",
            format!("{} is there but fails to start, so YouTube and similar links won't play.", p.display()),
        )
        .action(Action::InstallYtdlp),
        None => Finding::new(
            Warning,
            "yt-dlp is not installed",
            "YouTube and ~1800 other sites need it. The app can download it (about 35 MB, checked against its published checksums).",
        )
        .action(Action::InstallYtdlp),
    });

    // ---- DVD ----
    out.push(if f.dvdcss {
        Finding::new(
            Ok,
            "DVD decryption (libdvdcss)",
            "Installed: copy-protected DVDs can play.",
        )
    } else {
        Finding::new(
            Warning,
            "DVD decryption (libdvdcss) is missing",
            "Most store-bought DVDs are scrambled (CSS) and won't play without it.",
        )
        .command(DVDCSS_FIX)
    });

    // ---- Blu-ray ----
    let mode = f.settings.bluray_decryption.as_str();
    let makemkv_mode = mode == "makemkv";
    if !makemkv_mode {
        out.push(if f.aacs {
            Finding::new(Ok, "Blu-ray decryption (libaacs)", "Installed.")
        } else {
            Finding::new(
                if mode == "libaacs" { Problem } else { Warning },
                "Blu-ray decryption (libaacs) is missing",
                "Commercial Blu-rays are encrypted (AACS) and won't play without it, or MakeMKV.",
            )
            .command(AACS_FIX)
        });
        if f.aacs && !f.bdplus {
            out.push(
                Finding::new(
                    Info,
                    "BD+ support (libbdplus) is missing",
                    "A few Blu-rays add BD+ protection on top of AACS; those won't play.",
                )
                .command(AACS_FIX),
            );
        }
        out.push(match &f.keydb {
            Some((p, size, age)) if *age > 90 => Finding::new(
                Info,
                "Blu-ray key database is getting old",
                format!(
                    "{} is {age} days old ({} MB). Discs released since then won't be in it.",
                    p.display(),
                    size >> 20
                ),
            )
            .action(Action::DownloadKeydb),
            Some((p, size, age)) => Finding::new(
                Ok,
                "Blu-ray key database",
                format!("{} ({} MB, {age} days old). Kept up to date automatically when auto-update is on.", p.display(), size >> 20),
            ),
            None => Finding::new(
                Warning,
                "No Blu-ray key database",
                "libaacs looks up each disc's key in KEYDB.cfg. Download it once; it works offline afterwards.",
            )
            .action(Action::DownloadKeydb),
        });
    }

    // ---- MakeMKV ----
    let mkv_level = |worst: Level| if makemkv_mode { worst } else { Info };
    match &f.makemkv {
        MakeMkv::Broken(why) => out.push(
            Finding::new(
                mkv_level(Problem),
                "MakeMKV is installed but broken",
                format!(
                    "makemkvcon won't start ({why}). It was built for an older system; \
                     install the current version from source to fix it."
                ),
            )
            .command(MAKEMKV_FIX),
        ),
        MakeMkv::Works if f.mmbd => out.push(Finding::new(
            Ok,
            "MakeMKV",
            if makemkv_mode {
                "Installed and used for Blu-ray decryption."
            } else {
                "Installed. Settings → Blu-ray decryption → MakeMKV uses it instead of libaacs."
            },
        )),
        MakeMkv::Works => out.push(
            Finding::new(
                mkv_level(Problem),
                "MakeMKV's library (libmmbd) is missing",
                "makemkvcon runs, but the library other players use isn't installed. Install MakeMKV from source.",
            )
            .command(MAKEMKV_FIX),
        ),
        MakeMkv::NotFound if makemkv_mode => out.push(
            Finding::new(
                Problem,
                "MakeMKV is selected but not installed",
                "Settings → Blu-ray decryption is set to MakeMKV. Install it, or switch back to Automatic.",
            )
            .action(Action::OpenSettings)
            .command(MAKEMKV_FIX),
        ),
        MakeMkv::NotFound => {}
    }
    if f.makemkv_flatpak {
        out.push(Finding::new(
            Ok,
            "MakeMKV (Flatpak)",
            "Installed: File → Play disc → Rip & play with MakeMKV uses it.",
        ));
    }
    if f.makemkv_snap && !f.makemkv_flatpak {
        out.push(
            Finding::new(
                Info,
                "MakeMKV snap found",
                "The snap is a volunteer package and often several versions behind, and an outdated \
                 MakeMKV rejects the current beta key (\"key not found or invalid\", \"version too old\"). \
                 For Rip & play, install the current MakeMKV from Flathub; the snap is still fine for \
                 checking the drive's LibreDrive status.",
            )
            .command(crate::ripper::FLATPAK_INSTALL),
        );
    }

    // ---- drives ----
    if f.drives.is_empty() && cfg!(target_os = "linux") {
        out.push(Finding::new(
            Info,
            "No disc drive found",
            "Discs need an optical drive (USB ones work too).",
        ));
    }
    for d in &f.drives {
        let name = match &d.firmware {
            Some(fw) => format!("{} (firmware {fw})", d.label),
            None => d.label.clone(),
        };
        out.push(match &d.state {
            DriveState::Refuses if d.blu_ray => {
                let (steps, commands) = uhd_steps(d, &f.makemkv, f.makemkv_snap, f.libredrive.iter().find(|(dev, _)| *dev == d.device).map(|(_, i)| i));
                Finding::new(
                    Problem,
                    "The drive won't read the disc in it",
                    format!(
                        "{name} reports a disc but refuses to read it. This is what an Ultra HD (4K) \
                         Blu-ray does in a drive that isn't UHD-friendly (a dirty or damaged disc can \
                         too). What to do:\n\n{steps}"
                    ),
                )
                .command(commands)
            }
            DriveState::Refuses => Finding::new(
                Problem,
                "The drive won't read the disc in it",
                format!(
                    "{name} reports a disc but refuses to read it. The disc may be dirty, damaged, \
                     or a format this drive can't read."
                ),
            ),
            DriveState::Disc { uhd: true, .. } => {
                let (steps, commands) = uhd_steps(d, &f.makemkv, f.makemkv_snap, f.libredrive.iter().find(|(dev, _)| *dev == d.device).map(|(_, i)| i));
                Finding::new(
                    Info,
                    "Ultra HD Blu-ray in the drive",
                    format!(
                        "{name}: UHD discs need a UHD-friendly (LibreDrive) drive as well as the \
                         disc's key. If it won't play:\n\n{steps}"
                    ),
                )
                .command(commands)
            }
            DriveState::Disc { kind, .. } => Finding::new(
                Ok,
                "Disc drive",
                format!(
                    "{name}: {} ready.",
                    kind.map_or("disc".to_owned(), DiscKind::with_article)
                ),
            ),
            DriveState::TrayOpen => Finding::new(Ok, "Disc drive", format!("{name}: tray open.")),
            DriveState::NotReady => Finding::new(Ok, "Disc drive", format!("{name}: spinning up.")),
            DriveState::Empty | DriveState::Unknown => {
                Finding::new(Ok, "Disc drive", format!("{name}: no disc."))
            }
        });
    }

    // ---- desktop ----
    if f.wayland {
        out.push(
            Finding::new(
                Info,
                "Drag and drop is off on Wayland",
                "Paste files or links with Ctrl+V instead, or switch Settings → Window system to X11 \
                 (can look soft with fractional scaling).",
            )
            .action(Action::OpenSettings),
        );
    }
    if f.elevated {
        out.push(Finding::new(
            Warning,
            "Running as administrator",
            ELEVATED_HINT,
        ));
    }
    if !f.settings.auto_update {
        out.push(
            Finding::new(
                Info,
                "Automatic updates are off",
                "yt-dlp and the Blu-ray key database won't be refreshed; sites and new discs may stop working.",
            )
            .action(Action::OpenSettings),
        );
    }

    // Worst first; the stable sort keeps the order above within a level.
    out.sort_by_key(|f| std::cmp::Reverse(f.level));
    out
}

/// The findings as plain text, for "Copy report".
pub fn report(items: &[Finding]) -> String {
    let mut out = format!(
        "Super Media Player {} system check\n",
        env!("CARGO_PKG_VERSION")
    );
    for f in items {
        let tag = match f.level {
            Level::Problem => "PROBLEM",
            Level::Warning => "WARNING",
            Level::Info => "NOTE",
            Level::Ok => "OK",
        };
        out += &format!("\n[{tag}] {}\n{}\n", f.title, f.detail);
        if !f.command.is_empty() {
            out += &format!("Fix:\n{}\n", f.command);
        }
    }
    out
}

/// "2 problems, 1 warning" (empty when all is well).
pub fn summary(items: &[Finding]) -> String {
    let count = |l| items.iter().filter(|f| f.level == l).count();
    let mut parts = Vec::new();
    for (n, one, many) in [
        (count(Level::Problem), "problem", "problems"),
        (count(Level::Warning), "warning", "warnings"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {}", if n == 1 { one } else { many }));
        }
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Facts {
        Facts {
            engine: "mpv 0.37.0 · FFmpeg 6.1.1".into(),
            ytdlp: Some(("/usr/bin/yt-dlp".into(), Some("2026.11.03".into()))),
            dvdcss: true,
            aacs: true,
            bdplus: true,
            mmbd: false,
            makemkv: MakeMkv::NotFound,
            makemkv_snap: false,
            makemkv_flatpak: false,
            keydb: Some(("/home/u/.config/aacs/KEYDB.cfg".into(), 30 << 20, 5)),
            settings: Settings::default(),
            wayland: false,
            elevated: false,
            drives: vec![drive(DriveState::Empty)],
            libredrive: Vec::new(),
            ffmpeg_updates: Some(0),
        }
    }

    fn drive(state: DriveState) -> DriveReport {
        DriveReport {
            device: "/dev/sr1".into(),
            model: "HL-DT-ST BD-RE WH14NS40".into(),
            label: "/dev/sr1, HL-DT-ST BD-RE WH14NS40".into(),
            firmware: Some("1.00".into()),
            blu_ray: true,
            state,
        }
    }

    #[test]
    fn healthy_system_has_no_warnings() {
        let items = findings(&healthy());
        assert!(items.iter().all(|f| f.level == Level::Ok), "{items:#?}");
        assert_eq!(summary(&items), "");
    }

    #[test]
    fn missing_pieces_come_with_fixes() {
        let mut f = healthy();
        f.ytdlp = None;
        f.dvdcss = false;
        f.keydb = None;
        let items = findings(&f);
        let get = |t: &str| items.iter().find(|i| i.title.contains(t)).unwrap();
        assert_eq!(get("yt-dlp").action, Action::InstallYtdlp);
        assert!(get("libdvdcss").command.contains("libdvd-pkg"));
        assert_eq!(get("key database").action, Action::DownloadKeydb);
        assert_eq!(summary(&items), "3 warnings");
        let text = report(&items);
        assert!(text.contains("[WARNING] yt-dlp is not installed"));
        assert!(text.contains("Fix:\nsudo apt install libdvd-pkg"));
    }

    #[test]
    fn refusing_drive_is_a_problem_and_comes_first() {
        let mut f = healthy();
        f.makemkv = MakeMkv::Works;
        f.mmbd = true;
        f.drives = vec![drive(DriveState::Refuses)];
        let items = findings(&f);
        assert_eq!(items[0].level, Level::Problem);
        assert!(items[0].detail.contains("Ultra HD"), "{}", items[0].detail);
        assert!(items[0].detail.contains("firmware 1.00"));
        // Step-by-step, for this drive and device.
        assert!(
            items[0].detail.contains("2. Check the drive"),
            "{}",
            items[0].detail
        );
        assert!(
            items[0]
                .command
                .contains("makemkvcon f -d /dev/sr1 -f sdf.bin --info")
        );
        // No firmware file is named before the check has shown the platform.
        assert!(!items[0].detail.contains("WH16NS60"), "{}", items[0].detail);
        assert!(items[0].command.contains("<firmware file>.bin"));
    }

    #[test]
    fn uhd_steps_start_with_fixing_makemkv() {
        let mut d = drive(DriveState::Refuses);
        let (steps, cmds) = uhd_steps(&d, &MakeMkv::Broken("x".into()), false, None);
        assert!(steps.starts_with("1. Fix MakeMKV first"), "{steps}");
        assert!(
            cmds.contains("makemkvcon f -d /dev/sr1 -f sdf.bin --info"),
            "{cmds}"
        );
        // An unknown drive gets the generic firmware step and a placeholder.
        d.model = "SOME OTHER DRIVE".into();
        let (steps, cmds) = uhd_steps(&d, &MakeMkv::Works, false, None);
        assert!(steps.contains("Find your drive"), "{steps}");
        assert!(cmds.contains("<firmware file>.bin"));
    }

    #[test]
    fn broken_makemkv_with_snap_uses_the_snap() {
        let d = drive(DriveState::Refuses);
        let (steps, cmds) = uhd_steps(&d, &MakeMkv::Broken("x".into()), true, None);
        assert!(
            steps.starts_with("1. Get the MakeMKV snap ready"),
            "{steps}"
        );
        assert!(cmds.starts_with("# Let the MakeMKV snap write"), "{cmds}");
        assert!(cmds.contains("sudo snap connect makemkv:optical-write"));
        assert!(cmds.contains("cd ~/uhd-flash && wget -N https://www.makemkv.com/sdf.bin"));
        assert!(cmds.contains("/snap/bin/makemkvcon f -d /dev/sr1 -f sdf.bin --info"));
    }

    /// Pasting the whole command block into a terminal must never flash:
    /// every line that flashes is commented out.
    #[test]
    fn pasting_the_commands_never_flashes() {
        for (mk, snap) in [(MakeMkv::Works, false), (MakeMkv::Broken("x".into()), true)] {
            let (_, cmds) = uhd_steps(&drive(DriveState::Refuses), &mk, snap, None);
            assert!(cmds.contains("rawflash"));
            for line in cmds.lines().filter(|l| l.contains("rawflash")) {
                assert!(
                    line.trim_start().starts_with('#'),
                    "runnable flash line: {line}"
                );
            }
        }
    }

    #[test]
    fn broken_makemkv_matters_most_when_selected() {
        let mut f = healthy();
        f.makemkv = MakeMkv::Broken("libavcodec.so.58: cannot open".into());
        let info = findings(&f);
        let item = info.iter().find(|i| i.title.contains("broken")).unwrap();
        assert_eq!(item.level, Level::Info);
        assert!(item.command.contains("makemkv-oss"));

        f.settings.bluray_decryption = "makemkv".into();
        let items = findings(&f);
        assert_eq!(items[0].title, "MakeMKV is installed but broken");
        assert_eq!(items[0].level, Level::Problem);
        // libaacs and the key database don't matter in MakeMKV mode.
        assert!(!items.iter().any(|i| i.title.contains("key database")));
    }

    #[test]
    fn waiting_ffmpeg_updates_are_offered() {
        let mut f = healthy();
        f.ffmpeg_updates = Some(3);
        let items = findings(&f);
        let item = items
            .iter()
            .find(|i| i.title.contains("FFmpeg updates"))
            .unwrap();
        assert_eq!(item.action, Action::UpdateFfmpeg);
        assert_eq!(item.level, Level::Warning);
    }

    #[test]
    fn old_key_database_suggests_a_refresh() {
        let mut f = healthy();
        f.keydb = Some(("/k".into(), 1 << 20, 200));
        let items = findings(&f);
        let item = items
            .iter()
            .find(|i| i.title.contains("getting old"))
            .unwrap();
        assert_eq!(item.action, Action::DownloadKeydb);
    }

    #[test]
    fn makemkv_detection_runs() {
        // Whatever is installed, this must answer quickly and not panic.
        let _ = makemkv_state();
    }

    /// Print this computer's report: `cargo test -p super-media-player
    /// this_machine -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads this machine's drives and libraries"]
    fn this_machine() {
        let facts = gather(&Settings::load(), String::new());
        for f in findings(&facts) {
            println!(
                "[{:?}] {}\n    {}\n    {}",
                f.level,
                f.title,
                f.detail,
                f.command.replace('\n', "\n    $ ")
            );
        }
    }

    /// What `makemkvcon f --info` printed for the user's drive (2026-11-22).
    const MT1939_ENABLED: &str = "SDF.bin version: 0x00A6\n\nDrive Tool SDF present\n\n\
        [Drive Specific SDF] Embedded Info Strings:\n8000:LibreDrive Information\n\n8013:Status\n\
        8105:Enabled\n\n8001:Drive platform\n:MT1939\n\n8002:Firmware type\n8106:Original (unpatched)\n\n\
        8003:Firmware version\n:1.00\n";

    /// The whole reply, with the generic identification block after the
    /// drive-specific one (the user's drive, 2026-11-22).
    const MT1939_FULL: &str = "SDF.bin version: 0x00A6\nDrive Tool SDF present\nDrive Specific SDF present\n\
        [Drive Specific SDF] Embedded Info Strings:\n8000:LibreDrive Information\n8013:Status\n8105:Enabled\n\
        8001:Drive platform\n:MT1939\n8002:Firmware type\n8106:Original (unpatched)\n\
        8003:Firmware version\n:1.00\n8007:BD raw metadata read\n8110:Partial\n\
        [Drive Specific SDF] Embedded Status Message:\n8099:Using LibreDrive mode (v%1 id=%2)\n:06.3\n\
        Identification SDF present\n[Identification SDF] Embedded Info Strings:\n8000:LibreDrive Information\n\
        8013:Status\n8102:Possible, not yet enabled\n8001:Drive platform\n:MT1939\n";

    #[test]
    fn drive_specific_status_wins_over_identification() {
        let i = parse_libredrive(MT1939_FULL).unwrap();
        assert_eq!(i.status, "Enabled");
        assert_eq!(i.firmware_type, "Original (unpatched)");
        // Without the drive-specific block, the generic one is all there is.
        let generic = MT1939_FULL
            .split("Identification SDF present")
            .nth(1)
            .unwrap();
        assert_eq!(
            parse_libredrive(generic).unwrap().status,
            "Possible, not yet enabled"
        );
    }

    #[test]
    fn parses_libredrive_info() {
        let i = parse_libredrive(MT1939_ENABLED).unwrap();
        assert_eq!(i.status, "Enabled");
        assert_eq!(i.platform, "MT1939");
        assert_eq!(i.firmware_type, "Original (unpatched)");
        assert!(i.enabled() && !i.possible());
        assert!(parse_libredrive("Unable to load sdf.bin\nInternal error 0xffffffff").is_none());
    }

    #[test]
    fn enabled_drive_is_told_not_to_flash() {
        let mut f = healthy();
        f.drives = vec![drive(DriveState::Refuses)];
        f.libredrive = vec![("/dev/sr1".into(), parse_libredrive(MT1939_ENABLED).unwrap())];
        let item = &findings(&f)[0];
        assert!(item.detail.contains("don't flash it"), "{}", item.detail);
        assert!(item.detail.contains("MT1939"));
        assert!(!item.command.contains("rawflash"), "{}", item.command);
    }

    #[test]
    fn firmware_is_named_only_for_its_platform() {
        let d = drive(DriveState::Refuses);
        let possible = |platform: &str| LibreDrive {
            status: "Possible, not yet enabled".into(),
            platform: platform.into(),
            firmware_type: "Original (unpatched)".into(),
        };
        let (steps, cmds) = uhd_steps(&d, &MakeMkv::Works, false, Some(&possible("MT1959")));
        assert!(steps.contains("WH16NS60 1.02MK"), "{steps}");
        assert!(cmds.contains("WH16NS60-1.02-NM00100"));
        // The same model on the older chip must not get MT1959 firmware.
        let (steps, cmds) = uhd_steps(&d, &MakeMkv::Works, false, Some(&possible("MT1939")));
        assert!(!steps.contains("WH16NS60"), "{steps}");
        assert!(cmds.contains("<firmware file>.bin"));
    }
}
