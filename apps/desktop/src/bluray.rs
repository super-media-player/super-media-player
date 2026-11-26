//! Which path to open a Blu-ray drive by, so that its decryption works.
//!
//! Discs with *bus encryption* (most studio Blu-rays) are scrambled once more
//! on their way from the drive to the computer. libaacs undoes that whenever
//! both the disc and the drive advertise it, but a drive can stop doing it:
//! MakeMKV switches LibreDrive-capable drives to raw reads, merely by listing
//! the drives, and they stay that way until the disc is ejected. libaacs then
//! unscrambles data that isn't scrambled, every unit comes out as garbage, and
//! mpv says "unrecognized file format" until the disc is taken out and put
//! back in.
//!
//! So before playing, one unit of the film is decrypted both ways. When only
//! the raw way works, the disc is opened through a folder of links to its
//! files: libaacs can't trace that back to the drive, and leaves the bus
//! encryption step out.
//!
//! The same sectors read differently in the two modes, so what Linux still has
//! cached from before is dropped first.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// What to give libbluray for the Blu-ray in `device`: the drive itself, or a
/// folder standing in for it. Takes a few seconds; run it off the UI thread.
#[cfg(target_os = "linux")]
pub fn source(device: &str) -> String {
    if !device.starts_with("/dev/") {
        return device.to_owned();
    }
    match linux::raw_source(device) {
        Some(folder) => {
            log::info!(
                "{device}: the drive reads without bus encryption; opening the disc through {}",
                folder.display()
            );
            folder.to_string_lossy().into_owned()
        }
        None => device.to_owned(),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn source(device: &str) -> String {
    device.to_owned()
}

/// Bytes in one AACS unit (three sectors).
const UNIT: usize = 6144;

/// Whether a unit of a Blu-ray stream is encrypted (its copy permission bits,
/// in the part that always stays readable).
fn is_encrypted(unit: &[u8]) -> bool {
    unit.len() == UNIT && unit[0] & 0xc0 != 0
}

/// Whether to leave the bus encryption step out, by how a unit decrypts with
/// it (through the drive) and without (through the folder). `None`: libaacs
/// couldn't open the disc that way.
fn needs_raw(through_drive: Option<bool>, through_folder: impl FnOnce() -> Option<bool>) -> bool {
    through_drive == Some(false) && through_folder() == Some(true)
}

/// Make `dir` a folder holding a link to each thing in `root`, and nothing else.
fn link_folder(root: &Path, dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for e in std::fs::read_dir(dir)? {
        let path = e?.path();
        // Only links are ever made here; anything else isn't ours.
        if path.is_symlink() {
            std::fs::remove_file(path)?;
        }
    }
    #[cfg(unix)]
    for e in std::fs::read_dir(root)? {
        let e = e?;
        std::os::unix::fs::symlink(e.path(), dir.join(e.file_name()))?;
    }
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}

/// The folder standing in for `device`.
fn stand_in(device: &str) -> PathBuf {
    let name = Path::new(device)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let base = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if Path::new(&d).is_dir() => PathBuf::from(d).join("super-media-player"),
        _ => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
            std::env::temp_dir().join(format!("super-media-player-{user}"))
        }
    };
    base.join(format!("bluray-{name}"))
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileExt;
    use std::path::{Path, PathBuf};

    use super::UNIT;

    unsafe extern "C" {
        fn posix_fadvise(fd: c_int, offset: i64, len: i64, advice: c_int) -> c_int;
    }
    const POSIX_FADV_DONTNEED: c_int = 4;

    /// Forget what the system has cached of a file (or a whole drive).
    fn drop_cache(path: &Path) {
        if let Ok(f) = std::fs::File::open(path) {
            // SAFETY: a plain system call on a descriptor we own; advisory only.
            unsafe { posix_fadvise(f.as_raw_fd(), 0, 0, POSIX_FADV_DONTNEED) };
        }
    }

    /// Streams small enough to be menus and logos; the film is far bigger.
    const SMALL: u64 = 50 << 20;

    /// The disc's big streams, the largest first.
    fn films(root: &Path) -> Vec<PathBuf> {
        let Ok(dir) = std::fs::read_dir(root.join("BDMV/STREAM")) else {
            return Vec::new();
        };
        let mut files: Vec<(u64, PathBuf)> = dir
            .flatten()
            .filter_map(|e| Some((e.metadata().ok()?.len(), e.path())))
            .filter(|(size, p)| {
                *size > SMALL
                    && p.extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case("m2ts"))
            })
            .collect();
        files.sort_by(|a, b| b.cmp(a));
        files.into_iter().map(|f| f.1).collect()
    }

    /// An encrypted unit of `film`, read from the disc itself.
    fn encrypted_unit(film: &Path) -> Option<Vec<u8>> {
        let f = std::fs::File::open(film).ok()?;
        [0u64, 1000, 20_000].into_iter().find_map(|n| {
            let mut unit = vec![0u8; UNIT];
            f.read_exact_at(&mut unit, n * UNIT as u64).ok()?;
            super::is_encrypted(&unit).then_some(unit)
        })
    }

    /// Files bigger than this aren't part of what opening a disc reads.
    const MAX_AHEAD: u64 = 2 << 20;

    /// Read the disc's small files (playlists, clip information, keys) into
    /// memory. libbluray opens hundreds of them one by one through the folder,
    /// each costing two long seeks, which takes a minute. The records of all
    /// the files first and then their contents, both in disc order: 3 s.
    fn read_ahead(root: &Path) {
        use std::os::unix::fs::DirEntryExt;
        let mut files: Vec<(u64, PathBuf)> = [
            "BDMV",
            "BDMV/PLAYLIST",
            "BDMV/CLIPINF",
            "BDMV/BDJO",
            "AACS",
            "CERTIFICATE",
        ]
        .iter()
        .filter_map(|d| std::fs::read_dir(root.join(d)).ok())
        .flat_map(|d| d.flatten().map(|e| (e.ino(), e.path())))
        .collect();
        files.sort();
        let small: Vec<PathBuf> = files
            .into_iter()
            .filter(|(_, p)| {
                std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() <= MAX_AHEAD)
            })
            .map(|f| f.1)
            .collect();
        for p in small {
            let _ = std::fs::read(p);
        }
    }

    type Open2 = unsafe extern "C" fn(*const c_char, *const c_char, *mut c_int) -> *mut c_void;
    type DecryptUnit = unsafe extern "C" fn(*mut c_void, *mut u8) -> c_int;
    type Close = unsafe extern "C" fn(*mut c_void);

    /// Whether libaacs can decrypt `unit` of the disc at `root`; `None` when
    /// it can't open the disc at all (no keys for it, say).
    fn decrypts(lib: &libloading::Library, root: &Path, unit: &[u8]) -> Option<bool> {
        let path = CString::new(root.as_os_str().as_bytes()).ok()?;
        let mut unit = unit.to_vec();
        // SAFETY: the signatures are libaacs's (aacs.h), the buffer holds one
        // whole unit, and the handle is closed exactly once.
        unsafe {
            let open = lib.get::<Open2>(b"aacs_open2\0").ok()?;
            let decrypt = lib.get::<DecryptUnit>(b"aacs_decrypt_unit\0").ok()?;
            let close = lib.get::<Close>(b"aacs_close\0").ok()?;
            let mut error = -1;
            let aacs = open(path.as_ptr(), std::ptr::null(), &mut error);
            if aacs.is_null() {
                return None;
            }
            let ok = (error == 0).then(|| decrypt(aacs, unit.as_mut_ptr()) == 1);
            close(aacs);
            ok
        }
    }

    /// The stand-in folder for the disc in `device`, when the drive reads it
    /// without bus encryption (see the module's comment).
    pub fn raw_source(device: &str) -> Option<PathBuf> {
        let root = crate::disc::mount_point(device, true)?;
        let films = films(&root);
        drop_cache(Path::new(device));
        for f in &films {
            drop_cache(f);
        }
        // Another decryption library was chosen (MakeMKV's): it handles this itself.
        if std::env::var_os("LIBAACS_PATH").is_some() {
            return None;
        }
        let unit = encrypted_unit(films.first()?)?;
        let lib = crate::disc::open_bundled_library(&["libaacs.so.0"])?;
        let folder = super::stand_in(device);
        if let Err(e) = super::link_folder(&root, &folder) {
            log::warn!("{}: {e}", folder.display());
            return None;
        }
        let drive = decrypts(&lib, &root, &unit);
        log::debug!("{device}: decrypts through the drive: {drive:?}");
        if !super::needs_raw(drive, || decrypts(&lib, &folder, &unit)) {
            return None;
        }
        read_ahead(&root);
        Some(folder)
    }

    #[cfg(test)]
    mod tests {
        /// `cargo test -p super-media-player bluray_this_machine -- --ignored --nocapture`
        #[test]
        #[ignore = "uses this machine's drive"]
        fn bluray_this_machine() {
            let device = std::env::var("SMP_TEST_BLURAY").unwrap_or("/dev/sr1".into());
            let start = std::time::Instant::now();
            println!(
                "{device} -> {} ({:?})",
                super::super::source(&device),
                start.elapsed()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_only_when_just_the_folder_works() {
        // Scrambled on the way, as it should be.
        assert!(!needs_raw(Some(true), || unreachable!()));
        // Raw mode.
        assert!(needs_raw(Some(false), || Some(true)));
        // Wrong keys, or no keys: the folder is no better.
        assert!(!needs_raw(Some(false), || Some(false)));
        assert!(!needs_raw(Some(false), || None));
        assert!(!needs_raw(None, || unreachable!()));
    }

    #[test]
    fn encrypted_units_say_so() {
        let mut unit = vec![0u8; UNIT];
        assert!(!is_encrypted(&unit));
        unit[0] = 0xc5;
        assert!(is_encrypted(&unit));
        assert!(!is_encrypted(&unit[..100]));
    }

    #[test]
    fn folders_and_images_stay_as_they_are() {
        assert_eq!(source("/home/you/film.iso"), "/home/you/film.iso");
        assert_eq!(source(""), "");
    }

    #[cfg(unix)]
    #[test]
    fn link_folder_follows_the_disc() {
        let base = std::env::temp_dir().join(format!("smp-bluray-{}", std::process::id()));
        let (a, b, dir) = (base.join("a"), base.join("b"), base.join("links"));
        for d in [a.join("BDMV"), a.join("AACS"), b.join("BDMV")] {
            std::fs::create_dir_all(d).unwrap();
        }
        link_folder(&a, &dir).unwrap();
        assert!(dir.join("AACS").is_dir());
        // Another disc: nothing is left of the first, and the discs are untouched.
        link_folder(&b, &dir).unwrap();
        assert!(!dir.join("AACS").exists() && a.join("AACS").is_dir());
        assert_eq!(
            std::fs::read_link(dir.join("BDMV")).unwrap(),
            b.join("BDMV")
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
