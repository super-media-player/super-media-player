//! Optical discs and disc folders/images: finding a drive that holds a disc,
//! working out what the disc really is (DVD, Blu-ray or audio CD) from its
//! contents, and turning that into what mpv has to load.
//!
//! The menu item the user picked is only a preference: a DVD in a Blu-ray
//! drive, opened with "Blu-ray", still plays as a DVD.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use player_core::{LoadMode, Player};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiscKind {
    Dvd,
    Bluray,
    AudioCd,
}

impl DiscKind {
    pub fn name(self) -> &'static str {
        match self {
            DiscKind::Dvd => "DVD",
            DiscKind::Bluray => "Blu-ray",
            DiscKind::AudioCd => "audio CD",
        }
    }

    /// "a DVD", "an audio CD"
    pub fn with_article(self) -> String {
        let article = if self == DiscKind::AudioCd { "an" } else { "a" };
        format!("{article} {}", self.name())
    }

    /// The entry under Play disc that opens this kind.
    fn menu_name(self) -> &'static str {
        match self {
            DiscKind::AudioCd => "Audio CD",
            k => k.name(),
        }
    }

    fn scheme_and_option(self) -> (&'static str, &'static str) {
        match self {
            DiscKind::Dvd => ("dvd://", "dvd-device"),
            DiscKind::Bluray => ("bd://", "bluray-device"),
            DiscKind::AudioCd => ("cdda://", "cdda-device"),
        }
    }
}

/// What to hand to mpv for a disc: the device option and the URL to load.
#[derive(Clone, Debug, PartialEq)]
pub struct DiscPlan {
    pub kind: DiscKind,
    /// Drive (/dev/sr1), image file or disc folder.
    pub device: String,
    pub url: String,
    /// Shown to the user, e.g. "This disc is a DVD: playing it as a DVD".
    pub note: Option<String>,
    /// Missing decryption support that will probably stop this disc from playing.
    pub warning: Option<String>,
    /// The drive in plain words ("DVD drive (TSST SH-216DB, /dev/sr0)"),
    /// when it is a drive.
    pub drive: String,
    /// An audio CD's songs and names.
    pub cd: Option<CdDisc>,
}

/// What is known about an audio CD in a drive.
#[derive(Clone, Debug, PartialEq)]
pub struct CdDisc {
    /// (track number, start LBA, is data track), then the lead-out.
    pub toc: Vec<(u8, u32, bool)>,
    pub tracks: Vec<CdTrack>,
    /// Song names, once looked up ([`name_cd`]).
    pub album: Option<crate::cd::Album>,
    /// The album cover (a cached JPEG).
    pub cover: Option<PathBuf>,
}

impl DiscPlan {
    fn new(kind: DiscKind, device: String) -> Self {
        Self {
            kind,
            url: kind.scheme_and_option().0.to_owned(),
            device,
            note: None,
            warning: decryption_warning(kind),
            drive: String::new(),
            cd: None,
        }
    }

    /// A DVD's main film (the longest title).
    pub fn dvd(device: String) -> Self {
        Self::new(DiscKind::Dvd, device)
    }

    /// An Ultra HD Blu-ray needs more than a plain one (see [`UHD_HELP`]), so
    /// its failure gets that explanation instead of the generic one.
    fn check_uhd(&mut self, root: &Path) {
        if self.kind == DiscKind::Bluray && is_uhd(root) {
            log::info!("{}: Ultra HD Blu-ray (AACS 2.0)", self.device);
            self.warning = Some(UHD_HELP.into());
        }
    }

    /// Point mpv at the device, then load the disc. Always sets the device
    /// option, so a path left over from an earlier disc is replaced.
    pub fn apply(&self, player: &Player) -> player_core::Result<()> {
        self.apply_with(player, &[])
    }

    /// [`apply`](Self::apply) with mpv options for this file only.
    pub fn apply_with(&self, player: &Player, options: &[(&str, &str)]) -> player_core::Result<()> {
        player.set_string(self.kind.scheme_and_option().1, &self.device)?;
        // Read ahead: mpv's default cache ("auto") is for network streams
        // only, and without it every slow read or seek of the spinning disc
        // starves the sound ("audio device underrun"). ~30 s of a Blu-ray.
        let _ = player.set_string("cache", "yes");
        let _ = player.set_string("demuxer-max-bytes", "192MiB");
        let cover = self.cd.as_ref().and_then(|c| c.cover.as_ref());
        let _ = player.command(&["change-list", "cover-art-files", "clr", ""]);
        if let Some(c) = cover {
            let _ = player.command(&["change-list", "cover-art-files", "append", &c.to_string_lossy()]);
        }
        if options.is_empty() {
            player.load(&self.url, LoadMode::Replace)
        } else {
            player.load_with(&self.url, options)
        }
    }
}

/// Undo [`DiscPlan::apply`]'s read-ahead for ordinary files and streams.
pub fn reset_cache(player: &Player) {
    let _ = player.set_string("cache", "auto");
    let _ = player.set_string("demuxer-max-bytes", "150MiB");
    // Setting it to "" would make a list with one nameless file in it.
    let _ = player.command(&["change-list", "cover-art-files", "clr", ""]);
}

/// Look up the names of an audio CD's songs (and its cover), and name the
/// playlist entries after them. Blocks for up to a few seconds (the
/// MusicBrainz lookup; cached afterwards); run it off the UI thread.
pub fn name_cd(plan: &mut DiscPlan, online: bool) {
    let Some(cd) = plan.cd.as_mut() else { return };
    #[cfg(target_os = "linux")]
    let cd_text = || toc::cd_text(&plan.device).and_then(|p| crate::cd::parse_cd_text(&p));
    #[cfg(not(target_os = "linux"))]
    let cd_text = || None;
    cd.album = crate::cd::names(&cd.toc, cd_text, online);
    cd.cover = cd.album.as_ref().and_then(|a| crate::cd::cover(a, online));
    plan.url = cd_playlist(&cd.tracks, cd.album.as_ref());
}

/// Find the drives with a disc and plan how to play each (usually one; with
/// discs in several drives, the user picks). Blocking: waits for a
/// drive that is still spinning up and may mount the disc, so run it off the
/// UI thread. The error is a message for the user.
/// `report` hears what is happening, for the UI ("Checking the DVD drive
/// (…)…"): waiting for a drive can take half a minute.
pub fn plan_drive(
    preferred: Option<DiscKind>,
    report: impl Fn(String),
) -> Result<Vec<DiscPlan>, String> {
    let drives = list_drives().unwrap_or_default();
    if drives.is_empty() {
        // No drive list (non-Linux): let mpv try its default device.
        return Ok(vec![DiscPlan::new(
            preferred.unwrap_or(DiscKind::Dvd),
            String::new(),
        )]);
    }

    // Drives that can read the preferred kind of disc go first.
    let mut order: Vec<&(String, Vec<String>)> = drives.iter().collect();
    if let Some(kind) = preferred {
        order.sort_by_key(|(_, props)| !can_read(kind, props));
    }

    let mut tray_open = Vec::new();
    let mut found = Vec::new();
    let mut refused = Vec::new();
    for (dev, props) in order {
        let label = drive_label(dev, props);
        if drive_status(dev) == DriveStatus::NotReady {
            report(format!("Waiting for the {label} to get ready (up to 25 s)…"));
        } else {
            report(format!("Checking the {label}…"));
        }
        match wait_until_ready(dev, Duration::from_secs(25)) {
            // "Unknown": the driver doesn't say, so let detection try.
            status @ (DriveStatus::DiscOk | DriveStatus::Unknown) => {
                report(format!("Looking at the disc in the {label}…"));
                if let Some(mut plan) = detect(dev, props) {
                    plan.drive = label.clone();
                    found.push((plan, props.as_slice()));
                } else if status == DriveStatus::DiscOk && refuses_disc(dev) {
                    log::warn!("{dev}: the drive reports a disc but won't read it");
                    refused.push(label.clone());
                }
            }
            DriveStatus::TrayOpen => tray_open.push(dev.as_str()),
            // Still "not ready" after waiting: the drive is chewing on a disc
            // it can't read (an Ultra HD Blu-ray in a drive that isn't
            // UHD-friendly does this too).
            DriveStatus::NotReady => {
                log::warn!("{dev}: never got ready to read the disc in it");
                refused.push(label.clone());
            }
            _ => {}
        }
    }
    // A refusing drive explains the failure better than "no disc", even
    // when another drive holds something else (an audio CD).
    choose(preferred, found, &tray_open, &drives).map_err(|e| {
        if refused.is_empty() {
            e
        } else {
            refused_message(&refused.join(", "))
        }
    })
}

/// The drive reports a disc, but reading it fails. An audio CD has no data
/// sectors to read, so its track list counts as readable.
fn refuses_disc(dev: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        !toc::readable(dev) && audio_tracks(&toc::read(dev).unwrap_or_default()).is_empty()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dev;
        false
    }
}

/// Whether a [`plan_drive`] error means a drive refused its disc (an Ultra
/// HD Blu-ray in a drive that only MakeMKV can read it with).
pub fn is_refused_error(msg: &str) -> bool {
    msg.contains("sees a disc but refuses to read it")
}

fn refused_message(drives: &str) -> String {
    format!(
        "The {drives} sees a disc but refuses to read it. Ultra HD (4K) Blu-rays do this in drives \
         that aren't UHD-friendly; a scratched or unsupported disc can too. Tools → System check \
         explains what to do."
    )
}

/// What a drive and the disc in it look like, for the system check.
#[derive(Clone, Debug, PartialEq)]
pub struct DriveReport {
    /// "/dev/sr1"
    pub device: String,
    /// "HL-DT-ST BD-RE WH14NS40" (empty when udev doesn't say)
    pub model: String,
    /// "Blu-ray drive (LG WH14NS40, /dev/sr1)"
    pub label: String,
    /// Firmware revision, when udev knows it.
    pub firmware: Option<String>,
    pub blu_ray: bool,
    pub state: DriveState,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DriveState {
    Empty,
    TrayOpen,
    NotReady,
    /// Reports a disc, but reading it fails.
    Refuses,
    /// A readable disc; `kind` from udev, `uhd` when its files are mounted
    /// and say so.
    Disc { kind: Option<DiscKind>, uhd: bool },
    Unknown,
}

/// Every optical drive, without waiting for drives to spin up or mounting
/// anything (the system check must stay quick).
pub fn drive_reports() -> Vec<DriveReport> {
    list_drives()
        .unwrap_or_default()
        .into_iter()
        .map(|(dev, props)| {
            let has = |prefix: &str| props.iter().any(|p| p.starts_with(prefix));
            let state = match drive_status(&dev) {
                DriveStatus::NoDisc => DriveState::Empty,
                DriveStatus::TrayOpen => DriveState::TrayOpen,
                DriveStatus::NotReady => DriveState::NotReady,
                DriveStatus::Unknown => DriveState::Unknown,
                DriveStatus::DiscOk if refuses_disc(&dev) => DriveState::Refuses,
                DriveStatus::DiscOk => {
                    let kind = if has("ID_CDROM_MEDIA_BD") {
                        Some(DiscKind::Bluray)
                    } else if has("ID_CDROM_MEDIA_DVD") {
                        Some(DiscKind::Dvd)
                    } else if has("ID_CDROM_MEDIA_CD") {
                        Some(DiscKind::AudioCd)
                    } else {
                        None
                    };
                    let uhd = mount_point(&dev, false).is_some_and(|m| is_uhd(&m));
                    DriveState::Disc { kind, uhd }
                }
            };
            DriveReport {
                model: props
                    .iter()
                    .find_map(|p| p.strip_prefix("ID_MODEL="))
                    .map(|m| m.replace('_', " ").trim().to_owned())
                    .unwrap_or_default(),
                label: drive_label(&dev, &props),
                device: dev.clone(),
                firmware: props
                    .iter()
                    .find_map(|p| p.strip_prefix("ID_REVISION="))
                    .map(str::to_owned),
                blu_ray: can_read(DiscKind::Bluray, &props),
                state,
            }
        })
        .collect()
}

/// The found discs that fit the request, in drive order. With a preferred
/// kind, the discs of that kind; failing that, only discs sitting in a drive
/// that can read the preferred kind (a DVD in the Blu-ray drive, opened with
/// "Blu-ray"). A disc in some other drive (an audio CD in the DVD drive) is
/// not what the user asked for, so they are told instead of it starting by
/// surprise. More than one result: the user picks (main.rs).
fn choose(
    preferred: Option<DiscKind>,
    found: Vec<(DiscPlan, &[String])>,
    tray_open: &[&str],
    drives: &[(String, Vec<String>)],
) -> Result<Vec<DiscPlan>, String> {
    let fits: Vec<bool> = match preferred {
        None => vec![true; found.len()],
        Some(k) if found.iter().any(|(p, _)| p.kind == k) => {
            found.iter().map(|(p, _)| p.kind == k).collect()
        }
        Some(k) => found.iter().map(|(_, props)| can_read(k, props)).collect(),
    };
    if !fits.contains(&true) {
        let Some(want) = preferred else {
            return Err(if !tray_open.is_empty() {
                "The drive tray is open. Close it and try again.".into()
            } else {
                "There is no disc in the drive (or the drive can't read it).".into()
            });
        };
        let capable: Vec<&(String, Vec<String>)> =
            drives.iter().filter(|(_, p)| can_read(want, p)).collect();
        if capable.is_empty() {
            return Err(format!(
                "No drive in this computer can read {}.",
                want.with_article()
            ));
        }
        let names = capable
            .iter()
            .map(|(dev, p)| drive_label(dev, p))
            .collect::<Vec<_>>()
            .join(", ");
        let mut msg = if capable
            .iter()
            .any(|(dev, _)| tray_open.contains(&dev.as_str()))
        {
            format!("The tray of the {names} is open. Close it and try again.")
        } else {
            format!("No {} found: the {names} has no disc it can read.", want.name())
        };
        if let Some((other, _)) = found.first() {
            let place = if other.drive.is_empty() {
                other.device.clone()
            } else {
                format!("the {}", other.drive)
            };
            msg += &format!(
                " There is {} in {place}: to play it, use Play disc → {}.",
                other.kind.with_article(),
                other.kind.menu_name()
            );
        }
        return Err(msg);
    }
    Ok(found
        .into_iter()
        .zip(fits)
        .filter_map(|((mut plan, _), fits)| {
            if let Some(want) = preferred.filter(|&k| k != plan.kind) {
                plan.note = Some(format!(
                    "This disc is {}, not {}: playing it as {}",
                    plan.kind.with_article(),
                    want.with_article(),
                    plan.kind.with_article()
                ));
            }
            fits.then_some(plan)
        })
        .collect())
}

/// "/dev/sr1, HL-DT-ST BD-RE WH14NS40"
/// The drive in plain words, for people with more than one: "Blu-ray drive
/// (LG WH14NS40, /dev/sr1)", "DVD drive (TSST SH-216DB, /dev/sr0)".
fn drive_label(dev: &str, props: &[String]) -> String {
    let name = drive_name(props);
    match short_model(props) {
        Some(model) => format!("{name} ({model}, {dev})"),
        None => format!("{name} ({dev})"),
    }
}

/// "Blu-ray drive", "DVD drive" or "CD drive", by what it can read.
fn drive_name(props: &[String]) -> &'static str {
    if can_read(DiscKind::Bluray, props) {
        "Blu-ray drive"
    } else if can_read(DiscKind::Dvd, props) {
        "DVD drive"
    } else {
        "CD drive"
    }
}

/// The model without the drive-type words, with the maker's trading name:
/// "HL-DT-ST BD-RE  WH14NS40" → "LG WH14NS40".
fn short_model(props: &[String]) -> Option<String> {
    let raw = props.iter().find_map(|p| p.strip_prefix("ID_MODEL="))?;
    let words: Vec<String> = raw
        .replace('_', " ")
        .split_whitespace()
        .filter(|w| {
            !matches!(
                *w,
                "BD-RE" | "BD-ROM" | "DVD+-RW" | "DVD+" | "-RW" | "DVD-RW" | "DVD-RAM" | "DVD-ROM"
                    | "DVDRAM" | "CDDVDW" | "CD-RW"
            )
        })
        .map(|w| match w {
            "HL-DT-ST" => "LG".to_owned(),
            "TSSTcorp" => "TSST".to_owned(),
            "PIONEER" => "Pioneer".to_owned(),
            "ASUS" => "ASUS".to_owned(),
            w => w.to_owned(),
        })
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// What is on the disc in `dev`: audio tracks, a BDMV folder or a VIDEO_TS
/// folder. Falls back to udev's media type when the disc can't be mounted.
fn detect(dev: &str, props: &[String]) -> Option<DiscPlan> {
    #[cfg(target_os = "linux")]
    let toc = toc::read(dev).unwrap_or_default();
    #[cfg(not(target_os = "linux"))]
    let toc: Vec<(u8, u32, bool)> = Vec::new();
    let songs = audio_tracks(&toc);
    let cd = || {
        let mut plan = DiscPlan::new(DiscKind::AudioCd, dev.to_owned());
        plan.url = cd_playlist(&songs, None);
        plan.cd = Some(CdDisc { toc: toc.clone(), tracks: songs.clone(), album: None, cover: None });
        plan
    };
    // A plain audio CD has no filesystem to look at.
    if !songs.is_empty() && !toc.iter().any(|e| e.2) {
        return Some(cd());
    }
    if let Some(m) = mount_point(dev, true)
        && let Some(kind) = folder_kind(&m)
    {
        let mut plan = DiscPlan::new(kind, dev.to_owned());
        plan.check_uhd(&m);
        return Some(plan);
    }
    // Enhanced CD: songs plus a data track that isn't a video disc.
    if !songs.is_empty() {
        return Some(cd());
    }
    let has = |prefix: &str| props.iter().any(|p| p.starts_with(prefix));
    let kind = if has("ID_CDROM_MEDIA_BD") {
        DiscKind::Bluray
    } else if has("ID_CDROM_MEDIA_DVD") {
        DiscKind::Dvd
    } else {
        log::warn!("{dev}: could not tell what kind of disc this is");
        return None;
    };
    Some(DiscPlan::new(kind, dev.to_owned()))
}

fn can_read(kind: DiscKind, props: &[String]) -> bool {
    let flag = match kind {
        DiscKind::Bluray => "ID_CDROM_BD=1",
        DiscKind::Dvd => "ID_CDROM_DVD=1",
        DiscKind::AudioCd => "ID_CDROM_CD=1",
    };
    props.iter().any(|p| p == flag)
}

/// A folder that is (or contains) a DVD or Blu-ray structure.
fn folder_kind(dir: &Path) -> Option<DiscKind> {
    let names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_uppercase())
        .collect();
    if names.iter().any(|n| n == "BDMV") {
        Some(DiscKind::Bluray)
    } else if names.iter().any(|n| n == "VIDEO_TS") {
        Some(DiscKind::Dvd)
    } else {
        None
    }
}

fn upper_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_uppercase())
        .unwrap_or_default()
}

/// When `path` is a disc image, a disc folder, or a file inside one (a VOB in
/// VIDEO_TS, index.bdmv), how to play it as a disc. `None`: a normal file.
///
/// Playing VIDEO_TS/*.VOB files one by one loses the menus and title order,
/// and fails outright on copy-protected DVDs, whose VOBs are scrambled.
pub fn plan_path(path: &Path) -> Option<DiscPlan> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let parent = path.parent();
    let (kind, root): (DiscKind, PathBuf) = if path.is_dir() {
        match upper_name(path).as_str() {
            "VIDEO_TS" => (DiscKind::Dvd, parent?.to_owned()),
            "BDMV" => (DiscKind::Bluray, parent?.to_owned()),
            _ => (folder_kind(path)?, path.to_owned()),
        }
    } else if matches!(ext.as_str(), "iso" | "img") {
        (image_kind(path), path.to_owned())
    } else if matches!(ext.as_str(), "vob" | "ifo" | "bup")
        && parent.is_some_and(|p| upper_name(p) == "VIDEO_TS")
    {
        (DiscKind::Dvd, parent?.parent()?.to_owned())
    } else if ext == "bdmv" && parent.is_some_and(|p| upper_name(p) == "BDMV") {
        (DiscKind::Bluray, parent?.parent()?.to_owned())
    } else {
        return None;
    };
    // A folder on a mounted disc: read the drive itself, so the decryption
    // libraries can talk to it (DVD CSS keys and AACS need the drive).
    let device = device_of_mount(&root).unwrap_or_else(|| root.to_string_lossy().into_owned());
    let mut plan = DiscPlan::new(kind, device);
    if root.is_dir() {
        plan.check_uhd(&root);
    }
    Some(plan)
}

/// DVD or Blu-ray image? Looks for the BDMV / VIDEO_TS directory entries near
/// the start of the image (UDF and ISO 9660 both store a length or
/// compression byte right before the name), then falls back to the size.
fn image_kind(path: &Path) -> DiscKind {
    use std::io::Read;
    let scan = || -> Option<DiscKind> {
        let mut f = std::fs::File::open(path).ok()?;
        let mut buf = vec![0u8; 64 << 20];
        let n = f.read(&mut buf).ok()?;
        let hay = &buf[..n];
        let has = |needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
        if has(b"\x08BDMV") || has(b"\x04BDMV") {
            Some(DiscKind::Bluray)
        } else if has(b"\x08VIDEO_TS") {
            Some(DiscKind::Dvd)
        } else {
            None
        }
    };
    scan().unwrap_or_else(|| {
        // A dual-layer DVD holds at most 8.5 GB.
        let big = std::fs::metadata(path).is_ok_and(|m| m.len() > 9_400_000_000);
        if big { DiscKind::Bluray } else { DiscKind::Dvd }
    })
}

/// Ultra HD Blu-ray: the disc's BDMV/index.bdmv is version "0300" (plain
/// Blu-rays are "0100" or "0200").
fn is_uhd(root: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    std::fs::File::open(root.join("BDMV").join("index.bdmv"))
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && &head == b"INDX0300"
}

pub const UHD_HELP: &str = "This is an Ultra HD (4K) Blu-ray, protected with AACS 2.0. \
     The key database alone is usually not enough: the drive must be able to read UHD discs \
     in \"UHD-friendly\" (LibreDrive) mode, and this disc's key must be in the database. \
     Without a UHD-friendly drive, use MakeMKV (Settings → Blu-ray decryption → MakeMKV).";

/// Why this disc will probably not play, when a decryption library is missing.
fn decryption_warning(kind: DiscKind) -> Option<String> {
    match kind {
        DiscKind::Dvd
            if !library_present(&["libdvdcss.so.2", "libdvdcss-2.dll", "libdvdcss.2.dylib"]) =>
        {
            Some(DVDCSS_MISSING.into())
        }
        DiscKind::Bluray
            if !library_present(&["libaacs.so.0", "libaacs.dll", "libaacs.0.dylib"])
                && std::env::var("LIBAACS_PATH").is_err() =>
        {
            Some(
                "Blu-ray decryption (libaacs) is not installed: copy-protected discs will not play"
                    .into(),
            )
        }
        DiscKind::Bluray
            if std::env::var("LIBAACS_PATH").is_err()
                && !crate::settings::aacs_keydb().is_some_and(|p| p.exists()) =>
        {
            Some(
                "No Blu-ray key database yet: use Tools → Blu-ray key database to download one"
                    .into(),
            )
        }
        _ => None,
    }
}

pub const DVDCSS_MISSING: &str = "DVD decryption (libdvdcss) is not installed, so copy-protected DVDs can't play. \
     On Ubuntu/Debian: sudo apt install libdvd-pkg && sudo dpkg-reconfigure libdvd-pkg";

/// Whether a shared library can be loaded (tries each file name), from the
/// package's own lib/ folder (where libbluray and libdvdread find their
/// bundled copies) or the system.
pub fn library_present(names: &[&str]) -> bool {
    open_bundled_library(names).is_some()
}

/// Open the first of `names` that loads: the copy in the package's `lib/`
/// folder (next to the program), the developer copy in `.devlibs`, then the
/// system's. For optional libraries loaded at runtime.
pub fn open_bundled_library(names: &[&str]) -> Option<libloading::Library> {
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    let mut dirs: Vec<PathBuf> = exe_dir.iter().flat_map(|d| [d.join("lib"), d.clone()]).collect();
    if let Some(dev) = std::env::var_os("SMP_EXTRA_LIBS") {
        dirs.extend(std::env::split_paths(&dev));
    }
    names.iter().find_map(|n| {
        let candidates = dirs.iter().map(|d| d.join(n)).chain([PathBuf::from(n)]);
        candidates
            .filter(|c| !c.is_absolute() || c.exists())
            // SAFETY: these libraries have no unsafe load-time side effects.
            .find_map(|c| unsafe { libloading::Library::new(&c) }.ok())
    })
}

/// One audio track: start and length in CD frames (75 per second), relative to
/// the start of the first audio track, which is where mpv's cdda stream begins.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CdTrack {
    pub number: u8,
    pub start: u32,
    pub frames: u32,
}

const FRAMES_PER_SEC: f64 = 75.0;

/// Build an in-memory M3U playlist with one entry per song. Each entry is an
/// EDL segment of the cdda stream with a fixed length, so mpv knows each song's
/// exact duration up front (cdda alone only estimates it while reading), and
/// carries the song's tags when the names are known.
pub fn cd_playlist(tracks: &[CdTrack], album: Option<&crate::cd::Album>) -> String {
    let mut m3u = String::from("memory://#EXTM3U\n");
    for (i, t) in tracks.iter().enumerate() {
        let length = t.frames as f64 / FRAMES_PER_SEC;
        let name = match album {
            Some(a) => {
                let (artist, title) = (a.track_artist(i), a.track_title(i, t.number));
                if artist.is_empty() { title } else { format!("{artist} - {title}") }
            }
            None => format!("Track {}", t.number),
        };
        m3u += &format!(
            "#EXTINF:{},{}\n{}\n",
            length.round(),
            name.replace(['\n', '\r'], " "),
            crate::cd::track_edl(t, i, album)
        );
    }
    m3u
}

/// Audio tracks from (track number, start LBA, is data track) TOC entries,
/// where the last entry is the lead-out. Data tracks are skipped, and a song
/// ends where the next track (or the lead-out) starts.
pub fn audio_tracks(toc: &[(u8, u32, bool)]) -> Vec<CdTrack> {
    let Some(origin) = toc.iter().find(|e| !e.2).map(|e| e.1) else {
        return Vec::new();
    };
    toc.windows(2)
        .filter(|w| !w[0].2 && w[1].1 > w[0].1)
        .map(|w| CdTrack {
            number: w[0].0,
            start: w[0].1 - origin,
            frames: w[1].1 - w[0].1,
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum DriveStatus {
    DiscOk,
    NoDisc,
    TrayOpen,
    NotReady,
    Unknown,
}

/// Wait while the drive reports "not ready" (it spins up for several seconds
/// after a disc goes in), up to `limit`.
fn wait_until_ready(dev: &str, limit: Duration) -> DriveStatus {
    let start = Instant::now();
    loop {
        let status = drive_status(dev);
        if status != DriveStatus::NotReady || start.elapsed() > limit {
            if status == DriveStatus::NotReady {
                log::warn!("{dev}: still not ready after {}s", limit.as_secs());
            }
            return status;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

#[cfg(target_os = "linux")]
fn drive_status(dev: &str) -> DriveStatus {
    match toc::drive_status(dev) {
        Some(4) => DriveStatus::DiscOk,
        Some(1) => DriveStatus::NoDisc,
        Some(2) => DriveStatus::TrayOpen,
        Some(3) => DriveStatus::NotReady,
        // Some drivers don't report a status: let detection try.
        _ => DriveStatus::Unknown,
    }
}

#[cfg(not(target_os = "linux"))]
fn drive_status(_dev: &str) -> DriveStatus {
    DriveStatus::DiscOk
}

/// Where `dev` is mounted. With `mount`, ask udisks to mount it first (as the
/// file manager would; no root needed for removable media).
#[cfg(target_os = "linux")]
pub(crate) fn mount_point(dev: &str, mount: bool) -> Option<PathBuf> {
    let find = || {
        let mounts = std::fs::read_to_string("/proc/mounts").ok()?;
        let real = std::fs::canonicalize(dev).ok();
        mounts.lines().find_map(|l| {
            let mut f = l.split_whitespace();
            let (src, dst) = (f.next()?, f.next()?);
            let same = src == dev || real.as_deref() == std::fs::canonicalize(src).ok().as_deref();
            same.then(|| PathBuf::from(unescape_mount(dst)))
        })
    };
    if let Some(p) = find() {
        return Some(p);
    }
    if !mount {
        return None;
    }
    let ok = run_with_timeout(
        std::process::Command::new("udisksctl").args(["mount", "--no-user-interaction", "-b", dev]),
        Duration::from_secs(20),
    );
    if !ok {
        log::info!("{dev}: could not mount the disc to look at its contents");
    }
    find()
}

#[cfg(not(target_os = "linux"))]
fn mount_point(_dev: &str, _mount: bool) -> Option<PathBuf> {
    None
}

/// The optical drive mounted at `dir`, if any.
#[cfg(target_os = "linux")]
fn device_of_mount(dir: &Path) -> Option<String> {
    let dir = std::fs::canonicalize(dir).ok()?;
    let mounts = std::fs::read_to_string("/proc/mounts").ok()?;
    mounts.lines().find_map(|l| {
        let mut f = l.split_whitespace();
        let (src, dst) = (f.next()?, f.next()?);
        let is_drive = src.starts_with("/dev/sr") || src.starts_with("/dev/cdrom");
        (is_drive && Path::new(&unescape_mount(dst)) == dir).then(|| src.to_owned())
    })
}

#[cfg(not(target_os = "linux"))]
fn device_of_mount(_dir: &Path) -> Option<String> {
    None
}

/// /proc/mounts escapes spaces and friends as octal (\040).
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(target_os = "linux")]
fn run_with_timeout(cmd: &mut std::process::Command, limit: Duration) -> bool {
    use std::process::Stdio;
    let Ok(mut child) = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Optical drives as (device path, udev "KEY=VALUE" properties), sorted.
#[cfg(target_os = "linux")]
fn list_drives() -> Option<Vec<(String, Vec<String>)>> {
    let mut drives: Vec<(String, Vec<String>)> = std::fs::read_dir("/sys/class/block")
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with("sr") {
                return None;
            }
            let dev = std::fs::read_to_string(e.path().join("dev")).ok()?;
            let props = std::fs::read_to_string(format!("/run/udev/data/b{}", dev.trim()))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| l.strip_prefix("E:").map(str::to_owned))
                .collect();
            Some((format!("/dev/{name}"), props))
        })
        .collect();
    drives.sort();
    Some(drives)
}

#[cfg(not(target_os = "linux"))]
fn list_drives() -> Option<Vec<(String, Vec<String>)>> {
    None
}

/// The Linux CD-ROM ioctls: drive status and a CD's table of contents.
#[cfg(target_os = "linux")]
pub(crate) mod toc {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    const CDROMREADTOCHDR: u64 = 0x5305;
    const CDROMREADTOCENTRY: u64 = 0x5306;
    const CDROM_DRIVE_STATUS: u64 = 0x5326;
    const CDSL_CURRENT: i32 = i32::MAX;
    const CDROM_LBA: u8 = 0x01;
    const CDROM_LEADOUT: u8 = 0xAA;
    const CDROM_DATA_TRACK: u8 = 0x04;
    const O_NONBLOCK: i32 = 0o4000;

    #[repr(C)]
    struct TocHeader {
        first: u8,
        last: u8,
    }

    /// `struct cdrom_tocentry` with the LBA arm of its address union.
    #[repr(C)]
    struct TocEntry {
        track: u8,
        /// Low nibble: adr, high nibble: ctrl.
        adr_ctrl: u8,
        format: u8,
        lba: i32,
        datamode: u8,
    }

    unsafe extern "C" {
        fn ioctl(fd: i32, request: u64, ...) -> i32;
    }

    /// Non-blocking, so an open tray or empty drive fails fast.
    fn open(device: &str) -> Option<std::fs::File> {
        std::fs::File::options()
            .read(true)
            .custom_flags(O_NONBLOCK)
            .open(device)
            .ok()
    }

    /// CDS_NO_INFO (0), CDS_NO_DISC (1), CDS_TRAY_OPEN (2), CDS_DRIVE_NOT_READY (3), CDS_DISC_OK (4).
    pub fn drive_status(device: &str) -> Option<i32> {
        let file = open(device)?;
        let rc = unsafe { ioctl(file.as_raw_fd(), CDROM_DRIVE_STATUS, CDSL_CURRENT) };
        (rc >= 0).then_some(rc)
    }

    /// Whether the disc's data can be read at all: one sector from where
    /// every DVD/Blu-ray filesystem starts. A drive refusing the disc (an
    /// Ultra HD Blu-ray in a drive that isn't UHD-friendly) fails this with
    /// "no medium", though it reports a disc.
    pub fn readable(device: &str) -> bool {
        use std::os::unix::fs::FileExt;
        let Some(file) = open(device) else { return false };
        let mut buf = [0u8; 2048];
        file.read_at(&mut buf, 16 * 2048).is_ok()
    }

    const SG_IO: u64 = 0x2285;
    const SG_DXFER_FROM_DEV: i32 = -3;

    /// `struct sg_io_hdr` (scsi/sg.h).
    #[repr(C)]
    struct SgIoHdr {
        interface_id: i32,
        dxfer_direction: i32,
        cmd_len: u8,
        mx_sb_len: u8,
        iovec_count: u16,
        dxfer_len: u32,
        dxferp: *mut u8,
        cmdp: *const u8,
        sbp: *mut u8,
        timeout: u32,
        flags: u32,
        pack_id: i32,
        usr_ptr: *mut u8,
        status: u8,
        masked_status: u8,
        msg_status: u8,
        sb_len_wr: u8,
        host_status: u16,
        driver_status: u16,
        resid: i32,
        duration: u32,
        info: u32,
    }

    /// READ TOC/PMA/ATIP format 5 (CD-Text) into `buf`; the bytes the
    /// drive says it has (header included), or None.
    fn read_cd_text_into(fd: i32, buf: &mut [u8]) -> Option<usize> {
        let len = buf.len().min(0xFFFF) as u16;
        let cdb = [0x43u8, 0x00, 0x05, 0, 0, 0, 0, (len >> 8) as u8, len as u8, 0];
        let mut sense = [0u8; 32];
        let mut hdr = SgIoHdr {
            interface_id: b'S' as i32,
            dxfer_direction: SG_DXFER_FROM_DEV,
            cmd_len: cdb.len() as u8,
            mx_sb_len: sense.len() as u8,
            iovec_count: 0,
            dxfer_len: u32::from(len),
            dxferp: buf.as_mut_ptr(),
            cmdp: cdb.as_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: 5000,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        };
        let rc = unsafe { ioctl(fd, SG_IO, &mut hdr) };
        if rc != 0 || hdr.status != 0 || hdr.host_status != 0 || hdr.driver_status != 0 {
            return None;
        }
        Some(usize::from(u16::from_be_bytes([buf[0], buf[1]])) + 2)
    }

    /// The disc's raw CD-Text packs (18 bytes each), if it has any.
    pub fn cd_text(device: &str) -> Option<Vec<u8>> {
        let file = open(device)?;
        let fd = file.as_raw_fd();
        let mut head = [0u8; 4];
        let size = read_cd_text_into(fd, &mut head)?;
        if size <= 4 {
            return None;
        }
        let mut buf = vec![0u8; size.min(0xFFFF)];
        let got = read_cd_text_into(fd, &mut buf)?.min(buf.len());
        (got > 4).then(|| buf[4..got].to_vec())
    }

    /// (track number, start LBA, is data track) for each track, then the lead-out.
    /// None when the drive has no disc or the disc has no TOC (DVD, Blu-ray).
    pub fn read(device: &str) -> Option<Vec<(u8, u32, bool)>> {
        let file = open(device)?;
        let fd = file.as_raw_fd();
        let mut header = TocHeader { first: 0, last: 0 };
        if unsafe { ioctl(fd, CDROMREADTOCHDR, &mut header) } != 0
            || header.first == 0
            || header.last < header.first
            || header.last > 99
        {
            return None;
        }
        let entry = |track: u8| {
            let mut e = TocEntry {
                track,
                adr_ctrl: 0,
                format: CDROM_LBA,
                lba: 0,
                datamode: 0,
            };
            let ok = unsafe { ioctl(fd, CDROMREADTOCENTRY, &mut e) } == 0;
            (ok && e.lba >= 0).then_some((
                track,
                e.lba as u32,
                e.adr_ctrl >> 4 & CDROM_DATA_TRACK != 0,
            ))
        };
        (header.first..=header.last)
            .chain([CDROM_LEADOUT])
            .map(entry)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// The setup that went wrong: a Blu-ray drive with an unreadable (or no)
    /// disc, and a DVD drive holding an audio CD.
    fn two_drives() -> Vec<(String, Vec<String>)> {
        vec![
            (
                "/dev/sr0".into(),
                props(&["ID_MODEL=TSSTcorp_DVD", "ID_CDROM_CD=1", "ID_CDROM_DVD=1"]),
            ),
            (
                "/dev/sr1".into(),
                props(&[
                    "ID_MODEL=HL-DT-ST_BD-RE",
                    "ID_CDROM_CD=1",
                    "ID_CDROM_DVD=1",
                    "ID_CDROM_BD=1",
                ]),
            ),
        ]
    }

    #[test]
    fn blu_ray_request_does_not_play_a_cd_from_the_other_drive() {
        let drives = two_drives();
        let cd = DiscPlan::new(DiscKind::AudioCd, "/dev/sr0".into());
        let found = vec![(cd, drives[0].1.as_slice())];
        let err = choose(Some(DiscKind::Bluray), found, &[], &drives).unwrap_err();
        assert!(err.contains("No Blu-ray found"), "{err}");
        assert!(err.contains("the Blu-ray drive (LG, /dev/sr1) has no disc"), "{err}");
        assert!(err.contains("an audio CD in /dev/sr0"), "{err}");
    }

    #[test]
    fn blu_ray_request_plays_a_dvd_in_the_blu_ray_drive() {
        let drives = two_drives();
        let cd = DiscPlan::new(DiscKind::AudioCd, "/dev/sr0".into());
        let dvd = DiscPlan::new(DiscKind::Dvd, "/dev/sr1".into());
        let found = vec![(cd, drives[0].1.as_slice()), (dvd, drives[1].1.as_slice())];
        let plans = choose(Some(DiscKind::Bluray), found, &[], &drives).unwrap();
        let [plan] = <[DiscPlan; 1]>::try_from(plans).unwrap();
        assert_eq!(plan.device, "/dev/sr1");
        assert_eq!(plan.kind, DiscKind::Dvd);
        assert!(
            plan.note
                .unwrap()
                .starts_with("This disc is a DVD, not a Blu-ray")
        );
    }

    #[test]
    fn matching_kind_wins_over_drive_order() {
        let drives = two_drives();
        let cd = DiscPlan::new(DiscKind::AudioCd, "/dev/sr0".into());
        let bd = DiscPlan::new(DiscKind::Bluray, "/dev/sr1".into());
        let found = vec![(bd, drives[1].1.as_slice()), (cd, drives[0].1.as_slice())];
        let plans = choose(Some(DiscKind::AudioCd), found, &[], &drives).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].device, "/dev/sr0");
        assert!(plans[0].note.is_none());
    }

    #[test]
    fn detect_takes_whatever_is_there() {
        let drives = two_drives();
        let cd = DiscPlan::new(DiscKind::AudioCd, "/dev/sr0".into());
        let plans = choose(None, vec![(cd, drives[0].1.as_slice())], &[], &drives).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].kind, DiscKind::AudioCd);
        let err = choose(Some(DiscKind::Bluray), vec![], &["/dev/sr1"], &drives).unwrap_err();
        assert!(err.contains("tray of the Blu-ray drive (LG, /dev/sr1) is open"), "{err}");
    }

    #[test]
    fn detect_with_discs_in_both_drives_offers_both() {
        let drives = two_drives();
        let dvd = DiscPlan::new(DiscKind::Dvd, "/dev/sr0".into());
        let bd = DiscPlan::new(DiscKind::Bluray, "/dev/sr1".into());
        let found = vec![(dvd.clone(), drives[0].1.as_slice()), (bd.clone(), drives[1].1.as_slice())];
        let plans = choose(None, found, &[], &drives).unwrap();
        assert_eq!(plans.len(), 2);
        // Asking for one kind still plays it straight away.
        let found = vec![(dvd, drives[0].1.as_slice()), (bd, drives[1].1.as_slice())];
        let plans = choose(Some(DiscKind::Bluray), found, &[], &drives).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].device, "/dev/sr1");
    }

    #[test]
    fn audio_tracks_from_toc() {
        // An "enhanced CD": two songs, then a data track, then the lead-out.
        let toc = [
            (1, 150, false),
            (2, 19942, false),
            (3, 38487, true),
            (0xAA, 60000, false),
        ];
        assert_eq!(
            audio_tracks(&toc),
            [
                CdTrack {
                    number: 1,
                    start: 0,
                    frames: 19792
                },
                CdTrack {
                    number: 2,
                    start: 19792,
                    frames: 18545
                },
            ]
        );
        assert!(audio_tracks(&[(1, 0, true), (0xAA, 900, false)]).is_empty());
    }

    #[test]
    fn playlist_has_one_timed_entry_per_song() {
        let m3u = cd_playlist(&[CdTrack {
            number: 2,
            start: 19792,
            frames: 18545,
        }], None);
        assert_eq!(
            m3u,
            "memory://#EXTM3U\n#EXTINF:247,Track 2\nedl://%7%cdda://,start=263.893,length=247.267\n"
        );
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("smp-disc-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dvd_folder_and_vob_files_play_as_dvd() {
        let root = tmp("dvd");
        std::fs::create_dir_all(root.join("VIDEO_TS")).unwrap();
        let vob = root.join("VIDEO_TS").join("VTS_01_1.VOB");
        std::fs::write(&vob, b"x").unwrap();
        let expect = root.to_string_lossy().into_owned();
        for p in [root.clone(), root.join("VIDEO_TS"), vob] {
            let plan = plan_path(&p).unwrap();
            assert_eq!(
                (plan.kind, plan.device.as_str(), plan.url.as_str()),
                (DiscKind::Dvd, expect.as_str(), "dvd://")
            );
        }
        // A VOB that isn't inside VIDEO_TS is just a file.
        let loose = root.join("movie.vob");
        std::fs::write(&loose, b"x").unwrap();
        assert_eq!(plan_path(&loose), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bluray_folder_plays_as_bluray() {
        let root = tmp("bd");
        std::fs::create_dir_all(root.join("BDMV").join("STREAM")).unwrap();
        std::fs::write(root.join("BDMV").join("index.bdmv"), b"x").unwrap();
        for p in [
            root.clone(),
            root.join("BDMV"),
            root.join("BDMV").join("index.bdmv"),
        ] {
            assert_eq!(plan_path(&p).unwrap().kind, DiscKind::Bluray, "{p:?}");
        }
        assert_eq!(plan_path(&root.join("BDMV").join("STREAM")), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn drives_have_plain_names() {
        let lg = props(&["ID_MODEL=HL-DT-ST_BD-RE__WH14NS40", "ID_CDROM_DVD=1", "ID_CDROM_BD=1"]);
        assert_eq!(drive_label("/dev/sr1", &lg), "Blu-ray drive (LG WH14NS40, /dev/sr1)");
        let tsst = props(&["ID_MODEL=TSSTcorp_DVD+_-RW_SH-216DB", "ID_CDROM_CD=1", "ID_CDROM_DVD=1"]);
        assert_eq!(drive_label("/dev/sr0", &tsst), "DVD drive (TSST SH-216DB, /dev/sr0)");
        assert_eq!(drive_label("/dev/sr2", &[]), "CD drive (/dev/sr2)");
    }

    #[test]
    fn refused_message_names_the_drive_once() {
        let m = refused_message("Blu-ray drive (LG WH14NS40, /dev/sr1)");
        assert!(m.starts_with("The Blu-ray drive (LG WH14NS40, /dev/sr1) sees a disc"), "{m}");
        assert!(is_refused_error(&m));
    }

    #[test]
    fn uhd_blu_ray_is_recognised() {
        let root = tmp("uhd");
        std::fs::create_dir_all(root.join("BDMV")).unwrap();
        std::fs::write(root.join("BDMV").join("index.bdmv"), b"INDX0300rest").unwrap();
        let plan = plan_path(&root).unwrap();
        assert_eq!(plan.kind, DiscKind::Bluray);
        assert_eq!(plan.warning.as_deref(), Some(UHD_HELP));
        std::fs::write(root.join("BDMV").join("index.bdmv"), b"INDX0200rest").unwrap();
        assert!(!is_uhd(&root));
    }

    #[test]
    fn image_kind_from_directory_entries() {
        let root = tmp("iso");
        let mut dvd = vec![0u8; 40_000];
        dvd[33_000..33_009].copy_from_slice(b"\x08VIDEO_TS");
        std::fs::write(root.join("dvd.iso"), &dvd).unwrap();
        let mut bd = vec![0u8; 40_000];
        bd[20_000..20_005].copy_from_slice(b"\x08BDMV");
        std::fs::write(root.join("bd.iso"), &bd).unwrap();
        assert_eq!(image_kind(&root.join("dvd.iso")), DiscKind::Dvd);
        assert_eq!(image_kind(&root.join("bd.iso")), DiscKind::Bluray);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mount_paths_are_unescaped() {
        assert_eq!(unescape_mount("/media/me/MY\\040DISC"), "/media/me/MY DISC");
        assert_eq!(unescape_mount("/plain"), "/plain");
    }
}

#[cfg(test)]
mod machine {
    /// What Play disc → Blu-ray reports on this computer:
    /// `cargo test -p super-media-player disc_search_this_machine -- --ignored --nocapture`.
    #[test]
    #[ignore = "uses this machine's drives"]
    fn disc_search_this_machine() {
        let t = std::time::Instant::now();
        let r = super::plan_drive(Some(super::DiscKind::Bluray), |m| {
            println!("[{:>4.1}s] {m}", t.elapsed().as_secs_f32())
        });
        println!("[{:>4.1}s] result: {r:?}", t.elapsed().as_secs_f32());
    }
}
