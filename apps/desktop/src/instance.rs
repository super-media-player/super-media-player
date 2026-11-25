//! One player window: files opened from the file manager while the player
//! runs go to it, instead of starting a second player.
//!
//! The running player listens on a Unix socket in `$XDG_RUNTIME_DIR`. A new
//! start with files connects, sends them one per line, and exits.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const HELLO: &str = "SMP1 open";

/// This player owns the socket.
static LISTENING: AtomicBool = AtomicBool::new(false);

/// Where the running player listens.
pub fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if Path::new(&dir).is_dir() => PathBuf::from(dir).join("super-media-player.sock"),
        _ => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
            std::env::temp_dir().join(format!("super-media-player-{user}.sock"))
        }
    }
}

/// The message for `inputs`: paths made absolute, so the running player,
/// with another working folder, finds them.
pub fn message(inputs: &[String], cwd: &Path) -> String {
    let mut msg = format!("{HELLO}\n");
    for i in inputs {
        let i = i.trim();
        if i.is_empty() || i.contains('\n') {
            continue;
        }
        if i.contains("://") || Path::new(i).is_absolute() || i.starts_with('~') {
            msg.push_str(i);
        } else {
            msg.push_str(&cwd.join(i).to_string_lossy());
        }
        msg.push('\n');
    }
    msg
}

/// Parse a message; None when it isn't one.
pub fn parse(msg: &str) -> Option<Vec<String>> {
    let mut lines = msg.lines();
    (lines.next()? == HELLO).then(|| lines.filter(|l| !l.trim().is_empty()).map(String::from).collect())
}

/// Hand `inputs` to a player that is already running. True when one took them.
#[cfg(unix)]
pub fn forward(inputs: &[String]) -> bool {
    use std::os::unix::net::UnixStream;
    let Ok(mut s) = UnixStream::connect(socket_path()) else { return false };
    let _ = s.set_write_timeout(Some(std::time::Duration::from_secs(2)));
    let cwd = std::env::current_dir().unwrap_or_default();
    s.write_all(message(inputs, &cwd).as_bytes()).is_ok()
}

#[cfg(not(unix))]
pub fn forward(_inputs: &[String]) -> bool {
    false
}

/// Listen for files sent by later starts; `on_open` runs on a worker thread.
#[cfg(unix)]
pub fn listen(on_open: impl Fn(Vec<String>) + Send + 'static) {
    use std::os::unix::net::{UnixListener, UnixStream};
    let path = socket_path();
    // A socket file nobody answers on is left from a crash: replace it.
    if path.exists() && UnixStream::connect(&path).is_err() {
        let _ = std::fs::remove_file(&path);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            log::info!("single instance: can't listen on {}: {e}", path.display());
            return;
        }
    };
    LISTENING.store(true, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut text = String::new();
                for line in BufReader::new(stream).lines().take(10_000) {
                    match line {
                        Ok(l) => {
                            text.push_str(&l);
                            text.push('\n');
                        }
                        Err(_) => break,
                    }
                }
                if let Some(inputs) = parse(&text) {
                    on_open(inputs);
                }
            }
        })
        .ok();
}

#[cfg(not(unix))]
pub fn listen(_on_open: impl Fn(Vec<String>) + Send + 'static) {}

/// Remove the socket at exit, so the next start doesn't try it.
pub fn stop_listening() {
    if LISTENING.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(socket_path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_makes_paths_absolute() {
        let msg = message(
            &["song.mp3".into(), "/abs/film.mkv".into(), "https://x.y/v".into(), "".into()],
            Path::new("/home/me"),
        );
        assert_eq!(
            parse(&msg).unwrap(),
            vec!["/home/me/song.mp3", "/abs/film.mkv", "https://x.y/v"]
        );
        assert_eq!(parse("hello\n/x"), None);
    }
}
