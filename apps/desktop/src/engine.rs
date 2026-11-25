//! Updating FFmpeg, the decoding library under libmpv.
//!
//! FFmpeg can't be swapped on its own: libmpv is built against one FFmpeg
//! major version (6.x here: libavcodec.so.60), so a newer major needs a
//! libmpv built to match. What *can* be updated is where the libraries come
//! from:
//! - **System packages** (Ubuntu/Debian, and this app's development setup):
//!   the distribution ships FFmpeg fixes as package updates. [`update_system`]
//!   refreshes the package lists and installs newer FFmpeg/libmpv packages,
//!   asking for the administrator password once (pkexec).
//! - **Bundled** (the self-contained package, FFmpeg in its `lib/` folder):
//!   updated by installing a newer release of the player.
//!
//! Everything here blocks; call it from a worker thread.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Packages that make up FFmpeg and libmpv on Debian/Ubuntu.
const PACKAGE_PATTERN: &str = "^(libav|libsw|libpostproc|libmpv|ffmpeg)";

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// From the system's package manager, at this path.
    System(PathBuf),
    /// Shipped inside the player's own folder.
    Bundled(PathBuf),
    /// Not found in this process (engine not started yet).
    Unknown,
}

/// Where the running player loaded FFmpeg (libavcodec) from, by reading the
/// process's memory map.
pub fn source() -> Source {
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return Source::Unknown;
    };
    let Some(path) = maps
        .lines()
        .filter_map(|l| l.split_whitespace().nth(5))
        .find(|p| p.contains("/libavcodec.so"))
        .map(PathBuf::from)
    else {
        return Source::Unknown;
    };
    classify(&path, std::env::current_exe().ok().as_deref())
}

/// Only a library in the system's own folders comes from its package manager;
/// one beside the executable, or the engine scripts/build-engine.sh builds
/// into .devlibs/, travels with the player instead.
fn classify(lib: &Path, exe: Option<&Path>) -> Source {
    let beside_exe = exe
        .and_then(Path::parent)
        .is_some_and(|dir| lib.starts_with(dir.join("lib")));
    let system = ["/usr/", "/lib/", "/lib64/"].iter().any(|d| lib.starts_with(d));
    if beside_exe || !system {
        Source::Bundled(lib.to_owned())
    } else {
        Source::System(lib.to_owned())
    }
}

/// An FFmpeg/libmpv package with a newer version available.
#[derive(Clone, Debug, PartialEq)]
pub struct Upgrade {
    pub package: String,
    pub installed: String,
    pub available: String,
}

/// Parse `apt list --upgradable` lines such as
/// `libavcodec60/noble-updates 7:6.1.1-3ubuntu5.1 amd64 [upgradable from: 7:6.1.1-3ubuntu5]`.
fn parse_upgradable(out: &str) -> Vec<Upgrade> {
    let re_prefix = |name: &str| {
        ["libav", "libsw", "libpostproc", "libmpv", "ffmpeg"]
            .iter()
            .any(|p| name.starts_with(p))
    };
    out.lines()
        .filter_map(|l| {
            let (name, rest) = l.split_once('/')?;
            if !re_prefix(name) {
                return None;
            }
            let available = rest.split_whitespace().nth(1)?.to_owned();
            let installed = l
                .split("upgradable from: ")
                .nth(1)?
                .trim_end_matches(']')
                .trim()
                .to_owned();
            Some(Upgrade { package: name.to_owned(), installed, available })
        })
        .collect()
}

/// FFmpeg/libmpv packages with updates, according to the package lists as
/// of the last refresh (refreshing needs the administrator; [`update_system`]
/// does it). `None` when this isn't a Debian/Ubuntu system.
pub fn pending_upgrades() -> Option<Vec<Upgrade>> {
    let out = Command::new("apt")
        .args(["list", "--upgradable"])
        .env("LC_ALL", "C")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_upgradable(&String::from_utf8_lossy(&out.stdout)))
}

/// The installed package providing libavcodec, with its version, e.g.
/// ("libavcodec-extra60", "7:6.1.1-3ubuntu5+esm15").
pub fn installed_package(lib: &Path) -> Option<(String, String)> {
    let out = Command::new("dpkg").arg("-S").arg(lib).output().ok()?;
    let pkg = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .split(':')
        .next()?
        .trim()
        .to_owned();
    let ver = Command::new("dpkg-query")
        .args(["-W", "-f=${Version}", &pkg])
        .output()
        .ok()?;
    Some((pkg, String::from_utf8_lossy(&ver.stdout).trim().to_owned()))
}

/// The shell command [`update_system`] runs as administrator: refresh the
/// package lists, then upgrade only the FFmpeg/libmpv packages already
/// installed (nothing new is added).
pub fn update_command() -> String {
    format!(
        "apt-get update -qq && apt-get install --only-upgrade -y \
         $(dpkg-query -W -f='${{binary:Package}}\\n' | grep -E '{PACKAGE_PATTERN}')"
    )
}

/// Update the system's FFmpeg and libmpv packages. Asks for the
/// administrator password once (polkit). `Ok(true)` when something was
/// upgraded.
pub fn update_system() -> Result<bool, String> {
    let before = pending_upgrades().unwrap_or_default();
    let out = Command::new("pkexec")
        .args(["sh", "-c", &update_command()])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not ask for the administrator password (pkexec): {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string()
        + &String::from_utf8_lossy(&out.stderr);
    match out.status.code() {
        Some(0) => {
            // apt prints "N upgraded, M newly installed, ...".
            let upgraded = text
                .lines()
                .find(|l| l.contains(" upgraded,"))
                .and_then(|l| l.split_whitespace().next())
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(before.len() as u32);
            log::info!("FFmpeg update: {upgraded} package(s) upgraded");
            Ok(upgraded > 0)
        }
        // pkexec: 126 = the password dialog was dismissed, 127 = not authorized.
        Some(126) | Some(127) => Err("The update was cancelled (no administrator password)".into()),
        _ => {
            let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed");
            Err(format!("The update failed: {}", last.trim()))
        }
    }
}

/// "6.1.1" from mpv's "6.1.1-3ubuntu5+esm15".
pub fn short_version(full: &str) -> &str {
    full.split('-').next().unwrap_or(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgradable_ffmpeg_packages() {
        let out = "Listing...\n\
            libavcodec60/noble-updates 7:6.1.1-3ubuntu5.1 amd64 [upgradable from: 7:6.1.1-3ubuntu5]\n\
            firefox/noble 130.0 amd64 [upgradable from: 129.0]\n\
            libmpv2/noble-updates 0.37.0-1ubuntu4.1 amd64 [upgradable from: 0.37.0-1ubuntu4]\n";
        let u = parse_upgradable(out);
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].package, "libavcodec60");
        assert_eq!(u[0].installed, "7:6.1.1-3ubuntu5");
        assert_eq!(u[0].available, "7:6.1.1-3ubuntu5.1");
        assert_eq!(u[1].package, "libmpv2");
    }

    #[test]
    fn bundled_or_system() {
        let exe = Path::new("/opt/super-media-player/super-media-player");
        assert!(matches!(
            classify(Path::new("/opt/super-media-player/lib/libavcodec.so.60"), Some(exe)),
            Source::Bundled(_)
        ));
        assert!(matches!(
            classify(Path::new("/usr/lib/x86_64-linux-gnu/libavcodec.so.60"), Some(exe)),
            Source::System(_)
        ));
        // Built from source by scripts/build-engine.sh: apt can't update it.
        assert!(matches!(
            classify(
                Path::new("/home/u/media_player/.devlibs/engine/lib/libavcodec.so.62"),
                Some(Path::new("/home/u/media_player/target/debug/super-media-player"))
            ),
            Source::Bundled(_)
        ));
    }

    #[test]
    fn update_only_touches_ffmpeg_and_libmpv() {
        let c = update_command();
        assert!(c.starts_with("apt-get update -qq && apt-get install --only-upgrade -y"));
        assert!(c.contains("grep -E '^(libav|libsw|libpostproc|libmpv|ffmpeg)'"));
        assert_eq!(short_version("6.1.1-3ubuntu5+esm15"), "6.1.1");
    }
}

#[cfg(test)]
mod machine {
    /// `cargo test -p super-media-player engine_this_machine -- --ignored --nocapture`
    #[test]
    #[ignore = "uses this machine's libmpv and packages"]
    fn engine_this_machine() {
        let Ok(p) = player_core::Player::new(&player_core::PlayerConfig::headless(), |_| {}) else {
            eprintln!("no libmpv");
            return;
        };
        println!("ffmpeg: {:?}", p.get_string("ffmpeg-version"));
        let src = super::source();
        println!("source: {src:?}");
        if let super::Source::System(lib) = &src {
            println!("package: {:?}", super::installed_package(lib));
        }
        println!("pending: {:?}", super::pending_upgrades());
    }
}
