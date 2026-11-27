//! Helper programs and data the player can fetch for the user:
//!
//! - **yt-dlp**, which mpv runs to play YouTube and ~1800 other sites. The
//!   standalone build needs no Python. Downloads are checked against the
//!   SHA-256 list published with each release.
//! - The **Blu-ray key database** (KEYDB.cfg) that libaacs uses to decrypt
//!   commercial Blu-rays.
//!
//! Everything here blocks; call it from a worker thread.

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::settings;

const YTDLP_RELEASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
/// Public FindVUK database. Note: the site serves the zip over plain HTTP only.
pub const KEYDB_URL: &str = "http://fvonline-db.bplaced.net/fv_download.php?lang=eng";

/// The yt-dlp release file for this platform.
fn ytdlp_asset() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "yt-dlp_linux",
        ("linux", "aarch64") => "yt-dlp_linux_aarch64",
        ("windows", "x86_64") => "yt-dlp.exe",
        ("windows", "aarch64") => "yt-dlp_arm64.exe",
        ("macos", _) => "yt-dlp_macos",
        _ => return None,
    })
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    }
}

/// Where the app keeps its own copy of yt-dlp.
pub fn managed_ytdlp() -> Option<PathBuf> {
    settings::data_dir().map(|d| d.join("bin").join(exe_name()))
}

/// yt-dlp to use: the app's own copy, one shipped next to the program (the
/// packages bundle it), or one on PATH.
pub fn find_ytdlp() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf));
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.extend(managed_ytdlp());
    if let Some(d) = &exe_dir {
        candidates.push(d.join(exe_name()));
        candidates.push(d.join("bin").join(exe_name()));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join(exe_name()));
        }
    }
    candidates.into_iter().find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

fn get(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut resp = ureq::get(url)
        .config()
        .timeout_connect(Some(std::time::Duration::from_secs(20)))
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .build()
        .header(
            "User-Agent",
            concat!("SuperMediaPlayer/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    resp.body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| format!("download failed: {e}"))
}

/// Write `bytes` to `dest` through a temporary file, so a failed download
/// never leaves a broken file behind.
fn write_atomic(dest: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let dir = dest.parent().ok_or("bad destination")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .map_err(|e| e.to_string())?;
    }
    let _ = mode;
    std::fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", dest.display()))
}

/// Expected hash of `asset` from a release's `SHA2-256SUMS` file.
fn expected_sha256(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, name) = l.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == asset && hash.len() == 64)
            .then(|| hash.to_lowercase())
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Download the latest standalone yt-dlp into the app's data folder.
pub fn install_ytdlp() -> Result<PathBuf, String> {
    let dest = managed_ytdlp().ok_or("no data folder (HOME is not set)")?;
    download_ytdlp_to(&dest)?;
    log::info!("installed yt-dlp to {}", dest.display());
    Ok(dest)
}

/// Download the latest standalone yt-dlp to `dest`, checking its SHA-256.
fn download_ytdlp_to(dest: &Path) -> Result<(), String> {
    let asset = ytdlp_asset().ok_or("yt-dlp has no standalone build for this system")?;
    let sums = String::from_utf8(get(&format!("{YTDLP_RELEASE}/SHA2-256SUMS"), 1 << 20)?)
        .map_err(|_| "bad checksum list")?;
    let want = expected_sha256(&sums, asset).ok_or("checksum for yt-dlp not found")?;
    let bin = get(&format!("{YTDLP_RELEASE}/{asset}"), 300 << 20)?;
    let got = hex(&Sha256::digest(&bin));
    if got != want {
        return Err(format!(
            "yt-dlp download is corrupt (checksum {got}, expected {want})"
        ));
    }
    write_atomic(dest, &bin, 0o755)
}

/// Update yt-dlp. Our own copy updates itself (`-U` checks the release
/// signature); other copies belong to the system's package manager.
pub fn update_ytdlp() -> Result<String, String> {
    let Some(path) = find_ytdlp() else {
        return install_ytdlp().map(|p| format!("yt-dlp installed to {}", p.display()));
    };
    let ours = managed_ytdlp().is_some_and(|m| m == path);
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| path.starts_with(d)))
        .unwrap_or(false);
    if !ours && !bundled {
        // A system copy we can't update: install our own, which then wins.
        return install_ytdlp().map(|p| format!("yt-dlp installed to {}", p.display()));
    }
    if bundled && !ours {
        // The package's copy may sit in a read-only folder.
        return install_ytdlp().map(|_| "yt-dlp updated".to_owned());
    }
    let out = crate::process::command(&path)
        .arg("-U")
        .output()
        .map_err(|e| format!("could not run yt-dlp: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let last = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("done")
        .trim();
    if out.status.success() {
        Ok(format!("yt-dlp: {last}"))
    } else {
        Err(format!("yt-dlp update failed: {last}"))
    }
}

/// yt-dlp's version, e.g. "2026.11.03".
pub fn ytdlp_version(path: &Path) -> Option<String> {
    let out = crate::process::command(path)
        .arg("--version")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// How often the background updater looks for a new yt-dlp (sites change
/// weekly) and refreshes the key database (new discs come out monthly).
const YTDLP_CHECK_DAYS: u64 = 7;
const KEYDB_CHECK_DAYS: u64 = 30;
const YTDLP_LATEST_API: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";

/// What the background updater changed.
#[derive(Debug, Default)]
pub struct AutoUpdate {
    /// New yt-dlp version, when it was updated.
    pub ytdlp: Option<String>,
    pub keydb: bool,
}

/// Background update, run now and then (main.rs). Each part runs at most
/// once per its interval, remembered in stamp files, so frequent starts
/// don't mean frequent downloads. Only updates what the user already has:
/// - yt-dlp: the app's own or the packaged copy, never one from the
///   system's package manager, and never a first install (that is asked).
/// - The key database: only once the user has downloaded it themselves.
pub fn auto_update() -> AutoUpdate {
    let mut result = AutoUpdate::default();
    if check_due("ytdlp", YTDLP_CHECK_DAYS) {
        match auto_update_ytdlp() {
            Ok(v) => {
                result.ytdlp = v;
                mark_checked("ytdlp");
            }
            Err(e) => log::warn!("yt-dlp update check failed: {e}"),
        }
    }
    let have_keydb = settings::aacs_keydb().is_some_and(|p| p.exists());
    if have_keydb && check_due("keydb", KEYDB_CHECK_DAYS) {
        match install_keydb() {
            Ok(_) => {
                result.keydb = true;
                mark_checked("keydb");
            }
            Err(e) => log::warn!("key database update failed: {e}"),
        }
    }
    result
}

/// Update yt-dlp when a newer release exists. `Ok(Some(version))` when it
/// was updated, `Ok(None)` when up to date or not ours to update.
fn auto_update_ytdlp() -> Result<Option<String>, String> {
    let Some(path) = find_ytdlp() else {
        return Ok(None);
    };
    let managed = managed_ytdlp().ok_or("no data folder")?;
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| path.starts_with(d)))
        .unwrap_or(false);
    if path != managed && !bundled {
        log::info!(
            "yt-dlp at {} is managed by the system; not updating it",
            path.display()
        );
        return Ok(None);
    }
    let current = ytdlp_version(&path).unwrap_or_default();
    let json = String::from_utf8(get(YTDLP_LATEST_API, 1 << 20)?).map_err(|_| "bad reply")?;
    let latest =
        crate::artist::json_string_field(&json, "tag_name").ok_or("no version in reply")?;
    if !is_newer(&latest, &current) {
        log::info!("yt-dlp {current} is up to date");
        return Ok(None);
    }
    // Into the app's own folder: the packaged copy may be read-only, and the
    // app's copy is found first from then on.
    download_ytdlp_to(&managed)?;
    log::info!("yt-dlp updated from {current} to {latest}");
    Ok(Some(latest))
}

/// yt-dlp versions are dates ("2026.11.03", nightlies add ".123456"), so
/// newer means greater number by number.
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    let (l, c) = (parse(latest), parse(current));
    !l.iter().all(|&n| n == 0) && l > c
}

fn stamp(name: &str) -> Option<PathBuf> {
    settings::data_dir().map(|d| d.join("update-checks").join(name))
}

fn check_due(name: &str, days: u64) -> bool {
    let age = stamp(name)
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok());
    age.is_none_or(|a| a.as_secs() >= days * 86_400)
}

fn mark_checked(name: &str) {
    if let Some(p) = stamp(name) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, b"");
    }
}

/// The KEYDB.cfg inside the downloaded zip.
fn keydb_from_zip(zip_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| format!("not a zip file: {e}"))?;
    let name = (0..archive.len())
        .filter_map(|i| Some(archive.name_for_index(i)?.ok()?.into_owned()))
        .find(|n| n.to_lowercase().ends_with("keydb.cfg"))
        .ok_or("the download has no keydb.cfg")?;
    let mut entry = archive.by_name(&name).map_err(|e| e.to_string())?;
    let mut cfg = Vec::new();
    entry
        .by_ref()
        .take(500 << 20)
        .read_to_end(&mut cfg)
        .map_err(|e| format!("could not unpack: {e}"))?;
    if !looks_like_keydb(&cfg) {
        return Err("the download doesn't look like a key database".into());
    }
    Ok(cfg)
}

/// Download the FindVUK key database to ~/.config/aacs/KEYDB.cfg.
pub fn install_keydb() -> Result<PathBuf, String> {
    let dest = settings::aacs_keydb().ok_or("no config folder (HOME is not set)")?;
    let zip_bytes = get(KEYDB_URL, 200 << 20)?;
    let cfg = keydb_from_zip(&zip_bytes)?;
    write_atomic(&dest, &cfg, 0o600)?;
    log::info!("installed Blu-ray key database to {}", dest.display());
    Ok(dest)
}

/// KEYDB.cfg is text with one disc per line: `0x<disc id> = <title> | ...`.
/// It arrives over plain HTTP, so check it is that before libaacs parses it.
fn looks_like_keydb(cfg: &[u8]) -> bool {
    if cfg.len() < 100 || cfg.contains(&0) {
        return false;
    }
    let text = String::from_utf8_lossy(&cfg[..cfg.len().min(1 << 20)]);
    let entries = text
        .lines()
        .filter(|l| l.starts_with("0x") && l.contains(" = ") && l.contains('|'))
        .take(10)
        .count();
    entries >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_lookup() {
        let sums = "aa  yt-dlp\n".to_owned()
            + &"b".repeat(64)
            + "  yt-dlp_linux\n"
            + &"c".repeat(64)
            + "  yt-dlp_linux.zip\n";
        assert_eq!(expected_sha256(&sums, "yt-dlp_linux"), Some("b".repeat(64)));
        assert_eq!(expected_sha256(&sums, "yt-dlp"), None); // not 64 hex chars
        assert_eq!(expected_sha256(&sums, "nope"), None);
    }

    /// Downloads ~35 MB and runs it: `cargo test -p super-media-player -- --ignored`.
    #[test]
    #[ignore = "network"]
    fn downloads_and_verifies_ytdlp() {
        let dir = std::env::temp_dir().join(format!("smp-ytdlp-{}", std::process::id()));
        let dest = dir.join(exe_name());
        download_ytdlp_to(&dest).unwrap();
        assert!(is_executable(&dest));
        let version = ytdlp_version(&dest).expect("yt-dlp runs");
        assert!(version.starts_with("20"), "{version}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unpacks_the_key_database() {
        // keydb.cfg, deflated, as the site serves it.
        #[rustfmt::skip]
        let good: &[u8] = &[
            80, 75, 3, 4, 20, 0, 0, 0, 8, 0, 0, 0, 33, 0, 214, 216, 248, 151, 54, 0, 0, 0, 20, 1, 0,
            0, 9, 0, 0, 0, 107, 101, 121, 100, 98, 46, 99, 102, 103, 51, 168, 48, 32, 18, 40, 216,
            42, 184, 100, 22, 39, 43, 24, 40, 212, 40, 184, 0, 177, 145, 129, 145, 153, 174, 129,
            33, 16, 113, 25, 16, 107, 136, 33, 204, 16, 67, 10, 12, 49, 130, 25, 98, 68, 129, 33,
            198, 48, 67, 140, 49, 13, 1, 0, 80, 75, 1, 2, 20, 0, 20, 0, 0, 0, 8, 0, 0, 0, 33, 0,
            214, 216, 248, 151, 54, 0, 0, 0, 20, 1, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 107, 101, 121, 100, 98, 46, 99, 102, 103, 80, 75, 5, 6, 0, 0, 0, 0, 1, 0, 1,
            0, 55, 0, 0, 0, 93, 0, 0, 0, 0, 0,
        ];
        let cfg = keydb_from_zip(good).unwrap();
        assert!(String::from_utf8(cfg).unwrap().contains("= Disc 3 |"));
        // Damaged data is noticed, and so is something else than a zip.
        let mut bad = good.to_vec();
        bad[45] ^= 0xff;
        assert!(keydb_from_zip(&bad).is_err());
        assert!(keydb_from_zip(b"<html>not found</html>").is_err());
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer("2026.11.20", "2026.11.03"));
        assert!(is_newer("2026.11.03.123456", "2026.11.03"));
        assert!(is_newer("2027.01.01", "2026.12.31"));
        assert!(!is_newer("2026.11.03", "2026.11.03"));
        assert!(!is_newer("2026.10.30", "2026.11.03"));
        // A broken reply never counts as newer; an unknown local version does.
        assert!(!is_newer("garbage", "2026.11.03"));
        assert!(is_newer("2026.11.03", ""));
    }

    #[test]
    fn keydb_sanity_check() {
        let good = "; KEYDB\n".to_owned()
            + &"0x0123456789ABCDEF0123456789ABCDEF01234567 = MOVIE (Some Movie) | D | 2020-01-01 | V | 0x00\n"
                .repeat(5);
        assert!(looks_like_keydb(good.as_bytes()));
        assert!(!looks_like_keydb(b"<html>not found</html>"));
        assert!(!looks_like_keydb(&[0u8; 500]));
    }
}
