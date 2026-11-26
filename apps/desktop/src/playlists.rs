//! Playlists: save and open M3U / PLS / XSPF files, search the playlist,
//! sort it, move entries, remove duplicates and missing files.
//!
//! mpv owns the playlist; this module reorders it with `playlist-move`
//! and reads it back from the `playlist` property.

use std::path::Path;
use std::rc::Rc;

use player_core::Player;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::AppWindow;

/// Extensions read as playlists.
pub const EXTENSIONS: &[&str] = &["m3u", "m3u8", "pls", "xspf"];

pub fn is_playlist(p: &Path) -> bool {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| EXTENSIONS.contains(&e.as_str()))
}

/// One entry of mpv's playlist.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub file: String,
    pub title: Option<String>,
}

fn entries(p: &Player) -> Vec<Entry> {
    p.get_value("playlist")
        .unwrap_or_default()
        .as_array()
        .iter()
        .filter_map(|e| {
            Some(Entry {
                file: e.str_field("filename")?,
                title: e.str_field("title").filter(|t| !t.is_empty()),
            })
        })
        .collect()
}

// ---- reading ----

/// The entries of a playlist file, paths made absolute against its folder.
pub fn read(path: &Path) -> std::io::Result<Vec<String>> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    let base = path.parent().unwrap_or(Path::new("/"));
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    Ok(match ext.as_str() {
        "xspf" => parse_xspf(text, base),
        "pls" => parse_pls(text, base),
        _ => parse_m3u(text, base),
    })
}

fn resolve(item: &str, base: &Path) -> String {
    let item = item.trim();
    if let Some(rest) = item.strip_prefix("file://") {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        return crate::percent_decode(rest);
    }
    if item.contains("://") {
        return item.to_owned();
    }
    // Windows-made playlists: backslashes.
    let item = item.replace('\\', "/");
    let p = Path::new(&item);
    if p.is_absolute() {
        item
    } else {
        base.join(p).to_string_lossy().into_owned()
    }
}

pub fn parse_m3u(text: &str, base: &Path) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| resolve(l, base))
        .collect()
}

pub fn parse_pls(text: &str, base: &Path) -> Vec<String> {
    let mut items: Vec<(u32, String)> = text
        .lines()
        .filter_map(|l| {
            let (k, v) = l.trim().split_once('=')?;
            let n = k.trim().strip_prefix("File")?.parse().ok()?;
            Some((n, resolve(v, base)))
        })
        .collect();
    items.sort_by_key(|i| i.0);
    items.into_iter().map(|i| i.1).collect()
}

pub fn parse_xspf(text: &str, base: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("<location>") {
        rest = &rest[i + "<location>".len()..];
        let Some(end) = rest.find("</location>") else {
            break;
        };
        out.push(resolve(&xml_unescape(&rest[..end]), base));
        rest = &rest[end..];
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---- writing ----

pub fn write_m3u(entries: &[Entry]) -> String {
    let mut out = String::from("#EXTM3U\n");
    for e in entries {
        if let Some(t) = &e.title {
            out.push_str(&format!("#EXTINF:-1,{}\n", t.replace('\n', " ")));
        }
        out.push_str(&e.file);
        out.push('\n');
    }
    out
}

fn uri(file: &str) -> String {
    if file.contains("://") {
        return file.to_owned();
    }
    let mut out = String::from("file://");
    for b in file.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn write_xspf(entries: &[Entry]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<playlist version=\"1\" xmlns=\"http://xspf.org/ns/0/\">\n  <trackList>\n",
    );
    for e in entries {
        out.push_str("    <track>\n");
        out.push_str(&format!(
            "      <location>{}</location>\n",
            xml_escape(&uri(&e.file))
        ));
        if let Some(t) = &e.title {
            out.push_str(&format!("      <title>{}</title>\n", xml_escape(t)));
        }
        out.push_str("    </track>\n");
    }
    out.push_str("  </trackList>\n</playlist>\n");
    out
}

// ---- reordering ----

fn display_name(e: &Entry) -> String {
    e.title.clone().unwrap_or_else(|| {
        Path::new(&e.file)
            .file_name()
            .map_or_else(|| e.file.clone(), |n| n.to_string_lossy().into_owned())
    })
}

/// The order (old indices) for a sort: "title", "file", "folder", "reverse".
pub fn order(entries: &[Entry], how: &str) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..entries.len()).collect();
    match how {
        "title" => idx.sort_by(|&a, &b| {
            crate::natural_cmp(
                &display_name(&entries[a]).to_lowercase(),
                &display_name(&entries[b]).to_lowercase(),
            )
        }),
        "file" => idx.sort_by(|&a, &b| {
            let name = |e: &Entry| {
                Path::new(&e.file)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
            };
            crate::natural_cmp(
                &name(&entries[a]).unwrap_or_default(),
                &name(&entries[b]).unwrap_or_default(),
            )
        }),
        "folder" => idx.sort_by(|&a, &b| crate::natural_cmp(&entries[a].file, &entries[b].file)),
        "reverse" => idx.reverse(),
        _ => {}
    }
    idx
}

/// `playlist-move` steps that turn the playlist into `order`.
pub fn moves(order: &[usize]) -> Vec<(usize, usize)> {
    let mut cur: Vec<usize> = (0..order.len()).collect();
    let mut out = Vec::new();
    for (target, want) in order.iter().enumerate() {
        let from = cur.iter().position(|x| x == want).unwrap_or(target);
        if from != target {
            out.push((from, target));
            let v = cur.remove(from);
            cur.insert(target, v);
        }
    }
    out
}

/// Indices of entries already seen earlier in the list.
pub fn duplicates(entries: &[Entry]) -> Vec<usize> {
    let mut seen = std::collections::HashSet::new();
    (0..entries.len())
        .filter(|&i| !seen.insert(entries[i].file.clone()))
        .collect()
}

fn missing(entries: &[Entry]) -> Vec<usize> {
    (0..entries.len())
        .filter(|&i| {
            let f = &entries[i].file;
            !f.contains("://") && !Path::new(f).exists()
        })
        .collect()
}

fn remove(p: &Player, mut idx: Vec<usize>) -> usize {
    idx.sort_unstable();
    for &i in idx.iter().rev() {
        let _ = p.command(&["playlist-remove", &i.to_string()]);
    }
    idx.len()
}

// ---- UI ----

pub fn install(ui: &AppWindow, player: &Rc<Player>) {
    let (p, weak) = (player.clone(), ui.as_weak());
    ui.on_playlist_action(move |action, i| {
        let Some(ui) = weak.upgrade() else { return };
        act(&ui, &p, action.as_str(), i);
    });
    let weak = ui.as_weak();
    ui.on_playlist_search(move |q| {
        if let Some(ui) = weak.upgrade() {
            ui.set_playlist_query(q);
            refresh_filter(&ui);
        }
    });
}

fn act(ui: &AppWindow, p: &Player, action: &str, i: i32) {
    let list = entries(p);
    let n = list.len();
    let toast = |t: String| ui.set_toast(t.into());
    match action {
        "up" if i > 0 && (i as usize) < n => {
            let _ = p.command(&["playlist-move", &i.to_string(), &(i - 1).to_string()]);
        }
        "down" if i >= 0 && (i as usize) + 1 < n => {
            let _ = p.command(&["playlist-move", &i.to_string(), &(i + 2).to_string()]);
        }
        "title" | "file" | "folder" | "reverse" => {
            for (from, to) in moves(&order(&list, action)) {
                let _ = p.command(&["playlist-move", &from.to_string(), &to.to_string()]);
            }
            toast(match action {
                "reverse" => "Playlist reversed".into(),
                "folder" => "Sorted by folder and file".into(),
                "file" => "Sorted by file name".into(),
                _ => "Sorted by title".into(),
            });
        }
        "shuffle" => {
            let _ = p.command(&["playlist-shuffle"]);
            toast("Playlist shuffled".into());
        }
        "dedupe" => {
            let k = remove(p, duplicates(&list));
            toast(if k == 0 {
                "No duplicates".into()
            } else {
                format!("Removed {k} duplicate(s)")
            });
        }
        "missing" => {
            let k = remove(p, missing(&list));
            toast(if k == 0 {
                "Every file is there".into()
            } else {
                format!("Removed {k} missing file(s)")
            });
        }
        "save" => save(ui, &list),
        "open" => open(ui),
        _ => {}
    }
}

fn save(ui: &AppWindow, list: &[Entry]) {
    if list.is_empty() {
        ui.set_toast("The playlist is empty".into());
        return;
    }
    let Some(mut path) = rfd::FileDialog::new()
        .set_title("Save playlist")
        .set_file_name("playlist.m3u8")
        .add_filter("M3U playlist", &["m3u8", "m3u"])
        .add_filter("XSPF playlist", &["xspf"])
        .save_file()
    else {
        return;
    };
    if path.extension().is_none() {
        path.set_extension("m3u8");
    }
    let xspf = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xspf"));
    let text = if xspf {
        write_xspf(list)
    } else {
        write_m3u(list)
    };
    ui.set_toast(
        match std::fs::write(&path, text) {
            Ok(()) => format!("Saved {} items to {}", list.len(), path.display()),
            Err(e) => format!("Couldn't save the playlist: {e}"),
        }
        .into(),
    );
}

fn open(ui: &AppWindow) {
    if let Some(path) = rfd::FileDialog::new()
        .set_title("Open playlist")
        .add_filter("Playlists", EXTENSIONS)
        .pick_file()
    {
        crate::open_inputs(ui, vec![crate::Input::Path(path)], false);
    }
}

/// Which rows match the search (empty: all).
pub fn refresh_filter(ui: &AppWindow) {
    let q = ui.get_playlist_query().to_lowercase();
    let q = q.trim();
    if q.is_empty() {
        ui.set_playlist_shown(ModelRc::new(VecModel::from(Vec::<bool>::new())));
        return;
    }
    let list = ui.get_playlist();
    let shown: Vec<bool> = (0..list.row_count())
        .map(|i| {
            list.row_data(i).is_some_and(|r| {
                let hay = format!("{} {}", r.title, r.detail).to_lowercase();
                q.split_whitespace().all(|w| hay.contains(w))
            })
        })
        .collect();
    ui.set_playlist_shown(ModelRc::new(VecModel::from(shown)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3u_and_pls_and_xspf() {
        let base = Path::new("/music");
        let m3u = "#EXTM3U\n#EXTINF:123,Song\nalbum/01.flac\n/abs/x.mp3\nhttps://radio/x\nfile:///a%20b.ogg\n";
        assert_eq!(
            parse_m3u(m3u, base),
            [
                "/music/album/01.flac",
                "/abs/x.mp3",
                "https://radio/x",
                "/a b.ogg"
            ]
        );
        let pls = "[playlist]\nFile2=b.mp3\nFile1=a.mp3\nTitle1=A\nNumberOfEntries=2\n";
        assert_eq!(parse_pls(pls, base), ["/music/a.mp3", "/music/b.mp3"]);
        let list = vec![
            Entry {
                file: "/m/a & b.mp3".into(),
                title: Some("A <1>".into()),
            },
            Entry {
                file: "https://x/y".into(),
                title: None,
            },
        ];
        let x = write_xspf(&list);
        assert_eq!(parse_xspf(&x, base), ["/m/a & b.mp3", "https://x/y"]);
        assert_eq!(
            parse_m3u(&write_m3u(&list), base),
            ["/m/a & b.mp3", "https://x/y"]
        );
    }

    #[test]
    fn moves_reach_the_order() {
        for order in [
            vec![2, 0, 1],
            vec![3, 2, 1, 0],
            vec![0, 1, 2],
            vec![1, 3, 0, 2],
        ] {
            let mut list: Vec<usize> = (0..order.len()).collect();
            for (from, to) in moves(&order) {
                let v = list.remove(from);
                list.insert(to, v);
            }
            assert_eq!(list, order);
        }
    }

    #[test]
    fn sorts_and_duplicates() {
        let e = |f: &str| Entry {
            file: f.into(),
            title: None,
        };
        let list = vec![e("/b/track10.mp3"), e("/a/track2.mp3"), e("/b/track10.mp3")];
        assert_eq!(order(&list, "file"), [1, 0, 2]);
        assert_eq!(order(&list, "folder"), [1, 0, 2]);
        assert_eq!(duplicates(&list), [2]);
    }
}
