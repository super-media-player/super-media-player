//! One player window: files opened from the file manager while the player
//! runs go to it, instead of starting a second player.
//!
//! The running player listens on a Unix socket in `$XDG_RUNTIME_DIR`
//! (Windows: a TCP port on 127.0.0.1, written with a random secret to a file
//! only this user can read, so other users' programs can't send files). A
//! new start with files connects, sends them one per line, and exits.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const HELLO: &str = "SMP1 open";

/// This player owns the socket.
static LISTENING: AtomicBool = AtomicBool::new(false);

/// Where the running player listens.
#[cfg(unix)]
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

/// Windows: the running player's port and secret, in the config folder.
#[cfg(not(unix))]
fn port_file() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("instance.port"))
}

#[cfg(not(unix))]
pub fn forward(inputs: &[String]) -> bool {
    let Some(text) = port_file().and_then(|f| std::fs::read_to_string(f).ok()) else { return false };
    let Some((port, secret)) = text.trim().split_once(' ') else { return false };
    let Ok(port) = port.parse::<u16>() else { return false };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500)) else {
        return false;
    };
    let _ = s.set_write_timeout(Some(std::time::Duration::from_secs(2)));
    let cwd = std::env::current_dir().unwrap_or_default();
    s.write_all(format!("{secret}\n{}", message(inputs, &cwd)).as_bytes()).is_ok()
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
pub fn listen(on_open: impl Fn(Vec<String>) + Send + 'static) {
    let Some(file) = port_file() else { return };
    let listener = match std::net::TcpListener::bind(("127.0.0.1", 0)) {
        Ok(l) => l,
        Err(e) => {
            log::info!("single instance: can't listen: {e}");
            return;
        }
    };
    let Ok(port) = listener.local_addr().map(|a| a.port()) else { return };
    let mut bytes = [0u8; 16];
    if getrandom(&mut bytes).is_err() {
        return;
    }
    let secret: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if std::fs::write(&file, format!("{port} {secret}")).is_err() {
        return;
    }
    LISTENING.store(true, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut lines = BufReader::new(stream).lines();
                if lines.next().and_then(Result::ok).as_deref() != Some(secret.as_str()) {
                    continue;
                }
                let text: String = lines.take(10_000).map_while(Result::ok).map(|l| l + "\n").collect();
                if let Some(inputs) = parse(&text) {
                    on_open(inputs);
                }
            }
        })
        .ok();
}

/// Random bytes from the operating system's secure source (the secret of
/// the port file).
#[cfg(not(unix))]
fn getrandom(buf: &mut [u8]) -> Result<(), ()> {
    rustls::crypto::ring::default_provider().secure_random.fill(buf).map_err(|_| ())
}

/// Remove the socket at exit, so the next start doesn't try it.
pub fn stop_listening() {
    if LISTENING.load(Ordering::Relaxed) {
        #[cfg(unix)]
        let _ = std::fs::remove_file(socket_path());
        #[cfg(not(unix))]
        if let Some(f) = port_file() {
            let _ = std::fs::remove_file(f);
        }
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
