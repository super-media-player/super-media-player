//! The Library: links you saved (radio, audio, video), the internet radio
//! directory, and podcasts.
//!
//! - Saved links: `links.tsv` in the config folder (group, name, URL).
//! - Radio: radio-browser.info, a free community database of ~50,000
//!   stations with an open API meant for players.
//! - Podcasts: found with Apple's free podcast search; episodes come from
//!   each show's own RSS feed. Subscriptions: `podcasts.tsv`.
//!
//! Network calls run on worker threads; the UI only gets the results.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use player_core::Player;
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{AppWindow, LibraryRow, network, settings};

const AGENT: &str = concat!("SuperMediaPlayer/", env!("CARGO_PKG_VERSION"));
/// Saved link groups, as the Saved tab shows them.
pub const GROUPS: [&str; 3] = ["radio", "audio", "video"];
/// radio-browser.info mirrors, tried in turn.
const RADIO_SERVERS: [&str; 3] = [
    "https://de1.api.radio-browser.info",
    "https://nl1.api.radio-browser.info",
    "https://at1.api.radio-browser.info",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub group: String,
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Station {
    pub uuid: String,
    pub name: String,
    pub url: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Show {
    pub name: String,
    pub feed: String,
    pub author: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Episode {
    pub title: String,
    pub url: String,
    pub detail: String,
    /// The feed it came from.
    pub show: String,
}

// ---- saved links ----

fn links_file() -> Option<PathBuf> {
    settings::config_dir().map(|d| d.join("links.tsv"))
}

fn podcasts_file() -> Option<PathBuf> {
    settings::config_dir().map(|d| d.join("podcasts.tsv"))
}

fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ").trim().to_owned()
}

pub fn parse_links(text: &str) -> Vec<Link> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let group = it.next()?.to_owned();
            let name = it.next()?.to_owned();
            let url = it.next()?.trim().to_owned();
            (GROUPS.contains(&group.as_str()) && !url.is_empty()).then_some(Link {
                group,
                name,
                url,
            })
        })
        .collect()
}

fn load_links() -> Vec<Link> {
    links_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|t| parse_links(&t))
        .unwrap_or_default()
}

fn save_links(list: &[Link]) {
    let Some(f) = links_file() else { return };
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let text: String = list
        .iter()
        .map(|l| format!("{}\t{}\t{}\n", l.group, clean(&l.name), clean(&l.url)))
        .collect();
    let _ = std::fs::write(f, text);
}

/// "kissfm.chisinau" from "https://live.kissfm.ro/kissfm.chisinau".
pub fn name_from_url(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |x| x.1);
    let rest = rest
        .split(['?', '#'])
        .next()
        .unwrap_or(rest)
        .trim_end_matches('/');
    let last = rest.rsplit('/').next().unwrap_or(rest);
    if last.is_empty() {
        rest.to_owned()
    } else {
        last.to_owned()
    }
}

/// What a URL most likely is, for the group to save it in.
pub fn guess_group(url: &str, has_video: bool) -> &'static str {
    let u = url.to_lowercase();
    if has_video
        || [
            "youtube.", "youtu.be", "vimeo.", "twitch.", ".mp4", ".mkv", ".webm",
        ]
        .iter()
        .any(|k| u.contains(k))
    {
        "video"
    } else if [
        ".mp3",
        ".flac",
        ".m4a",
        ".ogg",
        ".opus",
        "soundcloud.",
        "bandcamp.",
    ]
    .iter()
    .any(|k| u.contains(k))
    {
        "audio"
    } else {
        "radio"
    }
}

// ---- radio-browser.info ----

fn get_json(url: &str) -> Result<Value, String> {
    let mut resp = ureq::get(url)
        .header("User-Agent", AGENT)
        .config()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .call()
        .map_err(|e| e.to_string())?;
    let body = resp
        .body_mut()
        .with_config()
        .limit(32 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                o.push(b as char)
            }
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// Stations by name (and / or genre tag) in a country ("MD", "" for all),
/// most popular first.
pub fn search_stations(text: &str, country: &str) -> Result<Vec<Station>, String> {
    let mut q = String::from("hidebroken=true&order=clickcount&reverse=true&limit=200");
    let text = text.trim();
    if !text.is_empty() {
        q += &format!("&name={}", enc(text));
    }
    let cc = country.trim().to_uppercase();
    if cc.len() == 2 {
        q += &format!("&countrycode={cc}");
    }
    let mut last = String::new();
    for server in RADIO_SERVERS {
        match get_json(&format!("{server}/json/stations/search?{q}")) {
            Ok(v) => {
                let mut list = stations_of(&v);
                // Nothing by name: the words may be a genre ("jazz").
                if list.is_empty() && !text.is_empty() {
                    let q2 = q.replace(
                        &format!("&name={}", enc(text)),
                        &format!("&tag={}", enc(&text.to_lowercase())),
                    );
                    if let Ok(v) = get_json(&format!("{server}/json/stations/search?{q2}")) {
                        list = stations_of(&v);
                    }
                }
                return Ok(list);
            }
            Err(e) => last = e,
        }
    }
    Err(format!("The radio directory couldn't be reached ({last})"))
}

pub fn stations_of(v: &Value) -> Vec<Station> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let url = s["url_resolved"]
                        .as_str()
                        .filter(|u| !u.is_empty())
                        .or(s["url"].as_str())?
                        .to_owned();
                    let name = s["name"].as_str()?.trim().to_owned();
                    let mut d = Vec::new();
                    if let Some(c) = s["country"].as_str().filter(|c| !c.is_empty()) {
                        d.push(c.to_owned());
                    }
                    let tags: Vec<&str> = s["tags"]
                        .as_str()
                        .unwrap_or("")
                        .split(',')
                        .filter(|t| !t.is_empty())
                        .take(3)
                        .collect();
                    if !tags.is_empty() {
                        d.push(tags.join(", "));
                    }
                    match (s["codec"].as_str(), s["bitrate"].as_i64()) {
                        (Some(c), Some(b)) if b > 0 => d.push(format!("{c} {b} kbit/s")),
                        (Some(c), _) if !c.is_empty() => d.push(c.to_owned()),
                        _ => {}
                    }
                    Some(Station {
                        uuid: s["stationuuid"].as_str().unwrap_or("").to_owned(),
                        name,
                        url,
                        detail: d.join(" · "),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Tell the directory a station was played (it ranks stations by this;
/// the API asks players to).
fn count_click(uuid: String) {
    if uuid.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let _ = get_json(&format!("{}/json/url/{uuid}", RADIO_SERVERS[0]));
    });
}

// ---- podcasts ----

pub fn search_podcasts(text: &str) -> Result<Vec<Show>, String> {
    let v = get_json(&format!(
        "https://itunes.apple.com/search?media=podcast&limit=50&term={}",
        enc(text.trim())
    ))
    .map_err(|e| format!("Podcast search couldn't be reached ({e})"))?;
    Ok(v["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    Some(Show {
                        name: r["collectionName"].as_str()?.to_owned(),
                        feed: r["feedUrl"].as_str()?.to_owned(),
                        author: r["artistName"].as_str().unwrap_or("").to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Episodes from a feed, newest first as the feed lists them.
pub fn episodes(feed: &str) -> Result<Vec<Episode>, String> {
    let mut resp = ureq::get(feed)
        .header("User-Agent", AGENT)
        .config()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .call()
        .map_err(|e| format!("The podcast feed couldn't be loaded ({e})"))?;
    let xml = resp
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let mut list = parse_feed(&xml)?;
    list.iter_mut().for_each(|e| feed.clone_into(&mut e.show));
    Ok(list)
}

pub fn parse_feed(xml: &str) -> Result<Vec<Episode>, String> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| format!("The podcast feed isn't valid ({e})"))?;
    let child_text = |n: roxmltree::Node, name: &str| {
        n.children()
            .find(|c| c.tag_name().name() == name)
            .and_then(|c| c.text())
            .map(|t| t.trim().to_owned())
    };
    let mut out = Vec::new();
    for item in doc.descendants().filter(|n| n.has_tag_name("item")) {
        let Some(url) = item
            .children()
            .find(|c| c.tag_name().name() == "enclosure")
            .and_then(|e| e.attribute("url"))
            .map(String::from)
        else {
            continue;
        };
        let title = child_text(item, "title").unwrap_or_else(|| name_from_url(&url));
        let mut d = Vec::new();
        if let Some(date) = child_text(item, "pubDate") {
            // "Tue, 18 Nov 2026 10:00:00 GMT" → "18 Nov 2026"
            let words: Vec<&str> = date.split_whitespace().collect();
            d.push(if words.len() >= 4 {
                words[1..4].join(" ")
            } else {
                date
            });
        }
        if let Some(dur) = child_text(item, "duration").filter(|s| !s.is_empty()) {
            d.push(fmt_duration(&dur));
        }
        out.push(Episode {
            title,
            url,
            detail: d.join(" · "),
            show: String::new(),
        });
    }
    Ok(out)
}

/// itunes:duration is seconds or h:mm:ss.
fn fmt_duration(d: &str) -> String {
    match d.parse::<u64>() {
        Ok(s) => crate::fmt_clock(s as f64),
        Err(_) => d.to_owned(),
    }
}

fn load_shows() -> Vec<Show> {
    let text = podcasts_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            Some(Show {
                name: it.next()?.to_owned(),
                author: it.next()?.to_owned(),
                feed: it.next()?.trim().to_owned(),
            })
        })
        .filter(|s| !s.feed.is_empty())
        .collect()
}

fn save_shows(list: &[Show]) {
    let Some(f) = podcasts_file() else { return };
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let text: String = list
        .iter()
        .map(|s| {
            format!(
                "{}\t{}\t{}\n",
                clean(&s.name),
                clean(&s.author),
                clean(&s.feed)
            )
        })
        .collect();
    let _ = std::fs::write(f, text);
}

/// The country part of the locale: "ro_MD.UTF-8" → "MD".
fn country_from_locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find_map(|l| {
            let region = l
                .split(['.', '@'])
                .next()?
                .split('_')
                .nth(1)?
                .to_uppercase();
            (region.len() == 2).then_some(region)
        })
        .unwrap_or_default()
}

// ---- UI ----

/// What the list in the Library shows now.
#[derive(Clone)]
enum Rows {
    Links,
    Stations(Vec<Station>),
    Shows(Vec<Show>),
    Episodes(Vec<Episode>),
    Net(Vec<Net>),
}

/// A row of the Network tab.
#[derive(Clone, Debug)]
enum Net {
    Server(network::Server),
    Share {
        uri: String,
        user: String,
    },
    Folder {
        control: String,
        id: String,
        title: String,
        count: Option<u32>,
    },
    Item {
        url: String,
        title: String,
        detail: String,
    },
    Dir(PathBuf),
    File(PathBuf),
}

thread_local! {
    /// The Network tab's folders above the one shown (for Back).
    static NET_STACK: RefCell<Vec<Vec<Net>>> = const { RefCell::new(Vec::new()) };
    static SERVERS: RefCell<Vec<network::Server>> = const { RefCell::new(Vec::new()) };
    static SHARES: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    static LINKS: RefCell<Vec<Link>> = const { RefCell::new(Vec::new()) };
    static SHOWS: RefCell<Vec<Show>> = const { RefCell::new(Vec::new()) };
    static ROWS: RefCell<Option<Rows>> = const { RefCell::new(None) };
}

fn row(title: &str, detail: &str, primary: &str, secondary: &str) -> LibraryRow {
    LibraryRow {
        title: title.into(),
        detail: detail.into(),
        primary: primary.into(),
        secondary: secondary.into(),
    }
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn net_row(n: &Net) -> LibraryRow {
    match n {
        Net::Server(s) => row(
            &s.name,
            "Media server (DLNA / UPnP) on your network",
            "Open",
            "",
        ),
        Net::Share { uri, user } => row(
            uri,
            &if user.is_empty() {
                "Saved share".to_string()
            } else {
                format!("Saved share · {user}")
            },
            "Connect",
            "Forget",
        ),
        Net::Folder { title, count, .. } => row(
            title,
            &count
                .map(|c| format!("{c} items"))
                .unwrap_or_else(|| "Folder".into()),
            "Open",
            "Play all",
        ),
        Net::Item { title, detail, .. } => row(title, detail, "Play", "Add"),
        Net::Dir(p) => row(&file_name(p), "Folder", "Open", "Play all"),
        Net::File(p) => row(&file_name(p), "", "Play", "Add"),
    }
}

fn show_rows(ui: &AppWindow, rows: Rows) {
    let saved_urls: Vec<String> = LINKS.with_borrow(|l| l.iter().map(|x| x.url.clone()).collect());
    let saved_feeds: Vec<String> =
        SHOWS.with_borrow(|l| l.iter().map(|x| x.feed.clone()).collect());
    let group = GROUPS
        .get(ui.get_library_group().max(0) as usize)
        .copied()
        .unwrap_or("radio");
    let list: Vec<LibraryRow> = match &rows {
        Rows::Links => LINKS.with_borrow(|l| {
            l.iter()
                .filter(|x| x.group == group)
                .map(|x| row(&x.name, &x.url, "Play", "Remove"))
                .collect()
        }),
        Rows::Stations(s) => s
            .iter()
            .map(|x| {
                row(
                    &x.name,
                    &x.detail,
                    "Play",
                    if saved_urls.contains(&x.url) {
                        "★ Saved"
                    } else {
                        "☆ Save"
                    },
                )
            })
            .collect(),
        Rows::Shows(s) => s
            .iter()
            .map(|x| {
                row(
                    &x.name,
                    &x.author,
                    "Episodes",
                    if saved_feeds.contains(&x.feed) {
                        "Unsubscribe"
                    } else {
                        "Subscribe"
                    },
                )
            })
            .collect(),
        Rows::Episodes(e) => e
            .iter()
            .map(|x| row(&x.title, &x.detail, "Play", "Add"))
            .collect(),
        Rows::Net(n) => n.iter().map(net_row).collect(),
    };
    ui.set_library_rows(ModelRc::new(VecModel::from(list)));
    let can_back = matches!(rows, Rows::Episodes(_))
        || (matches!(rows, Rows::Net(_)) && NET_STACK.with_borrow(|s| !s.is_empty()));
    ui.set_library_episodes(can_back);
    ROWS.with_borrow_mut(|r| *r = Some(rows));
}

/// The saved links of the chosen group, in list order.
fn group_links(ui: &AppWindow) -> Vec<Link> {
    let group = GROUPS
        .get(ui.get_library_group().max(0) as usize)
        .copied()
        .unwrap_or("radio");
    LINKS.with_borrow(|l| l.iter().filter(|x| x.group == group).cloned().collect())
}

fn play(ui: &AppWindow, url: &str, append: bool) {
    if let Some(input) = crate::parse_input(url) {
        crate::open_inputs(ui, vec![input], append);
    }
}

pub fn install(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<settings::Settings>>) {
    LINKS.with_borrow_mut(|l| *l = load_links());
    SHOWS.with_borrow_mut(|l| *l = load_shows());
    let saved = settings.borrow().radio_country.clone();
    ui.set_library_country(
        if saved.is_empty() {
            country_from_locale()
        } else {
            saved
        }
        .into(),
    );

    // Switching tabs (0 saved, 1 radio, 2 podcasts) or saved groups.
    let weak = ui.as_weak();
    ui.on_library_show(move || {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_library_status(SharedString::new());
        match ui.get_library_tab() {
            0 => show_rows(&ui, Rows::Links),
            1 => {
                // Opening the radio tab: the country's most played stations.
                let had = ROWS.with_borrow(|r| matches!(r, Some(Rows::Stations(_))));
                if !had {
                    ui.invoke_library_search(SharedString::new());
                }
            }
            2 => show_rows(&ui, Rows::Shows(SHOWS.with_borrow(|s| s.clone()))),
            _ => {
                NET_STACK.with_borrow_mut(|s| s.clear());
                show_rows(&ui, Rows::Net(net_home()));
                if SERVERS.with_borrow(|s| s.is_empty()) {
                    ui.invoke_library_rescan();
                }
            }
        }
    });
    install_network(ui);

    let weak = ui.as_weak();
    ui.on_library_search(move |text| {
        let Some(ui) = weak.upgrade() else { return };
        let tab = ui.get_library_tab();
        if tab == 0 {
            return;
        }
        let country = ui.get_library_country().trim().to_uppercase();
        if tab == 1 && (country.len() == 2 || country.is_empty()) {
            let mut s = settings.borrow_mut();
            if s.radio_country != country {
                s.radio_country = country.clone();
                s.save();
            }
        }
        ui.set_library_busy(true);
        ui.set_library_status("Searching…".into());
        let weak = ui.as_weak();
        let text = text.to_string();
        std::thread::spawn(move || {
            let result: Result<Rows, String> = if tab == 1 {
                search_stations(&text, &country).map(Rows::Stations)
            } else if text.trim().is_empty() {
                Ok(Rows::Shows(Vec::new()))
            } else {
                search_podcasts(&text).map(Rows::Shows)
            };
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_library_busy(false);
                match result {
                    Ok(rows) => {
                        let n = match &rows {
                            Rows::Stations(s) => s.len(),
                            Rows::Shows(s) => s.len(),
                            _ => 0,
                        };
                        ui.set_library_status(
                            if n == 0 {
                                "Nothing found".to_string()
                            } else {
                                format!("{n} found")
                            }
                            .into(),
                        );
                        show_rows(&ui, rows);
                    }
                    Err(e) => ui.set_library_status(e.into()),
                }
            });
        });
    });

    // A row: play it (stations, links, episodes) or open it (a podcast).
    let weak = ui.as_weak();
    ui.on_library_activate(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let i = i.max(0) as usize;
        let rows = ROWS.with_borrow(|r| r.clone());
        match rows {
            Some(Rows::Links) => {
                if let Some(l) = group_links(&ui).get(i) {
                    play(&ui, &l.url, false);
                }
            }
            Some(Rows::Stations(s)) => {
                if let Some(st) = s.get(i) {
                    count_click(st.uuid.clone());
                    play(&ui, &st.url, false);
                    ui.set_toast(format!("Playing {}", st.name).into());
                }
            }
            Some(Rows::Episodes(e)) => {
                if let Some(ep) = e.get(i) {
                    crate::pace::episode_of(&ep.url, &ep.show);
                    play(&ui, &ep.url, false);
                }
            }
            Some(Rows::Shows(s)) => {
                let Some(show) = s.get(i).cloned() else {
                    return;
                };
                ui.set_library_busy(true);
                ui.set_library_status(format!("Loading {}…", show.name).into());
                let weak = ui.as_weak();
                std::thread::spawn(move || {
                    let r = episodes(&show.feed);
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_library_busy(false);
                        match r {
                            Ok(eps) => {
                                ui.set_library_status(
                                    format!("{} · {} episodes", show.name, eps.len()).into(),
                                );
                                show_rows(&ui, Rows::Episodes(eps));
                            }
                            Err(e) => ui.set_library_status(e.into()),
                        }
                    });
                });
            }
            Some(Rows::Net(n)) => {
                if let Some(entry) = n.get(i).cloned() {
                    net_activate(&ui, entry);
                }
            }
            None => {}
        }
    });

    // The star / remove button of a row.
    let weak = ui.as_weak();
    ui.on_library_toggle_save(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let i = i.max(0) as usize;
        let rows = ROWS.with_borrow(|r| r.clone());
        match &rows {
            Some(Rows::Links) => {
                if let Some(l) = group_links(&ui).get(i) {
                    LINKS.with_borrow_mut(|all| {
                        all.retain(|x| x != l);
                        save_links(all);
                    });
                }
            }
            Some(Rows::Stations(s)) => {
                if let Some(st) = s.get(i) {
                    LINKS.with_borrow_mut(|all| {
                        if let Some(p) = all.iter().position(|x| x.url == st.url) {
                            all.remove(p);
                        } else {
                            all.push(Link {
                                group: "radio".into(),
                                name: st.name.clone(),
                                url: st.url.clone(),
                            });
                        }
                        save_links(all);
                    });
                }
            }
            Some(Rows::Shows(s)) => {
                if let Some(show) = s.get(i) {
                    SHOWS.with_borrow_mut(|all| {
                        if let Some(p) = all.iter().position(|x| x.feed == show.feed) {
                            all.remove(p);
                        } else {
                            all.push(show.clone());
                        }
                        save_shows(all);
                    });
                }
            }
            Some(Rows::Episodes(e)) => {
                if let Some(ep) = e.get(i) {
                    crate::pace::episode_of(&ep.url, &ep.show);
                    play(&ui, &ep.url, true);
                    ui.set_toast(format!("Added to the playlist: {}", ep.title).into());
                }
                return;
            }
            Some(Rows::Net(n)) => {
                if let Some(entry) = n.get(i).cloned() {
                    net_secondary(&ui, entry);
                }
                return;
            }
            _ => return,
        }
        if let Some(rows) = rows {
            show_rows(&ui, rows);
        }
    });

    // Adding a link by hand, or what is playing.
    let weak = ui.as_weak();
    ui.on_library_add(move |name, url| {
        let Some(ui) = weak.upgrade() else { return };
        let url = url.trim().to_owned();
        if !url.contains("://") {
            ui.set_library_status("Type a full link, starting with http:// or https://".into());
            return;
        }
        let group = GROUPS
            .get(ui.get_library_group().max(0) as usize)
            .copied()
            .unwrap_or("radio")
            .to_owned();
        let name = if name.trim().is_empty() {
            name_from_url(&url)
        } else {
            name.trim().to_owned()
        };
        LINKS.with_borrow_mut(|all| {
            all.retain(|x| x.url != url || x.group != group);
            all.push(Link {
                group,
                name: name.clone(),
                url,
            });
            save_links(all);
        });
        ui.set_library_status(format!("Saved {name}").into());
        show_rows(&ui, Rows::Links);
    });
    let (p, weak) = (player.clone(), ui.as_weak());
    ui.on_library_save_current(move || {
        let Some(ui) = weak.upgrade() else { return };
        let Some(url) = p.get_string("path").filter(|u| !u.is_empty()) else {
            ui.set_toast("Nothing is playing".into());
            return;
        };
        let has_video = ui.get_has_video();
        let group = if url.contains("://") {
            guess_group(&url, has_video)
        } else if has_video {
            "video"
        } else {
            "audio"
        };
        // A stream's own name, else the title shown, else from the link.
        let name = p
            .get_string("metadata/by-key/icy-name")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                p.get_string("media-title")
                    .filter(|t| !t.is_empty() && *t != url)
            })
            .unwrap_or_else(|| name_from_url(&url));
        LINKS.with_borrow_mut(|all| {
            all.retain(|x| x.url != url);
            all.push(Link {
                group: group.into(),
                name: name.clone(),
                url,
            });
            save_links(all);
        });
        let gi = GROUPS.iter().position(|g| *g == group).unwrap_or(0) as i32;
        ui.set_library_group(gi);
        ui.set_toast(
            format!(
                "Saved to {}: {name}",
                ["Radio", "Audio", "Video"][gi as usize]
            )
            .into(),
        );
        if ui.get_library_open() && ui.get_library_tab() == 0 {
            show_rows(&ui, Rows::Links);
        }
    });
    // Queue every saved link of the group (a radio night, a video list).
    let weak = ui.as_weak();
    ui.on_library_play_all(move || {
        let Some(ui) = weak.upgrade() else { return };
        let inputs: Vec<_> = group_links(&ui)
            .iter()
            .filter_map(|l| crate::parse_input(&l.url))
            .collect();
        if !inputs.is_empty() {
            crate::open_inputs(&ui, inputs, false);
        }
    });
}

// ---- the Network tab ----

fn shares_file() -> Option<PathBuf> {
    settings::config_dir().map(|d| d.join("shares.tsv"))
}

fn load_shares() -> Vec<(String, String)> {
    let text = shares_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let (uri, user) = l.split_once('\t').unwrap_or((l, ""));
            network::is_share(uri).then(|| (uri.trim().to_owned(), user.trim().to_owned()))
        })
        .collect()
}

fn save_shares(list: &[(String, String)]) {
    let Some(f) = shares_file() else { return };
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let text: String = list
        .iter()
        .map(|(u, n)| format!("{}\t{}\n", clean(u), clean(n)))
        .collect();
    let _ = std::fs::write(f, text);
}

/// The top of the Network tab: media servers found, then saved shares.
fn net_home() -> Vec<Net> {
    let mut v: Vec<Net> = SERVERS.with_borrow(|s| s.iter().cloned().map(Net::Server).collect());
    v.extend(SHARES.with_borrow(|s| {
        s.iter()
            .map(|(uri, user)| Net::Share {
                uri: uri.clone(),
                user: user.clone(),
            })
            .collect::<Vec<_>>()
    }));
    v
}

/// Open a folder of the Network tab (keeping the current one for Back).
fn enter(ui: &AppWindow, entries: Vec<Net>, status: String) {
    if let Some(Rows::Net(cur)) = ROWS.with_borrow(|r| r.clone()) {
        NET_STACK.with_borrow_mut(|s| s.push(cur));
    }
    ui.set_library_status(status.into());
    show_rows(ui, Rows::Net(entries));
}

fn dir_entries(dir: &std::path::Path) -> Result<Vec<Net>, String> {
    let (dirs, files) = network::list_dir(dir)?;
    Ok(dirs
        .into_iter()
        .map(Net::Dir)
        .chain(files.into_iter().map(Net::File))
        .collect())
}

fn dlna_entries(control: &str, list: Vec<network::Entry>) -> Vec<Net> {
    list.into_iter()
        .map(|e| match e {
            network::Entry::Folder { title, id, count } => Net::Folder {
                control: control.to_owned(),
                id,
                title,
                count,
            },
            network::Entry::Item { title, url, detail } => Net::Item { url, title, detail },
        })
        .collect()
}

/// Browse a DLNA folder on a worker thread, then show it.
fn open_dlna(ui: &AppWindow, control: String, id: String, title: String) {
    ui.set_library_busy(true);
    ui.set_library_status(format!("Opening {title}…").into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let r = network::browse(&control, &id);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_library_busy(false);
            match r {
                Ok(list) => {
                    let n = list.len();
                    enter(
                        &ui,
                        dlna_entries(&control, list),
                        format!("{title} · {n} items"),
                    );
                }
                Err(e) => ui.set_library_status(e.into()),
            }
        });
    });
}

fn net_activate(ui: &AppWindow, entry: Net) {
    match entry {
        Net::Server(s) => open_dlna(ui, s.control, "0".into(), s.name),
        Net::Folder {
            control, id, title, ..
        } => open_dlna(ui, control, id, title),
        Net::Item { url, .. } => play(ui, &url, false),
        Net::File(p) => crate::open_inputs(ui, vec![crate::Input::Path(p)], false),
        Net::Dir(p) => match dir_entries(&p) {
            Ok(list) => {
                let n = list.len();
                enter(ui, list, format!("{} · {n} items", file_name(&p)));
            }
            Err(e) => ui.set_library_status(e.into()),
        },
        Net::Share { uri, user } => {
            // The password field, when filled, is for this share.
            let password = ui.get_library_password().to_string();
            let user = if ui.get_library_user().trim().is_empty() {
                user
            } else {
                ui.get_library_user().trim().to_owned()
            };
            connect(ui, uri, user, password);
        }
    }
}

fn net_secondary(ui: &AppWindow, entry: Net) {
    match entry {
        Net::Share { uri, .. } => {
            SHARES.with_borrow_mut(|s| {
                s.retain(|x| x.0 != uri);
                save_shares(s);
            });
            show_rows(ui, Rows::Net(net_home()));
        }
        Net::Item { url, title, .. } => {
            play(ui, &url, true);
            ui.set_toast(format!("Added to the playlist: {title}").into());
        }
        Net::File(p) => crate::open_inputs(ui, vec![crate::Input::Path(p)], true),
        Net::Dir(p) => crate::open_inputs(ui, vec![crate::Input::Path(p)], false),
        Net::Folder {
            control, id, title, ..
        } => {
            // Everything playable in the folder (not its subfolders).
            ui.set_library_status(format!("Queueing {title}…").into());
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let r = network::browse(&control, &id);
                let _ = weak.upgrade_in_event_loop(move |ui| match r {
                    Ok(list) => {
                        let inputs: Vec<_> = list
                            .iter()
                            .filter_map(|e| match e {
                                network::Entry::Item { url, .. } => crate::parse_input(url),
                                _ => None,
                            })
                            .collect();
                        ui.set_library_status(
                            format!("{}: {} files queued", title, inputs.len()).into(),
                        );
                        if !inputs.is_empty() {
                            crate::open_inputs(&ui, inputs, false);
                        }
                    }
                    Err(e) => ui.set_library_status(e.into()),
                });
            });
        }
        Net::Server(_) => {}
    }
}

/// Mount a share on a worker thread, remember it, and open its folder.
fn connect(ui: &AppWindow, uri: String, user: String, password: String) {
    let uri = uri.trim().to_owned();
    if !network::is_share(&uri) {
        ui.set_library_status("Type an address like smb://nas/music, sftp://me@server/videos or davs://cloud.example/dav".into());
        return;
    }
    ui.set_library_busy(true);
    ui.set_library_status(format!("Connecting to {uri}…").into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let r = network::mount(&uri, &user, &password)
            .and_then(|dir| dir_entries(&dir).map(|e| (dir, e)));
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_library_busy(false);
            match r {
                Ok((dir, list)) => {
                    SHARES.with_borrow_mut(|s| {
                        if !s.iter().any(|x| x.0 == uri) {
                            s.push((uri.clone(), user.clone()));
                            save_shares(s);
                        }
                    });
                    ui.set_library_password(SharedString::new());
                    let n = list.len();
                    // Back from the share goes to the Network tab's top.
                    NET_STACK.with_borrow_mut(|s| s.clear());
                    ROWS.with_borrow_mut(|r| *r = Some(Rows::Net(net_home())));
                    enter(&ui, list, format!("{} · {n} items", dir.display()));
                }
                Err(e) => ui.set_library_status(e.into()),
            }
        });
    });
}

fn install_network(ui: &AppWindow) {
    SHARES.with_borrow_mut(|s| *s = load_shares());
    let weak = ui.as_weak();
    ui.on_library_rescan(move || {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_library_busy(true);
        ui.set_library_status("Looking for media servers on your network…".into());
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let found = network::discover(Duration::from_secs(3));
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_library_busy(false);
                let n = found.len();
                SERVERS.with_borrow_mut(|s| *s = found);
                ui.set_library_status(
                    match n {
                        0 => "No media servers (DLNA / UPnP) answered. Shares (SMB, SFTP, WebDAV) connect with the fields above.".to_string(),
                        1 => "1 media server found".into(),
                        n => format!("{n} media servers found"),
                    }
                    .into(),
                );
                if ui.get_library_tab() == 3 && NET_STACK.with_borrow(|s| s.is_empty()) {
                    show_rows(&ui, Rows::Net(net_home()));
                }
            });
        });
    });
    let weak = ui.as_weak();
    ui.on_library_connect(move |uri, user, password| {
        if let Some(ui) = weak.upgrade() {
            connect(
                &ui,
                uri.to_string(),
                user.trim().to_owned(),
                password.to_string(),
            );
        }
    });
    let weak = ui.as_weak();
    ui.on_library_back(move || {
        let Some(ui) = weak.upgrade() else { return };
        match ROWS.with_borrow(|r| r.clone()) {
            Some(Rows::Episodes(_)) => {
                show_rows(&ui, Rows::Shows(SHOWS.with_borrow(|s| s.clone())))
            }
            Some(Rows::Net(_)) => {
                let prev = NET_STACK
                    .with_borrow_mut(|s| s.pop())
                    .unwrap_or_else(net_home);
                ui.set_library_status(SharedString::new());
                show_rows(&ui, Rows::Net(prev));
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against the real services: cargo test -- --ignored live_services
    #[test]
    #[ignore]
    fn live_services() {
        let st = search_stations("kiss", "MD").unwrap();
        println!(
            "{} stations; first: {} | {} | {}",
            st.len(),
            st[0].name,
            st[0].url,
            st[0].detail
        );
        assert!(st.iter().any(|s| s.name.to_lowercase().contains("kiss")));
        let jazz = search_stations("jazz", "").unwrap();
        println!("jazz: {} stations", jazz.len());
        let shows = search_podcasts("history").unwrap();
        println!("{} podcasts; first: {}", shows.len(), shows[0].name);
        let eps = episodes(&shows[0].feed).unwrap();
        println!(
            "{} episodes; first: {} | {}",
            eps.len(),
            eps[0].title,
            eps[0].detail
        );
        assert!(!eps.is_empty());
    }

    #[test]
    fn link_names_and_groups() {
        assert_eq!(
            name_from_url("https://live.kissfm.ro/kissfm.chisinau"),
            "kissfm.chisinau"
        );
        assert_eq!(name_from_url("https://radio.example/stream/?x=1"), "stream");
        assert_eq!(name_from_url("http://host:8000/"), "host:8000");
        assert_eq!(
            guess_group("https://www.youtube.com/watch?v=x", false),
            "video"
        );
        assert_eq!(guess_group("https://x/song.flac", false), "audio");
        assert_eq!(
            guess_group("https://live.kissfm.ro/kissfm.chisinau", false),
            "radio"
        );
    }

    #[test]
    fn links_file_round_trip() {
        let l = parse_links(
            "radio\tKiss FM\thttps://live.kissfm.ro/kissfm.chisinau\nbad\tx\ty\nvideo\tT\t\n",
        );
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].name, "Kiss FM");
    }

    #[test]
    fn stations_and_feeds() {
        let v: Value = serde_json::from_str(
            r#"[{"stationuuid":"u1","name":" Kiss FM Chișinău ","url":"http://a","url_resolved":"https://b",
                 "country":"Moldova","tags":"pop,dance","codec":"AAC","bitrate":80}]"#,
        )
        .unwrap();
        let s = stations_of(&v);
        assert_eq!(s[0].url, "https://b");
        assert_eq!(s[0].name, "Kiss FM Chișinău");
        assert_eq!(s[0].detail, "Moldova · pop, dance · AAC 80 kbit/s");
        let feed = r#"<?xml version="1.0"?><rss xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd"><channel>
            <item><title>Ep 2</title><pubDate>Tue, 18 Nov 2026 10:00:00 GMT</pubDate><itunes:duration>3725</itunes:duration>
            <enclosure url="https://x/2.mp3" type="audio/mpeg"/></item>
            <item><title>No audio</title></item></channel></rss>"#;
        let e = parse_feed(feed).unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].detail, "18 Nov 2026 · 1:02:05");
    }
}
