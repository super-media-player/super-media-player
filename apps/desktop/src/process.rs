//! Starting other programs (ffmpeg, yt-dlp, ...), and other places where
//! Windows needs its own handling.
//!
//! On Windows the player is a window app, and each console program it
//! starts would flash its own console window. `command` asks Windows not
//! to make one.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `Command::new(program)`, without a console window on Windows.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// `path` in full. Elsewhere symlinks are resolved too; on Windows that
/// would give the `\\?\C:\...` form, which people don't recognise and
/// some programs don't accept.
pub fn full_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    let full = std::path::absolute(path);
    #[cfg(not(windows))]
    let full = std::fs::canonicalize(path);
    full.unwrap_or_else(|_| path.to_path_buf())
}

/// Files copied in the file manager (Ctrl+C in Explorer). Windows keeps
/// them as a file list, not as text; other systems give text, which the
/// open-location dialog reads.
#[cfg(windows)]
pub fn clipboard_files() -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::UI::Shell::DragQueryFileW;
    const CF_HDROP: u32 = 15;
    let mut out = Vec::new();
    // SAFETY: the list is only read while the clipboard is open, and each
    // name is copied into a buffer of the length Windows reported.
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP) == 0 || OpenClipboard(std::ptr::null_mut()) == 0 {
            return out;
        }
        let list = GetClipboardData(CF_HDROP);
        if !list.is_null() {
            let count = DragQueryFileW(list, u32::MAX, std::ptr::null_mut(), 0);
            for i in 0..count {
                let len = DragQueryFileW(list, i, std::ptr::null_mut(), 0);
                let mut name = vec![0u16; len as usize + 1];
                let got = DragQueryFileW(list, i, name.as_mut_ptr(), name.len() as u32);
                name.truncate(got as usize);
                if !name.is_empty() {
                    out.push(PathBuf::from(std::ffi::OsString::from_wide(&name)));
                }
            }
        }
        CloseClipboard();
    }
    out
}

#[cfg(not(windows))]
pub fn clipboard_files() -> Vec<PathBuf> {
    Vec::new()
}

/// Whether the player runs as administrator. Windows then blocks files
/// dragged from Explorer (a normal-user program) onto its windows.
#[cfg(windows)]
pub fn elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: plain Win32 calls on our own process token, closed after use.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut info = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut info).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && info.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn elevated() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// Every program the player starts goes through `command`: a plain
    /// `Command::new` would flash a console window on Windows.
    #[test]
    fn all_programs_start_without_a_console() {
        fn scan(dir: &Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    scan(&path, found);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && !path.ends_with("process.rs")
                {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for (n, line) in text.lines().enumerate() {
                        if line.contains("Command::new(") {
                            found.push(format!("{}:{}", path.display(), n + 1));
                        }
                    }
                }
            }
        }
        let mut found = Vec::new();
        scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut found);
        assert!(found.is_empty(), "use crate::process::command at {found:?}");
    }
}
