//! The media library's pages (ui/media.slint), built from the index
//! (medialib.rs) with pictures from art.rs.
//!
//! A page is a list of sections (headings, cards, songs) made on a worker
//! thread. For the window they become lines: one per heading, per song,
//! and per row of cards, so only what is on screen is ever drawn. The
//! cards' pictures are fetched when a card is first shown.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use player_core::Player;
use slint::{
    ComponentHandle, Model, ModelNotify, ModelRc, ModelTracker, SharedString, Timer, TimerMode,
    VecModel,
};

use crate::art::{self, Key};
use crate::medialib::{self, Album, Albums, Db, Group, MAX_SONGS, Pick, Sort, Track};
use crate::settings::Settings;
use crate::{AppWindow, Input, MediaCard, MediaLine, fmt_clock, resume};

/// The library is scanned again when it opens after this long.
const RESCAN_AFTER: Duration = Duration::from_secs(120);
const CONTINUE_MAX: usize = 24;
const NEWEST: usize = 18;

#[derive(Clone, Debug, PartialEq)]
enum Page {
    Home,
    Albums,
    Artists,
    Genres,
    Songs,
    Videos,
    Folders,
    Manage,
    Album(String),
    Artist(String),
    Genre(String, String),
    Folder(String),
    /// The videos of one folder (from the Videos page).
    VideoFolder(String),
    Search(String),
}

impl Page {
    fn of_tab(i: i32) -> Page {
        match i {
            1 => Page::Albums,
            2 => Page::Artists,
            3 => Page::Genres,
            4 => Page::Songs,
            5 => Page::Videos,
            6 => Page::Folders,
            7 => Page::Manage,
            _ => Page::Home,
        }
    }

    /// The sidebar entry it belongs to (-1: none).
    fn tab(&self) -> i32 {
        match self {
            Page::Home => 0,
            Page::Albums | Page::Album(_) => 1,
            Page::Artists | Page::Artist(_) => 2,
            Page::Genres | Page::Genre(..) => 3,
            Page::Songs => 4,
            Page::Videos | Page::VideoFolder(_) => 5,
            Page::Folders | Page::Folder(_) => 6,
            Page::Manage => 7,
            Page::Search(_) => -1,
        }
    }

    /// How it can be sorted; the first is the default.
    fn sorts(&self) -> &'static [Sort] {
        match self {
            Page::Albums => &[Sort::Name, Sort::Artist, Sort::Year, Sort::Newest],
            Page::Songs => &[
                Sort::Name,
                Sort::Artist,
                Sort::Album,
                Sort::Newest,
                Sort::Duration,
            ],
            Page::Genre(..) => &[Sort::Artist, Sort::Name, Sort::Album, Sort::Newest],
            Page::Videos | Page::VideoFolder(_) | Page::Folder(_) => &[Sort::Name, Sort::Newest],
            _ => &[],
        }
    }
}

fn sort_name(s: Sort) -> &'static str {
    match s {
        Sort::Name => "By name",
        Sort::Artist => "By artist",
        Sort::Album => "By album",
        Sort::Newest => "Newest first",
        Sort::Year => "By year",
        Sort::Duration => "Longest first",
    }
}

/// What a card opens.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    Page(Page),
    File(String),
}

#[derive(Clone, Debug)]
struct Card {
    title: String,
    subtitle: String,
    corner: String,
    icon: i32,
    shape: i32,
    art: Option<Key>,
    progress: f32,
    removable: bool,
    /// Seconds played so far, for a card whose file's length isn't known yet.
    left_at: Option<f64>,
    target: Target,
}

#[derive(Clone, Debug)]
enum Section {
    Heading(String),
    Cards(Vec<Card>),
    Songs(Vec<Track>),
    Note(String),
}

/// A page, ready to show.
#[derive(Clone, Debug, Default)]
struct Built {
    title: String,
    subtitle: String,
    head: Option<Card>,
    can_play: bool,
    sections: Vec<Section>,
    folders: Vec<String>,
}

// ---- the cards' model ----

/// The cards; a card's picture is looked up (and asked for) only when the
/// card is drawn.
struct Cards {
    list: RefCell<Vec<Card>>,
    notify: ModelNotify,
}

impl Cards {
    fn new(list: Vec<Card>) -> Self {
        Self {
            list: RefCell::new(list),
            notify: ModelNotify::default(),
        }
    }

    fn arrived(&self, key: &Key) {
        let rows: Vec<usize> = {
            let mut list = self.list.borrow_mut();
            let rows: Vec<usize> = list
                .iter()
                .enumerate()
                .filter(|(_, c)| c.art.as_ref() == Some(key))
                .map(|(i, _)| i)
                .collect();
            // Making a video's picture also measured it.
            if let Key::Video(path) = key
                && rows.iter().any(|i| list[*i].corner.is_empty())
                && let Some(d) = Db::open().ok().and_then(|db| db.duration(path))
            {
                for i in &rows {
                    set_duration(&mut list[*i], d);
                }
            }
            rows
        };
        for i in rows {
            self.notify.row_changed(i);
        }
    }
}

impl Model for Cards {
    type Data = MediaCard;

    fn row_count(&self) -> usize {
        self.list.borrow().len()
    }

    fn row_data(&self, i: usize) -> Option<MediaCard> {
        self.list.borrow().get(i).map(card_to_ui)
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn card_to_ui(c: &Card) -> MediaCard {
    let image = c.art.as_ref().and_then(art::get);
    MediaCard {
        title: c.title.as_str().into(),
        subtitle: c.subtitle.as_str().into(),
        corner: c.corner.as_str().into(),
        icon: c.icon,
        shape: c.shape,
        has_image: image.is_some(),
        image: image.unwrap_or_default(),
        progress: c.progress,
        removable: c.removable,
    }
}

/// Fill in the length of a card's file: how far it was played, or the
/// length itself over the corner.
fn set_duration(c: &mut Card, d: f64) {
    if d <= 0.0 {
        return;
    }
    if let Some(t) = c.left_at {
        c.progress = (t / d).clamp(0.0, 1.0) as f32;
        c.corner = format!("{} left", fmt_clock((d - t).max(0.0)));
    } else {
        c.corner = fmt_clock(d);
    }
}

// ---- state ----

struct View {
    page: Page,
    /// The pages Back returns to, and how far each was scrolled.
    back: Vec<(Page, f32)>,
    built: Built,
    cards: Rc<Cards>,
    /// The songs of the page, in line order.
    songs: Vec<Track>,
    /// The range of `songs` each section covers.
    ranges: Vec<std::ops::Range<usize>>,
    /// The room for cards, in logical pixels.
    width: f32,
    /// (cards per row for square, for wide) the lines were made with.
    columns: (usize, usize),
    sorts: Vec<(i32, Sort)>,
    last_scan: Option<Instant>,
    online: bool,
}

thread_local! {
    static VIEW: RefCell<View> = RefCell::new(View {
        page: Page::Home,
        back: Vec::new(),
        built: Built::default(),
        cards: Rc::new(Cards::new(Vec::new())),
        songs: Vec::new(),
        ranges: Vec::new(),
        width: 820.0,
        columns: (0, 0),
        sorts: Vec::new(),
        last_scan: None,
        online: false,
    });
    /// Only the newest page request is shown.
    static REQUEST: Cell<u64> = const { Cell::new(0) };
    static SEARCH_TIMER: Timer = Timer::default();
}

static SCANNING: AtomicBool = AtomicBool::new(false);

/// How many cards fit side by side: (square, wide).
fn columns(width: f32) -> (usize, usize) {
    let fit = |w: f32, least: usize| (((width + 16.0) / (w + 16.0)) as usize).max(least);
    (fit(158.0, 2), fit(226.0, 1))
}

/// Sections → lines. Also the cards and songs in the order lines refer to
/// them, and each section's range of songs.
fn lines(
    built: &Built,
    cols: (usize, usize),
) -> (
    Vec<MediaLine>,
    Vec<Card>,
    Vec<Track>,
    Vec<std::ops::Range<usize>>,
) {
    let (mut out, mut cards, mut songs, mut ranges) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for s in &built.sections {
        let start = songs.len();
        match s {
            Section::Heading(t) => out.push(MediaLine {
                kind: 0,
                title: t.as_str().into(),
                ..Default::default()
            }),
            Section::Note(t) => out.push(MediaLine {
                kind: 3,
                title: t.as_str().into(),
                ..Default::default()
            }),
            Section::Cards(list) => {
                let shape = list.first().map_or(0, |c| c.shape);
                let n = if shape == 2 { cols.1 } else { cols.0 };
                for row in list.chunks(n) {
                    out.push(MediaLine {
                        kind: 1,
                        first: cards.len() as i32,
                        count: row.len() as i32,
                        columns: n as i32,
                        shape,
                        ..Default::default()
                    });
                    cards.extend(row.iter().cloned());
                }
            }
            Section::Songs(list) => {
                let album = matches!(&built.head, Some(c) if matches!(&c.target, Target::Page(Page::Album(_))));
                let discs = album && list.iter().any(|t| t.disc > 1);
                for (i, t) in list.iter().enumerate() {
                    let number = match (album, t.track) {
                        (true, 0) => String::new(),
                        (true, n) if discs => format!("{}-{n:02}", t.disc.max(1)),
                        (true, n) => n.to_string(),
                        _ => (i + 1).to_string(),
                    };
                    let detail = if t.video {
                        String::new()
                    } else {
                        let mut d = vec![t.artist.as_str()];
                        if !album {
                            d.push(&t.album);
                        }
                        d.retain(|s| !s.is_empty());
                        // Untagged: the folder says where it belongs.
                        match Path::new(&t.dir).file_name() {
                            Some(f) if !album && t.album.is_empty() => {
                                let f = f.to_string_lossy();
                                d.iter()
                                    .map(|s| s.to_string())
                                    .chain([f.into_owned()])
                                    .collect::<Vec<_>>()
                                    .join(" · ")
                            }
                            _ => d.join(" · "),
                        }
                    };
                    out.push(MediaLine {
                        kind: 2,
                        title: t.title.as_str().into(),
                        detail: detail.into(),
                        right: if t.duration > 0.0 {
                            fmt_clock(t.duration).into()
                        } else {
                            SharedString::new()
                        },
                        number: number.into(),
                        path: t.path.as_str().into(),
                        first: (songs.len() + i) as i32,
                        ..Default::default()
                    });
                }
                songs.extend(list.iter().cloned());
                ranges.push(start..songs.len());
            }
        }
    }
    (out, cards, songs, ranges)
}

// ---- making a page (worker thread) ----

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn total_time(list: &[Track]) -> String {
    let s: f64 = list.iter().map(|t| t.duration).sum();
    let m = (s / 60.0).round() as u64;
    match (m / 60, m % 60) {
        _ if s <= 0.0 => String::new(),
        (0, m) => format!("{} min", m.max(1)),
        (h, m) => format!("{h} h {m} min"),
    }
}

fn join(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn plain(title: &str, subtitle: String, target: Target) -> Card {
    Card {
        title: title.to_owned(),
        subtitle,
        corner: String::new(),
        icon: 0,
        shape: 0,
        art: None,
        progress: -1.0,
        removable: false,
        left_at: None,
        target,
    }
}

fn album_card(a: &Album, with_artist: bool) -> Card {
    let year = if a.year > 0 {
        a.year.to_string()
    } else {
        String::new()
    };
    let sub = if with_artist {
        join(&[a.artist.clone(), year])
    } else {
        join(&[year, plural(a.songs as usize, "song", "songs")])
    };
    Card {
        art: Some(Key::Cover(a.path.clone())),
        ..plain(&a.name, sub, Target::Page(Page::Album(a.key.clone())))
    }
}

fn artist_card(g: &Group, online: bool) -> Card {
    let albums = if g.albums > 0 {
        plural(g.albums as usize, "album", "albums")
    } else {
        String::new()
    };
    Card {
        icon: 1,
        shape: 1,
        art: Some(Key::Artist {
            name: g.name.clone(),
            path: g.path.clone(),
            online,
        }),
        ..plain(
            &g.name,
            join(&[albums, plural(g.items as usize, "song", "songs")]),
            Target::Page(Page::Artist(g.key.clone())),
        )
    }
}

fn file_key(path: &str, video: bool) -> Key {
    if video {
        Key::Video(path.to_owned())
    } else {
        Key::Cover(path.to_owned())
    }
}

fn folder_card(g: &Group, page: Page, shape: i32) -> Card {
    let (songs, videos) = ((g.items - g.videos) as usize, g.videos as usize);
    let count = join(&[
        if songs > 0 {
            plural(songs, "song", "songs")
        } else {
            String::new()
        },
        if videos > 0 {
            plural(videos, "video", "videos")
        } else {
            String::new()
        },
    ]);
    let name = Path::new(&g.key)
        .file_name()
        .map_or(g.key.clone(), |n| n.to_string_lossy().into_owned());
    // Where it is, when that says more than the name.
    let place = g
        .name
        .rsplit_once(" / ")
        .map(|(p, _)| p.to_owned())
        .unwrap_or_default();
    Card {
        icon: 3,
        shape,
        art: Some(file_key(&g.path, !medialib::is_audio(Path::new(&g.path)))),
        corner: count,
        ..plain(&name, place, Target::Page(page))
    }
}

fn video_card(t: &Track) -> Card {
    let sub = Path::new(&t.dir)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut c = Card {
        icon: 2,
        shape: 2,
        art: Some(Key::Video(t.path.clone())),
        ..plain(&t.title, sub, Target::File(t.path.clone()))
    };
    set_duration(&mut c, t.duration);
    c
}

/// "Continue watching": what was left part-way, the latest first.
fn continue_cards(db: &Db, left: &[(f64, String)]) -> Vec<Card> {
    let mut out = Vec::new();
    for (t, path) in left {
        if out.len() >= CONTINUE_MAX {
            break;
        }
        let link = path.contains("://");
        if !link && !Path::new(path).is_file() {
            continue; // deleted, or its drive isn't there now
        }
        let audio = !link && medialib::is_audio(Path::new(path));
        let title = match db.title(path) {
            Some((title, artist)) if !artist.is_empty() => format!("{artist} - {title}"),
            Some((title, _)) => title,
            None if link => crate::library::name_from_url(path),
            None => Path::new(path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let mut c = Card {
            icon: if audio { 0 } else { 2 },
            shape: 2,
            art: (!link).then(|| file_key(path, !audio)),
            progress: 0.0,
            removable: true,
            left_at: Some(*t),
            ..plain(
                &title,
                format!("Stopped at {}", fmt_clock(*t)),
                Target::File(path.clone()),
            )
        };
        if let Some(d) = db.duration(path).filter(|_| !link) {
            set_duration(&mut c, d);
        }
        out.push(c);
    }
    out
}

/// Videos by folder: a folder with several becomes one card.
fn video_sections(db: &Db, sort: Sort) -> medialib::Result<(usize, Vec<Section>)> {
    let all = db.tracks(&Pick::Videos, sort)?;
    let roots = db.folders()?;
    let mut folders: Vec<Card> = Vec::new();
    let mut single: Vec<Card> = Vec::new();
    let mut per_dir: std::collections::HashMap<&str, Vec<&Track>> =
        std::collections::HashMap::new();
    for t in &all {
        per_dir.entry(&t.dir).or_default().push(t);
    }
    for t in &all {
        let list = &per_dir[t.dir.as_str()];
        if list.len() < 2 || roots.contains(&t.dir) {
            single.push(video_card(t));
        } else if list[0].path == t.path {
            let g = Group {
                key: t.dir.clone(),
                name: medialib::dir_name(&t.dir, &roots),
                items: list.len() as u32,
                videos: list.len() as u32,
                path: t.path.clone(),
                albums: 0,
            };
            folders.push(folder_card(&g, Page::VideoFolder(t.dir.clone()), 2));
        }
    }
    let mut out = Vec::new();
    if !folders.is_empty() {
        out.push(Section::Heading("Folders".into()));
        out.push(Section::Cards(folders));
        if !single.is_empty() {
            out.push(Section::Heading("Videos".into()));
        }
    }
    out.push(Section::Cards(single));
    Ok((all.len(), out))
}

fn build(
    db: &Db,
    page: &Page,
    sort: Sort,
    left: &[(f64, String)],
    online: bool,
) -> medialib::Result<Built> {
    let mut b = Built {
        folders: db.folders()?,
        ..Default::default()
    };
    let roots = b.folders.clone();
    let heading = |s: &mut Vec<Section>, t: &str| s.push(Section::Heading(t.into()));
    match page {
        Page::Home => {
            b.title = "Home".into();
            let (songs, videos) = db.counts()?;
            if songs + videos > 0 {
                b.subtitle = join(&[
                    plural(songs as usize, "song", "songs"),
                    plural(videos as usize, "video", "videos"),
                ]);
            }
            let cont = continue_cards(db, left);
            if !cont.is_empty() {
                heading(&mut b.sections, "Continue watching");
                b.sections.push(Section::Cards(cont));
            }
            let new = db.tracks(&Pick::Newest(true, NEWEST), Sort::Newest)?;
            if !new.is_empty() {
                heading(&mut b.sections, "New videos");
                b.sections
                    .push(Section::Cards(new.iter().map(video_card).collect()));
            }
            let new = db.albums(&Albums::Newest(NEWEST), Sort::Newest)?;
            if !new.is_empty() {
                heading(&mut b.sections, "New albums");
                b.sections.push(Section::Cards(
                    new.iter().map(|a| album_card(a, true)).collect(),
                ));
            }
            let new = db.tracks(&Pick::Newest(false, NEWEST), Sort::Newest)?;
            if !new.is_empty() {
                heading(&mut b.sections, "New songs");
                b.sections.push(Section::Songs(new));
            }
            if b.sections.is_empty() && !roots.is_empty() {
                b.sections.push(Section::Note(
                    "Nothing found in the library's folders yet.".into(),
                ));
            }
        }
        Page::Albums => {
            let all = db.albums(&Albums::All, sort)?;
            b.title = "Albums".into();
            b.subtitle = plural(all.len(), "album", "albums");
            if all.is_empty() {
                b.sections.push(Section::Note("No song here names its album in its tags. Songs without tags are under Songs and Folders.".into()));
            }
            b.sections.push(Section::Cards(
                all.iter().map(|a| album_card(a, true)).collect(),
            ));
        }
        Page::Artists => {
            let all = db.artists(None)?;
            b.title = "Artists".into();
            b.subtitle = plural(all.len(), "artist", "artists");
            if all.is_empty() {
                b.sections.push(Section::Note(
                    "No artists found in the songs' tags or file names.".into(),
                ));
            }
            b.sections.push(Section::Cards(
                all.iter().map(|g| artist_card(g, online)).collect(),
            ));
        }
        Page::Genres => {
            let all = db.genres()?;
            b.title = "Genres".into();
            b.subtitle = plural(all.len(), "genre", "genres");
            if all.is_empty() {
                b.sections
                    .push(Section::Note("No genres found in the songs' tags.".into()));
            }
            let cards = all.iter().map(|g| Card {
                icon: 4,
                art: Some(Key::Cover(g.path.clone())),
                ..plain(
                    &g.name,
                    plural(g.items as usize, "song", "songs"),
                    Target::Page(Page::Genre(g.key.clone(), g.name.clone())),
                )
            });
            b.sections.push(Section::Cards(cards.collect()));
        }
        Page::Songs => {
            let all = db.tracks(&Pick::Songs, sort)?;
            b.title = "Songs".into();
            b.subtitle = join(&[plural(all.len(), "song", "songs"), total_time(&all)]);
            if all.len() == MAX_SONGS {
                b.subtitle = format!("The first {MAX_SONGS} songs; search finds the others");
            }
            b.sections.push(Section::Songs(all));
        }
        Page::Videos => {
            let (n, sections) = video_sections(db, sort)?;
            b.title = "Videos".into();
            b.subtitle = plural(n, "video", "videos");
            b.sections = sections;
            if n == 0 {
                b.sections.push(Section::Note(
                    "No videos found in the library's folders.".into(),
                ));
            }
        }
        Page::Folders => {
            let all = db.dirs()?;
            b.title = "Folders".into();
            b.subtitle = plural(all.len(), "folder", "folders");
            b.sections.push(Section::Cards(
                all.iter()
                    .map(|g| folder_card(g, Page::Folder(g.key.clone()), 0))
                    .collect(),
            ));
        }
        Page::Manage => {
            b.title = "Library folders".into();
        }
        Page::Album(key) => {
            let songs = db.tracks(&Pick::Album(key.clone()), sort)?;
            let album = db
                .albums(&Albums::All, Sort::Name)?
                .into_iter()
                .find(|a| a.key == *key)
                .unwrap_or_default();
            b.title = album.name.clone();
            b.subtitle = join(&[
                album.artist.clone(),
                if album.year > 0 {
                    album.year.to_string()
                } else {
                    String::new()
                },
                plural(songs.len(), "song", "songs"),
                total_time(&songs),
            ]);
            b.head = Some(album_card(&album, true));
            b.sections.push(Section::Songs(songs));
        }
        Page::Artist(key) => {
            let songs = db.tracks(&Pick::Artist(key.clone()), Sort::Name)?;
            let albums = db.albums(&Albums::Artist(key.clone()), Sort::Year)?;
            let g = db
                .artists(None)?
                .into_iter()
                .find(|g| g.key == *key)
                .unwrap_or_default();
            b.title = g.name.clone();
            b.subtitle = join(&[
                if albums.is_empty() {
                    String::new()
                } else {
                    plural(albums.len(), "album", "albums")
                },
                plural(songs.len(), "song", "songs"),
                total_time(&songs),
            ]);
            b.head = Some(artist_card(&g, online));
            if !albums.is_empty() {
                heading(&mut b.sections, "Albums");
                b.sections.push(Section::Cards(
                    albums.iter().map(|a| album_card(a, false)).collect(),
                ));
                heading(&mut b.sections, "Songs");
            }
            b.sections.push(Section::Songs(songs));
        }
        Page::Genre(key, name) => {
            let songs = db.tracks(&Pick::Genre(key.clone()), sort)?;
            let albums = db.albums(&Albums::Genre(key.clone()), Sort::Artist)?;
            b.title = name.clone();
            b.subtitle = join(&[plural(songs.len(), "song", "songs"), total_time(&songs)]);
            if !albums.is_empty() {
                heading(&mut b.sections, "Albums");
                b.sections.push(Section::Cards(
                    albums.iter().map(|a| album_card(a, true)).collect(),
                ));
                heading(&mut b.sections, "Songs");
            }
            b.sections.push(Section::Songs(songs));
        }
        Page::Folder(dir) | Page::VideoFolder(dir) => {
            let mut all = db.tracks(&Pick::Folder(dir.clone()), sort)?;
            if matches!(page, Page::VideoFolder(_)) {
                all.retain(|t| t.video);
            }
            let (videos, songs): (Vec<Track>, Vec<Track>) = all.into_iter().partition(|t| t.video);
            b.title = Path::new(dir)
                .file_name()
                .map_or(dir.clone(), |n| n.to_string_lossy().into_owned());
            b.subtitle = join(&[
                medialib::dir_name(dir, &roots),
                if songs.is_empty() {
                    String::new()
                } else {
                    plural(songs.len(), "song", "songs")
                },
                if videos.is_empty() {
                    String::new()
                } else {
                    plural(videos.len(), "video", "videos")
                },
                total_time(&songs),
            ]);
            // Songs are what Play queues in a mixed folder; else its videos.
            b.can_play = !videos.is_empty();
            if !videos.is_empty() {
                b.sections.push(Section::Cards(
                    videos
                        .iter()
                        .map(|t| Card {
                            subtitle: String::new(),
                            ..video_card(t)
                        })
                        .collect(),
                ));
            }
            if !songs.is_empty() {
                b.sections.push(Section::Songs(songs));
            }
        }
        Page::Search(text) => {
            b.title = format!("“{}”", text.trim());
            let artists = db.artists(Some(text))?;
            let albums = db.albums(&Albums::Search(text.clone()), Sort::Name)?;
            let videos = db.tracks(&Pick::Search(text.clone(), true), Sort::Name)?;
            let songs = db.tracks(&Pick::Search(text.clone(), false), Sort::Name)?;
            let n = artists.len() + albums.len() + videos.len() + songs.len();
            b.subtitle = if n == 0 {
                "Nothing found".into()
            } else {
                plural(n, "match", "matches")
            };
            if !artists.is_empty() {
                heading(&mut b.sections, "Artists");
                b.sections.push(Section::Cards(
                    artists
                        .iter()
                        .take(24)
                        .map(|g| artist_card(g, online))
                        .collect(),
                ));
            }
            if !albums.is_empty() {
                heading(&mut b.sections, "Albums");
                b.sections.push(Section::Cards(
                    albums
                        .iter()
                        .take(60)
                        .map(|a| album_card(a, true))
                        .collect(),
                ));
            }
            if !videos.is_empty() {
                heading(&mut b.sections, "Videos");
                b.sections.push(Section::Cards(
                    videos.iter().take(120).map(video_card).collect(),
                ));
            }
            if !songs.is_empty() {
                heading(&mut b.sections, "Songs");
                b.sections.push(Section::Songs(songs));
            }
        }
    }
    // No folders yet: the invitation to add some says it all.
    if roots.is_empty() && *page != Page::Home {
        b.sections.clear();
    }
    b.can_play |= *page != Page::Home
        && b.sections
            .iter()
            .any(|s| matches!(s, Section::Songs(l) if !l.is_empty()));
    Ok(b)
}

/// Everything a page plays, in order.
fn playable(db: &Db, page: &Page, sort: Sort) -> medialib::Result<Vec<String>> {
    let pick = match page {
        Page::Songs => Pick::Songs,
        Page::Album(k) => Pick::Album(k.clone()),
        Page::Artist(k) => Pick::Artist(k.clone()),
        Page::Genre(k, _) => Pick::Genre(k.clone()),
        Page::Folder(d) | Page::VideoFolder(d) => Pick::Folder(d.clone()),
        Page::Search(t) => Pick::Search(t.clone(), false),
        _ => return Ok(Vec::new()),
    };
    let mut list = db.tracks(&pick, sort)?;
    let songs_first = matches!(page, Page::Folder(_)) && list.iter().any(|t| !t.video);
    match page {
        Page::VideoFolder(_) => list.retain(|t| t.video),
        _ if songs_first => list.retain(|t| !t.video),
        _ => {}
    }
    Ok(list.into_iter().map(|t| t.path).collect())
}

// ---- showing a page ----

fn sort_of(page: &Page) -> Sort {
    let default = page.sorts().first().copied().unwrap_or(Sort::Name);
    VIEW.with_borrow(|v| {
        v.sorts
            .iter()
            .find(|(t, _)| *t == page.tab())
            .map(|(_, s)| *s)
    })
    .filter(|s| page.sorts().contains(s))
    .unwrap_or(default)
}

/// Make the page on a worker thread and show it; `scroll` is where to.
fn load(ui: &AppWindow, scroll: Option<f32>) {
    let request = REQUEST.get() + 1;
    REQUEST.set(request);
    let (page, online) = VIEW.with_borrow(|v| (v.page.clone(), v.online));
    let sort = sort_of(&page);
    let left = if page == Page::Home {
        resume::list()
    } else {
        Vec::new()
    };
    ui.set_media_page(page.tab());
    ui.set_media_can_back(VIEW.with_borrow(|v| !v.back.is_empty()));
    ui.set_media_loading(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let built = Db::open().and_then(|db| build(&db, &page, sort, &left, online));
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if REQUEST.get() != request {
                return; // another page was asked for since
            }
            ui.set_media_loading(false);
            match built {
                Ok(b) => show(&ui, b, &page, sort, scroll),
                Err(e) => {
                    log::warn!("{e}");
                    ui.set_media_status(e.into());
                }
            }
        });
    });
}

fn show(ui: &AppWindow, built: Built, page: &Page, sort: Sort, scroll: Option<f32>) {
    let strings = |list: Vec<SharedString>| ModelRc::new(VecModel::from(list));
    ui.set_media_heading(built.title.as_str().into());
    ui.set_media_subheading(built.subtitle.as_str().into());
    ui.set_media_can_play(built.can_play);
    ui.set_media_empty(built.folders.is_empty());
    ui.set_media_folders(strings(
        built.folders.iter().map(|f| f.as_str().into()).collect(),
    ));
    ui.set_media_sorts(strings(
        page.sorts().iter().map(|s| sort_name(*s).into()).collect(),
    ));
    ui.set_media_sort(page.sorts().iter().position(|s| *s == sort).unwrap_or(0) as i32);
    ui.set_media_has_head(built.head.is_some());
    if let Some(h) = &built.head {
        ui.set_media_head(card_to_ui(h));
    }
    VIEW.with_borrow_mut(|v| v.built = built);
    lay_out(ui);
    if let Some(y) = scroll {
        ui.set_media_scroll(y);
    }
}

/// Turn the page into lines for the window's width.
fn lay_out(ui: &AppWindow) {
    let (list, cards) = VIEW.with_borrow_mut(|v| {
        v.columns = columns(v.width);
        let (list, cards, songs, ranges) = lines(&v.built, v.columns);
        v.cards = Rc::new(Cards::new(cards));
        v.songs = songs;
        v.ranges = ranges;
        (list, v.cards.clone())
    });
    // The cards first: lines refer to them.
    ui.set_media_lines(ModelRc::default());
    ui.set_media_cards(ModelRc::from(cards as Rc<dyn Model<Data = MediaCard>>));
    ui.set_media_lines(ModelRc::new(VecModel::from(list)));
}

/// Go to a page, remembering this one for Back.
fn open(ui: &AppWindow, page: Page) {
    let scroll = ui.get_media_scroll();
    VIEW.with_borrow_mut(|v| {
        let old = std::mem::replace(&mut v.page, page);
        v.back.push((old, scroll));
    });
    load(ui, Some(0.0));
}

// ---- playing ----

/// Play these files from number `start`.
fn play(ui: &AppWindow, player: &Player, files: Vec<String>, start: usize) {
    let there = |f: &String| f.contains("://") || Path::new(f).exists();
    if files.get(start).is_none_or(|f| !there(f)) {
        ui.set_toast("That file isn't there now".into());
        return;
    }
    // Files that are gone don't shift the one to start with.
    let start = files[..start].iter().filter(|f| there(f)).count();
    let files: Vec<String> = files.into_iter().filter(there).collect();
    let video = files
        .get(start)
        .is_some_and(|f| !medialib::is_audio(Path::new(f)));
    let n = files.len();
    crate::open_inputs(ui, files.into_iter().map(input).collect(), false);
    // "93 items added to the playlist" isn't news after "Play".
    if n > 1 {
        ui.set_toast(SharedString::new());
    }
    if start > 0 && start < n {
        let _ = player.command(&["playlist-play-index", &start.to_string()]);
    }
    let _ = player.set_flag("pause", false);
    if video {
        ui.invoke_close_media();
    }
}

/// Play these files from the first one that is there.
fn play_first(ui: &AppWindow, player: &Player, files: Vec<String>) {
    let start = files
        .iter()
        .position(|f| Path::new(f).exists())
        .unwrap_or(0);
    play(ui, player, files, start);
}

fn input(path: String) -> Input {
    if path.contains("://") {
        Input::Url(path)
    } else {
        Input::Path(PathBuf::from(path))
    }
}

fn shuffled(mut list: Vec<String>) -> Vec<String> {
    // Seeded from the clock: good enough to mix songs.
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
        | 1;
    for i in (1..list.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        list.swap(i, (x % (i as u64 + 1)) as usize);
    }
    list
}

/// Play, shuffle or queue all of `page`.
fn play_page(ui: &AppWindow, player: &Player, page: &Page, how: &str) {
    let files = match Db::open().and_then(|db| playable(&db, page, sort_of(page))) {
        Ok(f) => f,
        Err(e) => {
            ui.set_toast(e.into());
            return;
        }
    };
    match how {
        "add" => crate::open_inputs(ui, files.into_iter().map(input).collect(), true),
        "shuffle" => play_first(ui, player, shuffled(files)),
        _ => play_first(ui, player, files),
    }
}

// ---- scanning ----

/// Scan the folders on a low-priority thread, then show what changed.
fn scan(ui: &AppWindow) {
    if SCANNING.swap(true, Ordering::AcqRel) {
        return;
    }
    VIEW.with_borrow_mut(|v| v.last_scan = Some(Instant::now()));
    ui.set_media_scanning(true);
    ui.set_media_status("Looking for changes…".into());
    let weak = ui.as_weak();
    let spawned = std::thread::Builder::new()
        .name("library-scan".into())
        .spawn(move || {
            art::low_priority();
            let mut last = Instant::now();
            let result = Db::open().and_then(|mut db| {
                db.scan(&mut |done, of| {
                    if of > 0 && last.elapsed() > Duration::from_millis(300) {
                        last = Instant::now();
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            ui.set_media_status(
                                format!("Reading {} of {of}…", done.min(of)).into(),
                            );
                        });
                    }
                    true
                })
            });
            SCANNING.store(false, Ordering::Release);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_media_scanning(false);
                match result {
                    Ok(s) => {
                        log::info!("library scan: {s:?}");
                        ui.set_media_status(match s.missing.as_slice() {
                            [] => SharedString::new(),
                            [one] => {
                                format!("{one} isn't there now (its drive may be unplugged)").into()
                            }
                            more => format!("{} folders aren't there now", more.len()).into(),
                        });
                        if ui.get_media_open() && (s.read > 0 || s.removed > 0) {
                            load(&ui, None);
                        }
                    }
                    Err(e) => ui.set_media_status(e.into()),
                }
            });
        });
    if let Err(e) = spawned {
        SCANNING.store(false, Ordering::Release);
        ui.set_media_scanning(false);
        log::warn!("library scan: {e}");
    }
}

fn add_folders(ui: &AppWindow, dirs: &[PathBuf]) {
    let result = Db::open().and_then(|db| dirs.iter().try_for_each(|d| db.add_folder(d)));
    if let Err(e) = result {
        ui.set_media_status(e.into());
        return;
    }
    load(ui, None);
    scan(ui);
}

/// The user's Music and Videos folders, those that exist.
fn default_folders() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let Some(home) = home else { return Vec::new() };
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default();
    [("XDG_MUSIC_DIR", "Music"), ("XDG_VIDEOS_DIR", "Videos")]
        .into_iter()
        .map(|(key, name)| user_dir(&text, key, &home).unwrap_or_else(|| home.join(name)))
        // An unset folder is the home folder itself: not all of that.
        .filter(|d| d.is_dir() && *d != home)
        .collect()
}

/// `XDG_MUSIC_DIR="$HOME/Music"` in user-dirs.dirs.
fn user_dir(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let value = text
        .lines()
        .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix('='))?;
    let value = value.trim().trim_matches('"');
    let dir = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    dir.is_absolute().then_some(dir)
}

// ---- callbacks ----

pub fn install(ui: &AppWindow, player: &Rc<Player>, settings: Rc<RefCell<Settings>>) {
    macro_rules! on {
        ($method:ident, |$ui:ident $(, $arg:ident)*| $body:block) => {{
            let weak = ui.as_weak();
            ui.$method(move |$($arg),*| {
                let Some($ui) = weak.upgrade() else { return };
                $body
            });
        }};
    }

    let weak = ui.as_weak();
    art::on_arrival(move |key| {
        let Some(ui) = weak.upgrade() else { return };
        let (cards, head) = VIEW.with_borrow(|v| (v.cards.clone(), v.built.head.clone()));
        cards.arrived(key);
        if let Some(h) = head.filter(|h| h.art.as_ref() == Some(key)) {
            ui.set_media_head(card_to_ui(&h));
        }
    });

    on!(on_media_shown, |ui| {
        let due = VIEW.with_borrow_mut(|v| {
            v.online = settings.borrow().artist_image_online;
            v.last_scan.is_none_or(|t| t.elapsed() > RESCAN_AFTER)
        });
        if due
            && Db::open()
                .and_then(|db| db.folders())
                .is_ok_and(|f| !f.is_empty())
        {
            scan(&ui);
        }
    });
    on!(on_media_closed, |ui| {
        // Free everything the library holds.
        REQUEST.set(REQUEST.get() + 1);
        VIEW.with_borrow_mut(|v| {
            v.built = Built::default();
            v.back.clear();
            v.songs = Vec::new();
            v.ranges = Vec::new();
            v.cards = Rc::new(Cards::new(Vec::new()));
        });
        ui.set_media_lines(ModelRc::default());
        ui.set_media_cards(ModelRc::default());
        ui.set_media_head(MediaCard::default());
        ui.set_media_query(SharedString::new());
        art::clear();
    });
    on!(on_media_nav, |ui, tab| {
        VIEW.with_borrow_mut(|v| {
            v.page = Page::of_tab(tab);
            v.back.clear();
        });
        ui.set_media_query(SharedString::new());
        load(&ui, Some(0.0));
    });
    on!(on_media_back, |ui| {
        let Some(scroll) = VIEW.with_borrow_mut(|v| {
            let (page, scroll) = v.back.pop()?;
            v.page = page;
            Some(scroll)
        }) else {
            return;
        };
        let text = VIEW.with_borrow(|v| {
            if let Page::Search(t) = &v.page {
                t.clone()
            } else {
                String::new()
            }
        });
        ui.set_media_query(text.into());
        load(&ui, Some(scroll));
    });
    on!(on_media_search, |ui, text| {
        let weak = ui.as_weak();
        // Wait for a pause in the typing.
        SEARCH_TIMER.with(|t| {
            t.start(
                TimerMode::SingleShot,
                Duration::from_millis(140),
                move || {
                    let Some(ui) = weak.upgrade() else { return };
                    if !ui.get_media_open() {
                        return;
                    }
                    let searching = VIEW.with_borrow(|v| matches!(v.page, Page::Search(_)));
                    let page = Page::Search(text.to_string());
                    match (text.trim().is_empty(), searching) {
                        (true, true) => ui.invoke_media_back(),
                        (true, false) => {}
                        (false, true) => {
                            VIEW.with_borrow_mut(|v| v.page = page);
                            load(&ui, Some(0.0));
                        }
                        (false, false) => open(&ui, page),
                    }
                },
            );
        });
    });
    on!(on_media_sort_by, |ui, i| {
        VIEW.with_borrow_mut(|v| {
            let tab = v.page.tab();
            if let Some(s) = v.page.sorts().get(i.max(0) as usize).copied() {
                v.sorts.retain(|(t, _)| *t != tab);
                v.sorts.push((tab, s));
            }
        });
        load(&ui, Some(0.0));
    });
    on!(on_media_layout, |ui, width| {
        let changed = VIEW.with_borrow_mut(|v| {
            if width > 50.0 {
                v.width = width;
            }
            columns(v.width) != v.columns
        });
        if changed {
            // Not while the list is being laid out.
            let weak = ui.as_weak();
            Timer::single_shot(Duration::ZERO, move || {
                if let Some(ui) = weak.upgrade() {
                    lay_out(&ui);
                }
            });
        }
    });

    let card_at =
        |i: i32| VIEW.with_borrow(|v| v.cards.list.borrow().get(i.max(0) as usize).cloned());
    let p = player.clone();
    on!(on_media_open_card, |ui, i| {
        match card_at(i).map(|c| c.target) {
            Some(Target::Page(page)) => open(&ui, page),
            Some(Target::File(f)) => play(&ui, &p, vec![f], 0),
            None => {}
        }
    });
    let p = player.clone();
    on!(on_media_play_card, |ui, i| {
        match card_at(i).map(|c| c.target) {
            Some(Target::Page(page)) => play_page(&ui, &p, &page, "play"),
            Some(Target::File(f)) => play(&ui, &p, vec![f], 0),
            None => {}
        }
    });
    on!(on_media_remove_card, |ui, i| {
        if let Some(Target::File(f)) = card_at(i).filter(|c| c.removable).map(|c| c.target) {
            resume::forget(&f);
            load(&ui, None);
        }
    });
    let p = player.clone();
    on!(on_media_play_song, |ui, i| {
        let i = i.max(0) as usize;
        // The song's own list from that song on.
        let (files, start) = VIEW.with_borrow(|v| {
            let r = v
                .ranges
                .iter()
                .find(|r| r.contains(&i))
                .cloned()
                .unwrap_or(0..0);
            (
                v.songs[r.clone()]
                    .iter()
                    .map(|t| t.path.clone())
                    .collect::<Vec<_>>(),
                i.saturating_sub(r.start),
            )
        });
        play(&ui, &p, files, start);
    });
    on!(on_media_add_song, |ui, i| {
        if let Some(t) = VIEW.with_borrow(|v| v.songs.get(i.max(0) as usize).cloned()) {
            crate::open_inputs(&ui, vec![input(t.path)], true);
        }
    });
    let p = player.clone();
    on!(on_media_play_all, |ui, how| {
        let page = VIEW.with_borrow(|v| v.page.clone());
        play_page(&ui, &p, &page, &how);
    });

    on!(on_media_add_folder, |ui| {
        if let Some(dir) = rfd::FileDialog::new()
            .set_title("Add a folder to the library")
            .pick_folder()
        {
            add_folders(&ui, &[dir]);
        }
    });
    on!(on_media_add_defaults, |ui| {
        let dirs = default_folders();
        if dirs.is_empty() {
            ui.set_media_status("No Music or Videos folder was found; choose a folder".into());
        } else {
            add_folders(&ui, &dirs);
        }
    });
    on!(on_media_remove_folder, |ui, i| {
        if SCANNING.load(Ordering::Acquire) {
            ui.set_toast("Please wait for the scan to finish".into());
            return;
        }
        let Some(dir) = ui.get_media_folders().row_data(i.max(0) as usize) else {
            return;
        };
        if let Err(e) = Db::open().and_then(|mut db| db.remove_folder(&dir)) {
            ui.set_media_status(e.into());
        }
        load(&ui, None);
    });
    on!(on_media_rescan, |ui| {
        scan(&ui);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(path: &str, track: u32) -> Track {
        Track {
            path: path.into(),
            title: path.into(),
            track,
            dir: "/m/Folder".into(),
            duration: 65.0,
            ..Default::default()
        }
    }

    #[test]
    fn pages_become_lines() {
        assert_eq!(columns(820.0), (4, 3));
        assert_eq!(columns(60.0), (2, 1));
        let card = |t: &str| plain(t, String::new(), Target::File(t.into()));
        let built = Built {
            sections: vec![
                Section::Heading("Albums".into()),
                Section::Cards((0..5).map(|i| card(&i.to_string())).collect()),
                Section::Songs(vec![track("/a", 0), track("/b", 0)]),
                Section::Cards(vec![Card {
                    shape: 2,
                    ..card("w")
                }]),
                Section::Songs(vec![track("/c", 0)]),
            ],
            ..Default::default()
        };
        let (lines, cards, songs, ranges) = lines(&built, (4, 3));
        let kinds: Vec<i32> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, [0, 1, 1, 2, 2, 1, 2]);
        assert_eq!(
            (lines[2].first, lines[2].count, lines[2].columns),
            (4, 1, 4)
        );
        assert_eq!(
            (lines[5].first, lines[5].columns, lines[5].shape),
            (5, 3, 2)
        );
        assert_eq!(lines[6].first, 2);
        assert_eq!(
            (lines[3].detail.as_str(), lines[3].right.as_str()),
            ("Folder", "1:05")
        );
        assert_eq!((cards.len(), songs.len()), (6, 3));
        assert_eq!(ranges, [0..2, 2..3]);
    }

    #[test]
    fn progress_and_lengths() {
        let mut c = plain("x", String::new(), Target::File("/x".into()));
        c.left_at = Some(600.0);
        set_duration(&mut c, 2400.0);
        assert_eq!((c.progress, c.corner.as_str()), (0.25, "30:00 left"));
        let list = [track("/a", 1), track("/b", 2)];
        assert_eq!(total_time(&list), "2 min");
        assert_eq!(join(&["a".into(), String::new(), "b".into()]), "a · b");
        let mut s = shuffled((0..50).map(|i| i.to_string()).collect());
        s.sort_by_key(|x| x.parse::<u32>().unwrap());
        assert_eq!(s.len(), 50);
        assert_eq!(s[49], "49");
    }

    #[test]
    fn user_dirs() {
        let t = "# x\nXDG_MUSIC_DIR=\"$HOME/Muzica\"\nXDG_VIDEOS_DIR=\"/mnt/v\"\n";
        let h = Path::new("/home/a");
        assert_eq!(
            user_dir(t, "XDG_MUSIC_DIR", h),
            Some("/home/a/Muzica".into())
        );
        assert_eq!(user_dir(t, "XDG_VIDEOS_DIR", h), Some("/mnt/v".into()));
        assert_eq!(user_dir(t, "XDG_X", h), None);
    }
}
