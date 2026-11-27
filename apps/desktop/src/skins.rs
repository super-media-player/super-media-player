//! Skins: colour schemes for the player's views, read from small text files.
//!
//! The views themselves (layouts) are Slint components: the media player,
//! compact, classic deck and silver deck. Each takes its colours from a
//! Slint global (`Theme`, `Classic`, `Silver`) whose properties can be set
//! at run time, and a skin is a list of such colours:
//!
//! ```text
//! name = Silver deck – Midnight
//! layout = silver
//! case-1 = #4a4f57
//! theme.accent = #39e0ff
//! ```
//!
//! `layout = any` makes a colour theme: it colours the media player and
//! compact views (and every dialog) without switching between them.
//!
//! Unprefixed names belong to the layout's own palette; `theme.`,
//! `classic.` and `silver.` name a palette outright. Gradients are built in
//! Slint from numbered stop colours (`case-1`..`case-4`), so a skin only
//! deals in colours. Built-in skins live in `apps/desktop/skins/`, the
//! user's in `~/.config/super-media-player/skins/`. See SKINS.md.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use slint::{Color, ComponentHandle, SharedString};

use crate::{AppWindow, Classic, PlaylistWindow, Silver, Theme};

/// The plain views, in view order (`settings::VIEW_MODES`): skin id = view
/// name, no colour changes.
pub const PLAIN: [(&str, &str); 4] = [
    ("player", "Media player"),
    ("compact", "Compact"),
    ("classic", "Classic deck (Winamp-style)"),
    ("silver", "Silver deck (MMD3-style)"),
];

/// Skins and themes that ship with the player: (id, file).
const BUILT_IN: &[(&str, &str)] = &[
    ("arctic", include_str!("../skins/arctic.skin")),
    ("aurora", include_str!("../skins/aurora.skin")),
    ("blossom", include_str!("../skins/blossom.skin")),
    ("ember", include_str!("../skins/ember.skin")),
    ("espresso", include_str!("../skins/espresso.skin")),
    ("flamingo", include_str!("../skins/flamingo.skin")),
    ("forest", include_str!("../skins/forest.skin")),
    ("graphite", include_str!("../skins/graphite.skin")),
    ("jade", include_str!("../skins/jade.skin")),
    ("mint", include_str!("../skins/mint.skin")),
    ("nebula", include_str!("../skins/nebula.skin")),
    ("ocean", include_str!("../skins/ocean.skin")),
    ("rose", include_str!("../skins/rose.skin")),
    ("sapphire", include_str!("../skins/sapphire.skin")),
    ("sky", include_str!("../skins/sky.skin")),
    ("slate", include_str!("../skins/slate.skin")),
    ("steel", include_str!("../skins/steel.skin")),
    ("sunset", include_str!("../skins/sunset.skin")),
    ("amber", include_str!("../skins/amber.skin")),
    ("ice", include_str!("../skins/ice.skin")),
    ("midnight", include_str!("../skins/midnight.skin")),
    ("rose-gold", include_str!("../skins/rose-gold.skin")),
];

/// Written into the user's skins folder.
const README: &str = include_str!("../skins/README.txt");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Palette {
    Theme,
    Classic,
    Silver,
}

impl Palette {
    const ALL: [Palette; 3] = [Palette::Theme, Palette::Classic, Palette::Silver];

    fn name(self) -> &'static str {
        match self {
            Palette::Theme => "theme",
            Palette::Classic => "classic",
            Palette::Silver => "silver",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The palette a layout (view) is coloured by.
    fn of_layout(layout: &str) -> Self {
        match layout {
            "classic" => Palette::Classic,
            "silver" => Palette::Silver,
            _ => Palette::Theme,
        }
    }

    fn props(self) -> Vec<Prop<AppWindow>> {
        match self {
            Palette::Theme => theme_props(),
            Palette::Classic => classic_props(),
            Palette::Silver => silver_props(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Color(Color),
    Str(SharedString),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Color,
    Str,
}

/// One settable property of a palette global, for component type `C`.
pub struct Prop<C> {
    pub key: &'static str,
    pub kind: Kind,
    get: fn(&C) -> Val,
    set: fn(&C, &Val),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skin {
    /// File name without ".skin" (or the view name for the plain views).
    pub id: String,
    pub name: String,
    /// The view it dresses: one of `settings::VIEW_MODES`.
    pub layout: String,
    pub author: String,
    pub colors: Vec<(Palette, String, Val)>,
    /// Where it was read from (None: built in).
    pub path: Option<PathBuf>,
}

impl Skin {
    fn plain(id: &str, name: &str) -> Self {
        Skin {
            id: id.into(),
            name: name.into(),
            layout: id.into(),
            author: String::new(),
            colors: Vec::new(),
            path: None,
        }
    }

    /// Index of its layout in `settings::VIEW_MODES`; -1 for a theme
    /// (`layout = any`), which keeps the view.
    pub fn view_index(&self) -> i32 {
        if self.is_theme() {
            return -1;
        }
        crate::settings::VIEW_MODES
            .iter()
            .position(|v| *v == self.layout)
            .unwrap_or(0) as i32
    }

    /// A colour theme for the media player and compact views.
    pub fn is_theme(&self) -> bool {
        self.layout == ANY
    }

    /// Whether it can dress `view`: its own layout, or a theme on the
    /// media player or compact view.
    pub fn fits(&self, view: &str) -> bool {
        self.layout == view || (self.is_theme() && Palette::of_layout(view) == Palette::Theme)
    }
}

/// The layout of a colour theme.
pub const ANY: &str = "any";

/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` or `transparent`.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("transparent") {
        return Some(Color::from_argb_u8(0, 0, 0, 0));
    }
    let hex = s.strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let digits: Vec<u8> = match hex.len() {
        3 | 4 => hex
            .chars()
            .map(|c| {
                let d = c.to_digit(16).unwrap() as u8;
                d * 17
            })
            .collect(),
        6 | 8 => (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect(),
        _ => return None,
    };
    let a = digits.get(3).copied().unwrap_or(255);
    Some(Color::from_argb_u8(a, digits[0], digits[1], digits[2]))
}

fn color_text(c: Color) -> String {
    if c.alpha() == 255 {
        format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue())
    } else {
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            c.red(),
            c.green(),
            c.blue(),
            c.alpha()
        )
    }
}

/// Read a skin file. Mistakes in single lines are skipped and reported in
/// the warnings; only a missing or unknown layout rejects the file.
pub fn parse(id: &str, text: &str) -> Result<(Skin, Vec<String>), String> {
    let mut skin = Skin {
        id: id.into(),
        name: id.into(),
        layout: String::new(),
        author: String::new(),
        colors: Vec::new(),
        path: None,
    };
    let mut warnings = Vec::new();
    let mut entries = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            warnings.push(format!("line {}: expected name = value", n + 1));
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "name" => skin.name = v.into(),
            "layout" => skin.layout = v.into(),
            "author" => skin.author = v.into(),
            "description" => {}
            _ => entries.push((n + 1, k.to_owned(), v.to_owned())),
        }
    }
    if skin.layout != ANY && !crate::settings::VIEW_MODES.contains(&skin.layout.as_str()) {
        return Err(format!(
            "layout must be one of {} or {ANY}",
            crate::settings::VIEW_MODES.join(", ")
        ));
    }
    let own = Palette::of_layout(&skin.layout);
    for (line, key, value) in entries {
        let (palette, name) = match key.split_once('.') {
            Some((p, name)) => match Palette::from_name(p) {
                Some(p) => (p, name),
                None => {
                    warnings.push(format!("line {line}: no colour set called \"{p}\""));
                    continue;
                }
            },
            None => (own, key.as_str()),
        };
        let props = palette.props();
        let Some(prop) = props.iter().find(|p| p.key == name) else {
            warnings.push(format!(
                "line {line}: {} has no colour \"{name}\"",
                palette.name()
            ));
            continue;
        };
        let val = match prop.kind {
            Kind::Color => match parse_color(&value) {
                Some(c) => Val::Color(c),
                None => {
                    warnings.push(format!(
                        "line {line}: \"{value}\" is not a colour like #12abef"
                    ));
                    continue;
                }
            },
            Kind::Str => Val::Str(value.trim_matches('"').into()),
        };
        skin.colors.push((palette, prop.key.to_owned(), val));
    }
    Ok((skin, warnings))
}

/// The user's skins folder.
pub fn user_dir() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("skins"))
}

/// All skins: the plain views first (in view order), then the others by
/// layout and name. A user skin with a built-in skin's id replaces it.
/// Also returns what was wrong with the files.
pub fn load_all() -> (Vec<Skin>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut extra: Vec<Skin> = Vec::new();
    for (id, text) in BUILT_IN {
        match parse(id, text) {
            Ok((s, w)) => {
                debug_assert!(w.is_empty(), "built-in skin {id}: {w:?}");
                extra.push(s);
            }
            Err(e) => log::warn!("built-in skin {id}: {e}"),
        }
    }
    if let Some(dir) = user_dir() {
        for (skin, w) in read_dir(&dir, &mut warnings) {
            warnings.extend(w.into_iter().map(|w| format!("{}.skin, {w}", skin.id)));
            if PLAIN.iter().any(|(id, _)| *id == skin.id) {
                warnings.push(format!(
                    "{}.skin: that name is taken by a view; rename the file",
                    skin.id
                ));
                continue;
            }
            extra.retain(|s| s.id != skin.id);
            extra.push(skin);
        }
    }
    // Themes right after the plain views, then the skins of each view.
    extra.sort_by(|a, b| (a.view_index(), &a.name).cmp(&(b.view_index(), &b.name)));
    let mut all: Vec<Skin> = PLAIN
        .iter()
        .map(|(id, name)| Skin::plain(id, name))
        .collect();
    all.extend(extra);
    (all, warnings)
}

fn read_dir(dir: &Path, warnings: &mut Vec<String>) -> Vec<(Skin, Vec<String>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "skin") && p.is_file())
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        let id = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                warnings.push(format!("{id}.skin: {e}"));
                continue;
            }
        };
        match parse(&id, &text) {
            Ok((mut skin, w)) => {
                skin.path = Some(path);
                out.push((skin, w));
            }
            Err(e) => warnings.push(format!("{id}.skin: {e}")),
        }
    }
    out
}

/// Colours by palette and name.
type Colours = Vec<(Palette, &'static str, Val)>;

thread_local! {
    /// Every palette colour as the .slint files define it, read before the
    /// first skin changes any.
    static DEFAULTS: RefCell<Option<Colours>> = const { RefCell::new(None) };
    /// Counts colour changes, for windows that copy the colours.
    static CHANGES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Changes when any colour (or the text size) changes.
pub fn changes() -> u64 {
    CHANGES.get()
}

/// Note a change of colours or text size.
pub fn changed() {
    CHANGES.set(CHANGES.get().wrapping_add(1));
}

fn defaults(ui: &AppWindow) -> Colours {
    DEFAULTS.with_borrow_mut(|d| {
        d.get_or_insert_with(|| {
            Palette::ALL
                .into_iter()
                .flat_map(|p| {
                    p.props()
                        .into_iter()
                        .map(move |prop| (p, prop.key, (prop.get)(ui)))
                })
                .collect()
        })
        .clone()
    })
}

/// Dress the windows in `skin`: every colour back to its default, then the
/// skin's. The decks' own playlist window (`pw`) follows along.
pub fn apply(ui: &AppWindow, pw: Option<&PlaylistWindow>, skin: &Skin) {
    let defaults = defaults(ui);
    for p in Palette::ALL {
        for prop in &p.props() {
            if let Some(v) = resolved(skin, &defaults, p, prop.key) {
                (prop.set)(ui, &v);
            }
        }
    }
    if let Some(pw) = pw {
        copy_to_playlist_window(ui, pw);
    }
    changed();
}

/// Colours that a skin which doesn't set them takes from another of its
/// colours: they were that one colour before they got their own.
const FOLLOWS: &[(Palette, &str, &str)] = &[
    (Palette::Theme, "title-bar", "bg-2"),
    (Palette::Theme, "controls", "surface"),
    (Palette::Theme, "playlist", "surface-hi"),
    (Palette::Theme, "nav", "bg-2"),
];

/// What `skin` makes colour `key` of palette `p`: its own value, else the
/// value of the colour it follows, else the default.
fn resolved(skin: &Skin, defaults: &Colours, p: Palette, key: &str) -> Option<Val> {
    let own = |key: &str| {
        skin.colors
            .iter()
            .rev()
            .find(|(sp, k, _)| *sp == p && k == key)
            .map(|(_, _, v)| v.clone())
    };
    own(key)
        .or_else(|| {
            FOLLOWS
                .iter()
                .find(|(fp, k, _)| *fp == p && *k == key)
                .and_then(|(_, _, from)| own(from))
        })
        .or_else(|| {
            defaults
                .iter()
                .find(|(dp, k, _)| *dp == p && *k == key)
                .map(|(_, _, v)| v.clone())
        })
}

// ---- the colour editor (colours.slint) ----

/// The palettes the editor shows for a layout: its own, then (for the
/// decks) the theme, which colours the dialogs over them.
fn edited_palettes(layout: &str) -> Vec<Palette> {
    let own = Palette::of_layout(layout);
    if own == Palette::Theme {
        vec![Palette::Theme]
    } else {
        vec![own, Palette::Theme]
    }
}

/// A plain name for a colour, for the editor.
fn colour_label(p: Palette, key: &str) -> String {
    let named = match (p, key) {
        (Palette::Theme, "bg") => "Window background",
        (Palette::Theme, "bg-2") => "Background glow",
        (Palette::Theme, "surface") => "Panels and bars",
        (Palette::Theme, "surface-hi") => "Raised panels, small popups",
        (Palette::Theme, "border") => "Lines and edges",
        (Palette::Theme, "text") => "Text",
        (Palette::Theme, "text-dim") => "Dim text",
        (Palette::Theme, "text-faint") => "Faint text",
        (Palette::Theme, "accent") => "Accent",
        (Palette::Theme, "accent-2") => "Second accent",
        (Palette::Theme, "danger") => "Warnings",
        (Palette::Theme, "panel") => "Dialogs (settings, library…)",
        (Palette::Theme, "button") => "Buttons",
        (Palette::Theme, "button-hover") => "Buttons under the pointer",
        (Palette::Theme, "button-text") => "Button text",
        (Palette::Theme, "popup") => "Drop-down lists",
        (Palette::Theme, "on-accent") => "Text and icons on the accent",
        (Palette::Theme, "title-bar") => "Title bar",
        (Palette::Theme, "controls") => "Control bar",
        (Palette::Theme, "playlist") => "Playlist",
        (Palette::Theme, "nav") => "Navigation window",
        (Palette::Classic, "outside") | (Palette::Silver, "outside") => "Around the deck",
        (Palette::Classic, "lcd") => "Screen",
        (Palette::Classic, "lcd-edge") => "Screen edge",
        (Palette::Classic, "green") => "Screen text",
        (Palette::Classic, "green-dim") => "Screen text, dim",
        (Palette::Classic, "label") => "Labels",
        (Palette::Classic, "led-off") => "Lights when off",
        (Palette::Silver, "led") => "Lights",
        (Palette::Silver, "led-off") => "Lights when off",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_owned();
    }
    // "button-pressed-1" -> "Button pressed 1"
    let mut words = key.replace('-', " ");
    if let Some(first) = words.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    words
}

/// One line of the editor: (key such as "theme.accent", label, colour);
/// a heading has an empty key and no colour.
pub type EditorLine = (String, String, Option<Color>);

/// The editor's lines for a layout, with the colours shown now.
pub fn editor_lines(ui: &AppWindow, layout: &str) -> Vec<EditorLine> {
    let mut lines = Vec::new();
    for p in edited_palettes(layout) {
        let heading = match p {
            Palette::Theme if Palette::of_layout(layout) == Palette::Theme => "COLOURS",
            Palette::Theme => "DIALOGS, BUTTONS AND LISTS",
            Palette::Classic => "CLASSIC DECK",
            Palette::Silver => "SILVER DECK",
        };
        lines.push((String::new(), heading.to_owned(), None));
        for prop in p.props() {
            if let Val::Color(c) = (prop.get)(ui) {
                lines.push((
                    format!("{}.{}", p.name(), prop.key),
                    colour_label(p, prop.key),
                    Some(c),
                ));
            }
        }
    }
    lines
}

/// Set one colour by its editor key ("silver.case-1"). False for an
/// unknown key.
pub fn set_colour(ui: &AppWindow, key: &str, colour: Color) -> bool {
    let Some((p, name)) = key.split_once('.') else {
        return false;
    };
    let Some(p) = Palette::from_name(p) else {
        return false;
    };
    match p.props().iter().find(|prop| prop.key == name) {
        Some(prop) if prop.kind == Kind::Color => {
            (prop.set)(ui, &Val::Color(colour));
            changed();
            true
        }
        _ => false,
    }
}

/// "#39d0ff" (or "#39d0ff80" when see-through).
pub fn hex(c: Color) -> String {
    color_text(c)
}

/// Save the colours shown now as a skin file: over `skin`'s own file when
/// it is one of the user's, else as a new "My …" copy. Only colours that
/// differ from the layout's own are written. Returns the saved skin's id
/// and name.
pub fn save_shown(ui: &AppWindow, skin: &Skin) -> std::io::Result<(String, String)> {
    let dir = user_dir().ok_or_else(|| std::io::Error::other("no config folder"))?;
    std::fs::create_dir_all(&dir)?;
    // The media player's and compact's colours are saved as a theme, for
    // both views.
    let theme = Palette::of_layout(&skin.layout) == Palette::Theme;
    let (id, name, path) = match &skin.path {
        Some(path) if path.parent() == Some(dir.as_path()) => {
            (skin.id.clone(), skin.name.clone(), path.clone())
        }
        _ => {
            let plain = skin.path.is_none() && skin.colors.is_empty();
            let (id, name) = if theme && plain {
                ("my-theme".to_owned(), "My theme".to_owned())
            } else {
                (format!("my-{}", skin.id), format!("My {}", skin.name))
            };
            let path = dir.join(format!("{id}.skin"));
            (id, name, path)
        }
    };
    let mut skin = skin.clone();
    if theme {
        skin.layout = ANY.to_owned();
    }
    let text = shown_skin_text(ui, &skin, &name);
    std::fs::write(&path, text)?;
    Ok((id, name))
}

fn shown_skin_text(ui: &AppWindow, skin: &Skin, name: &str) -> String {
    let defaults = defaults(ui);
    let own = Palette::of_layout(&skin.layout);
    let edited = edited_palettes(&skin.layout);
    let key_of = |p: Palette, key: &str| {
        if p == own {
            key.to_owned()
        } else {
            format!("{}.{key}", p.name())
        }
    };
    let mut text = format!(
        "# Made with Super Media Player's Looks panel (View › Themes and colours).\n\
         name = {name}\nlayout = {}\n",
        skin.layout
    );
    if !skin.author.is_empty() {
        text += &format!(
            "author = {}
",
            skin.author
        );
    }
    text += "
";
    for &p in &edited {
        for prop in p.props() {
            let shown = (prop.get)(ui);
            match &shown {
                Val::Color(c) => {
                    let default = defaults
                        .iter()
                        .find(|(dp, k, _)| *dp == p && *k == prop.key)
                        .map(|(_, _, v)| v);
                    if default != Some(&shown) {
                        text += &format!("{} = {}\n", key_of(p, prop.key), color_text(*c));
                    }
                }
                Val::Str(_) => {
                    // Fonts aren't edited: keep the skin's own.
                    if let Some((_, _, Val::Str(v))) = skin
                        .colors
                        .iter()
                        .rev()
                        .find(|(sp, k, _)| *sp == p && k == prop.key)
                    {
                        text += &format!("{} = {v}\n", key_of(p, prop.key));
                    }
                }
            }
        }
    }
    // Colours the skin gave other palettes, untouched by the editor.
    for (p, key, v) in &skin.colors {
        if edited.contains(p) {
            continue;
        }
        match v {
            Val::Color(c) => text += &format!("{} = {}\n", key_of(*p, key), color_text(*c)),
            Val::Str(s) => text += &format!("{} = {s}\n", key_of(*p, key)),
        }
    }
    text
}

// ---- the Looks panel: theme cards and simple colours ----

/// A theme's colours for its little picture in the Looks panel.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub ground: Color,
    pub top: Color,
    pub panel: Color,
    pub bar: Color,
    pub text: Color,
    pub accent: Color,
    pub accent_2: Color,
}

fn colour_of(skin: &Skin, defaults: &Colours, p: Palette, key: &str) -> Color {
    match resolved(skin, defaults, p, key) {
        Some(Val::Color(c)) => c,
        _ => Color::default(),
    }
}

/// What the Looks panel offers for `view`, as (a theme?, index in `themes`
/// or `skins`, card): the plain view first, then the themes (for the media
/// player and compact), then the view's own skins.
pub fn cards(
    ui: &AppWindow,
    skins: &[Skin],
    themes: &[Skin],
    view: &str,
) -> Vec<(bool, usize, Card)> {
    let defaults = defaults(ui);
    let p = Palette::of_layout(view);
    let card = |skin: &Skin| {
        let c = |p: Palette, key: &str| colour_of(skin, &defaults, p, key);
        match p {
            Palette::Theme => Card {
                ground: c(p, "bg"),
                top: c(p, "title-bar"),
                panel: c(p, "playlist"),
                bar: c(p, "controls"),
                text: c(p, "text"),
                accent: c(p, "accent"),
                accent_2: c(p, "accent-2"),
            },
            Palette::Classic => Card {
                ground: c(p, "outside"),
                top: c(p, "body-1"),
                panel: c(p, "lcd"),
                bar: c(p, "body-3"),
                text: c(p, "green"),
                accent: c(p, "green"),
                accent_2: c(p, "green-dim"),
            },
            Palette::Silver => Card {
                ground: c(p, "outside"),
                top: c(p, "case-1"),
                panel: c(p, "glass-1"),
                bar: c(p, "case-3"),
                text: c(p, "text"),
                accent: c(p, "led"),
                accent_2: c(p, "accent"),
            },
        }
    };
    let mut out: Vec<(bool, usize, Card)> = skins
        .iter()
        .enumerate()
        .filter(|(_, s)| s.id == view && s.colors.is_empty() && s.path.is_none())
        .map(|(i, s)| (false, i, card(s)))
        .collect();
    out.extend(
        themes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.fits(view))
            .map(|(i, s)| (true, i, card(s))),
    );
    out.extend(
        skins
            .iter()
            .enumerate()
            .filter(|(_, s)| s.layout == view && s.id != view)
            .map(|(i, s)| (false, i, card(s))),
    );
    out
}

/// The Looks panel's short list of colours for a layout: (label, palette,
/// colours). Changing one changes the first colour and moves the others
/// along with it, so their shades keep their places.
fn simple_groups(layout: &str) -> Vec<(&'static str, Palette, &'static [&'static str])> {
    use Palette::*;
    let dialogs = ("Settings and other panels", Theme, &["panel", "popup"][..]);
    match layout {
        "classic" => vec![
            ("Around the deck", Classic, &["outside"][..]),
            (
                "Deck",
                Classic,
                &[
                    "body-1",
                    "body-2",
                    "body-3",
                    "body-edge",
                    "ridge-light",
                    "ridge-dark",
                ],
            ),
            ("Screen", Classic, &["lcd", "lcd-edge", "playlist-hover"]),
            (
                "Screen text",
                Classic,
                &["green", "green-dim", "playlist-current"],
            ),
            ("Labels", Classic, &["label"]),
            (
                "Buttons",
                Classic,
                &[
                    "button-1",
                    "button-2",
                    "button-3",
                    "button-pressed-1",
                    "button-pressed-2",
                    "button-edge",
                    "button-edge-pressed",
                    "toggle-1",
                    "toggle-2",
                    "toggle-pressed",
                    "toggle-edge",
                ],
            ),
            dialogs,
        ],
        "silver" => vec![
            ("Around the deck", Silver, &["outside"][..]),
            (
                "Case",
                Silver,
                &[
                    "case-1",
                    "case-2",
                    "case-3",
                    "case-4",
                    "case-edge",
                    "bezel-1",
                    "bezel-2",
                    "bezel-3",
                    "bezel-edge",
                ],
            ),
            (
                "Screen",
                Silver,
                &[
                    "glass-1",
                    "glass-2",
                    "glass-3",
                    "glass-edge",
                    "playlist-1",
                    "playlist-2",
                ],
            ),
            (
                "Screen text",
                Silver,
                &["text", "text-dim", "text-off", "glass-mark"],
            ),
            (
                "Lights",
                Silver,
                &["led", "led-glow", "led-edge", "accent", "accent-2"],
            ),
            (
                "Buttons",
                Silver,
                &[
                    "chrome-1",
                    "chrome-2",
                    "chrome-3",
                    "chrome-pressed-1",
                    "chrome-pressed-2",
                    "chrome-pressed-3",
                    "chrome-edge",
                ],
            ),
            dialogs,
        ],
        "compact" => vec![
            ("Background", Theme, &["bg", "bg-2"][..]),
            ("Text", Theme, &["text", "text-dim", "text-faint"]),
            ("Highlight", Theme, &["accent", "accent-2"]),
            dialogs,
        ],
        _ => vec![
            ("Background", Theme, &["bg", "bg-2"][..]),
            ("Title bar", Theme, &["title-bar"]),
            ("Control bar", Theme, &["controls"]),
            ("Playlist", Theme, &["playlist"]),
            ("Navigation window", Theme, &["nav"]),
            dialogs,
            ("Text", Theme, &["text", "text-dim", "text-faint"]),
            ("Highlight", Theme, &["accent", "accent-2"]),
        ],
    }
}

fn shown(ui: &AppWindow, p: Palette, key: &str) -> Option<Color> {
    match p
        .props()
        .iter()
        .find(|prop| prop.key == key)
        .map(|prop| (prop.get)(ui))
    {
        Some(Val::Color(c)) => Some(c),
        _ => None,
    }
}

/// The short list for a layout, with the colours shown now.
pub fn simple_lines(ui: &AppWindow, layout: &str) -> Vec<(String, Color)> {
    simple_groups(layout)
        .into_iter()
        .map(|(label, p, keys)| (label.to_owned(), shown(ui, p, keys[0]).unwrap_or_default()))
        .collect()
}

/// The colours that change when line `index` of the short list becomes
/// `colour`, as (editor key, new colour).
pub fn simple_change(
    ui: &AppWindow,
    layout: &str,
    index: usize,
    colour: Color,
) -> Vec<(String, Color)> {
    let Some((_, p, keys)) = simple_groups(layout).into_iter().nth(index) else {
        return Vec::new();
    };
    let Some(leader) = shown(ui, p, keys[0]) else {
        return Vec::new();
    };
    keys.iter()
        .filter_map(|key| {
            let now = shown(ui, p, key)?;
            Some((
                format!("{}.{key}", p.name()),
                shade_along(leader, colour, now),
            ))
        })
        .collect()
}

/// `member` moved along when `leader` becomes `to`: the same steps of
/// hue, strength and brightness away from it, and its own opacity. Steps of
/// brightness turn round when the colour goes from dark to light (or back),
/// so dim text stays between the text and the background.
fn shade_along(leader: Color, to: Color, member: Color) -> Color {
    let (l, t, m) = (leader.to_hsva(), to.to_hsva(), member.to_hsva());
    let mut dv = m.value - l.value;
    if (l.value >= 0.5) != (t.value >= 0.5) {
        dv = -dv;
    }
    // A grey leader says nothing about hue: take the new one.
    let hue = if l.saturation < 0.08 || m.saturation < 0.08 {
        t.hue
    } else {
        t.hue + m.hue - l.hue
    };
    let mut sat = (t.saturation + m.saturation - l.saturation).clamp(0.0, 1.0);
    if t.saturation < 0.08 {
        // Greys stay greys.
        sat = sat.min(t.saturation + 0.03);
    }
    Color::from_hsva(
        hue.rem_euclid(360.0),
        sat,
        (t.value + dv).clamp(0.0, 1.0),
        m.alpha,
    )
}

/// The decks' own playlist window takes the decks' colours.
pub fn copy_to_playlist_window(ui: &AppWindow, pw: &PlaylistWindow) {
    let pairs = [
        (
            classic_props::<PlaylistWindow>(),
            classic_props::<AppWindow>(),
        ),
        (
            silver_props::<PlaylistWindow>(),
            silver_props::<AppWindow>(),
        ),
    ];
    for (to, from) in pairs {
        for prop in to {
            if let Some(src) = from.iter().find(|p| p.key == prop.key) {
                (prop.set)(pw, &(src.get)(ui));
            }
        }
    }
}

/// Make the user's skins folder, with the README, the built-in skins as
/// examples, and one file per layout listing every colour it has (with
/// its default), to copy from. Returns the folder.
pub fn prepare_user_dir(ui: &AppWindow) -> std::io::Result<PathBuf> {
    let dir = user_dir().ok_or_else(|| std::io::Error::other("no config folder"))?;
    let examples = dir.join("examples");
    std::fs::create_dir_all(&examples)?;
    std::fs::write(dir.join("README.txt"), README)?;
    for (id, text) in BUILT_IN {
        std::fs::write(examples.join(format!("{id}.skin")), text)?;
    }
    let defaults = defaults(ui);
    for (layout, title) in PLAIN {
        let own = Palette::of_layout(layout);
        let mut text = format!(
            "# Every colour of the {title} layout, with its default.\n\
             # Copy this file up one folder, rename it, and change what you like.\n\
             name = My {title}\nlayout = {layout}\n\n"
        );
        for (p, key, v) in &defaults {
            if *p == own {
                match v {
                    Val::Color(c) => text += &format!("{key} = {}\n", color_text(*c)),
                    Val::Str(s) => text += &format!("{key} = {s}\n"),
                }
            }
        }
        std::fs::write(examples.join(format!("all-colours-{layout}.skin")), text)?;
    }
    Ok(dir)
}

macro_rules! palette {
    (@wrap Color, $e:expr) => { Val::Color($e) };
    (@wrap Str, $e:expr) => { Val::Str($e) };
    (@unwrap Color, $v:expr) => { match $v { Val::Color(c) => Some(*c), _ => None } };
    (@unwrap Str, $v:expr) => { match $v { Val::Str(s) => Some(s.clone()), _ => None } };
    ($C:ty, $G:ident, [ $($key:literal => $get:ident, $set:ident: $kind:ident),* $(,)? ]) => {
        vec![$(Prop::<$C> {
            key: $key,
            kind: Kind::$kind,
            get: |c: &$C| palette!(@wrap $kind, c.global::<$G>().$get()),
            set: |c: &$C, v: &Val| {
                if let Some(x) = palette!(@unwrap $kind, v) {
                    c.global::<$G>().$set(x);
                }
            },
        }),*]
    };
}

// The palettes' properties. Keep in step with the globals in theme.slint,
// classic.slint and silver.slint (a test checks).

fn theme_props<C: ComponentHandle>() -> Vec<Prop<C>>
where
    for<'a> Theme<'a>: slint::Global<'a, C>,
{
    palette!(C, Theme, [
        "bg" => get_bg, set_bg: Color,
        "bg-2" => get_bg_2, set_bg_2: Color,
        "surface" => get_surface, set_surface: Color,
        "surface-hi" => get_surface_hi, set_surface_hi: Color,
        "border" => get_border, set_border: Color,
        "text" => get_text, set_text: Color,
        "text-dim" => get_text_dim, set_text_dim: Color,
        "text-faint" => get_text_faint, set_text_faint: Color,
        "accent" => get_accent, set_accent: Color,
        "accent-2" => get_accent_2, set_accent_2: Color,
        "danger" => get_danger, set_danger: Color,
        "panel" => get_panel, set_panel: Color,
        "button" => get_button, set_button: Color,
        "button-hover" => get_button_hover, set_button_hover: Color,
        "button-text" => get_button_text, set_button_text: Color,
        "popup" => get_popup, set_popup: Color,
        "on-accent" => get_on_accent, set_on_accent: Color,
        "title-bar" => get_title_bar, set_title_bar: Color,
        "controls" => get_controls, set_controls: Color,
        "playlist" => get_playlist, set_playlist: Color,
        "nav" => get_nav, set_nav: Color,
    ])
}

fn classic_props<C: ComponentHandle>() -> Vec<Prop<C>>
where
    for<'a> Classic<'a>: slint::Global<'a, C>,
{
    palette!(C, Classic, [
        "outside" => get_outside, set_outside: Color,
        "body-1" => get_body_1, set_body_1: Color,
        "body-2" => get_body_2, set_body_2: Color,
        "body-3" => get_body_3, set_body_3: Color,
        "body-edge" => get_body_edge, set_body_edge: Color,
        "lcd" => get_lcd, set_lcd: Color,
        "lcd-edge" => get_lcd_edge, set_lcd_edge: Color,
        "green" => get_green, set_green: Color,
        "green-dim" => get_green_dim, set_green_dim: Color,
        "label" => get_label, set_label: Color,
        "alert" => get_alert, set_alert: Color,
        "ridge-light" => get_ridge_light, set_ridge_light: Color,
        "ridge-dark" => get_ridge_dark, set_ridge_dark: Color,
        "button-1" => get_button_1, set_button_1: Color,
        "button-2" => get_button_2, set_button_2: Color,
        "button-3" => get_button_3, set_button_3: Color,
        "button-pressed-1" => get_button_pressed_1, set_button_pressed_1: Color,
        "button-pressed-2" => get_button_pressed_2, set_button_pressed_2: Color,
        "button-edge" => get_button_edge, set_button_edge: Color,
        "button-edge-pressed" => get_button_edge_pressed, set_button_edge_pressed: Color,
        "button-ink" => get_button_ink, set_button_ink: Color,
        "button-ink-hover" => get_button_ink_hover, set_button_ink_hover: Color,
        "toggle-1" => get_toggle_1, set_toggle_1: Color,
        "toggle-2" => get_toggle_2, set_toggle_2: Color,
        "toggle-pressed" => get_toggle_pressed, set_toggle_pressed: Color,
        "toggle-edge" => get_toggle_edge, set_toggle_edge: Color,
        "led-off" => get_led_off, set_led_off: Color,
        "playlist-current" => get_playlist_current, set_playlist_current: Color,
        "playlist-current-text" => get_playlist_current_text, set_playlist_current_text: Color,
        "playlist-hover" => get_playlist_hover, set_playlist_hover: Color,
        "meter-low" => get_meter_low, set_meter_low: Color,
        "meter-mid" => get_meter_mid, set_meter_mid: Color,
        "meter-high" => get_meter_high, set_meter_high: Color,
        "groove" => get_groove, set_groove: Color,
        "mono" => get_mono, set_mono: Str,
    ])
}

fn silver_props<C: ComponentHandle>() -> Vec<Prop<C>>
where
    for<'a> Silver<'a>: slint::Global<'a, C>,
{
    palette!(C, Silver, [
        "outside" => get_outside, set_outside: Color,
        "case-1" => get_case_1, set_case_1: Color,
        "case-2" => get_case_2, set_case_2: Color,
        "case-3" => get_case_3, set_case_3: Color,
        "case-4" => get_case_4, set_case_4: Color,
        "case-edge" => get_case_edge, set_case_edge: Color,
        "bezel-1" => get_bezel_1, set_bezel_1: Color,
        "bezel-2" => get_bezel_2, set_bezel_2: Color,
        "bezel-3" => get_bezel_3, set_bezel_3: Color,
        "bezel-edge" => get_bezel_edge, set_bezel_edge: Color,
        "chrome-1" => get_chrome_1, set_chrome_1: Color,
        "chrome-2" => get_chrome_2, set_chrome_2: Color,
        "chrome-3" => get_chrome_3, set_chrome_3: Color,
        "chrome-pressed-1" => get_chrome_pressed_1, set_chrome_pressed_1: Color,
        "chrome-pressed-2" => get_chrome_pressed_2, set_chrome_pressed_2: Color,
        "chrome-pressed-3" => get_chrome_pressed_3, set_chrome_pressed_3: Color,
        "chrome-edge" => get_chrome_edge, set_chrome_edge: Color,
        "glass-1" => get_glass_1, set_glass_1: Color,
        "glass-2" => get_glass_2, set_glass_2: Color,
        "glass-3" => get_glass_3, set_glass_3: Color,
        "glass-edge" => get_glass_edge, set_glass_edge: Color,
        "glass-mark" => get_glass_mark, set_glass_mark: Color,
        "text" => get_text, set_text: Color,
        "text-dim" => get_text_dim, set_text_dim: Color,
        "text-off" => get_text_off, set_text_off: Color,
        "text-alert" => get_text_alert, set_text_alert: Color,
        "shadow" => get_shadow, set_shadow: Color,
        "ink" => get_ink, set_ink: Color,
        "ink-hover" => get_ink_hover, set_ink_hover: Color,
        "engraved" => get_engraved, set_engraved: Color,
        "grille" => get_grille, set_grille: Color,
        "led" => get_led, set_led: Color,
        "led-glow" => get_led_glow, set_led_glow: Color,
        "led-edge" => get_led_edge, set_led_edge: Color,
        "led-off" => get_led_off, set_led_off: Color,
        "led-off-edge" => get_led_off_edge, set_led_off_edge: Color,
        "wheel-1" => get_wheel_1, set_wheel_1: Color,
        "wheel-2" => get_wheel_2, set_wheel_2: Color,
        "wheel-3" => get_wheel_3, set_wheel_3: Color,
        "wheel-edge" => get_wheel_edge, set_wheel_edge: Color,
        "wheel-cross" => get_wheel_cross, set_wheel_cross: Color,
        "wheel-ink" => get_wheel_ink, set_wheel_ink: Color,
        "wheel-ink-hover" => get_wheel_ink_hover, set_wheel_ink_hover: Color,
        "ring" => get_ring, set_ring: Color,
        "well-1" => get_well_1, set_well_1: Color,
        "well-2" => get_well_2, set_well_2: Color,
        "well-3" => get_well_3, set_well_3: Color,
        "well-edge" => get_well_edge, set_well_edge: Color,
        "knob-1" => get_knob_1, set_knob_1: Color,
        "knob-2" => get_knob_2, set_knob_2: Color,
        "knob-3" => get_knob_3, set_knob_3: Color,
        "groove" => get_groove, set_groove: Color,
        "groove-edge" => get_groove_edge, set_groove_edge: Color,
        "accent" => get_accent, set_accent: Color,
        "accent-2" => get_accent_2, set_accent_2: Color,
        "ticker" => get_ticker, set_ticker: Color,
        "playlist-1" => get_playlist_1, set_playlist_1: Color,
        "playlist-2" => get_playlist_2, set_playlist_2: Color,
        "logo" => get_logo, set_logo: Color,
        "mark" => get_mark, set_mark: Color,
        "mono" => get_mono, set_mono: Str,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_labels_read_plainly() {
        assert_eq!(
            colour_label(Palette::Theme, "panel"),
            "Dialogs (settings, library…)"
        );
        assert_eq!(colour_label(Palette::Silver, "case-1"), "Case 1");
        assert_eq!(
            colour_label(Palette::Classic, "button-pressed-2"),
            "Button pressed 2"
        );
    }

    /// `in-out property <color|string> name` in global `name` of `source`.
    fn slint_props(source: &str, global: &str) -> Vec<String> {
        let start = source.find(&format!("global {global} {{")).expect("global");
        let body = &source[start..];
        let body = &body[..body.find("\n}").unwrap()];
        body.lines()
            .filter_map(|l| {
                let l = l.trim();
                let rest = l
                    .strip_prefix("in-out property <color> ")
                    .or_else(|| l.strip_prefix("in-out property <string> "))?;
                Some(rest.split(':').next()?.trim().to_owned())
            })
            .collect()
    }

    #[test]
    fn tables_match_the_slint_globals() {
        for (p, src, g) in [
            (Palette::Theme, include_str!("../ui/theme.slint"), "Theme"),
            (
                Palette::Classic,
                include_str!("../ui/classic.slint"),
                "Classic",
            ),
            (
                Palette::Silver,
                include_str!("../ui/silver.slint"),
                "Silver",
            ),
        ] {
            let table: Vec<String> = p.props().iter().map(|p| p.key.to_owned()).collect();
            assert_eq!(table, slint_props(src, g), "update {g}'s table in skins.rs");
        }
    }

    #[test]
    fn colours() {
        let c = parse_color("#12abef").unwrap();
        assert_eq!(
            (c.red(), c.green(), c.blue(), c.alpha()),
            (0x12, 0xab, 0xef, 255)
        );
        let c = parse_color("#fff8").unwrap();
        assert_eq!((c.red(), c.alpha()), (255, 0x88));
        assert_eq!(parse_color("transparent").unwrap().alpha(), 0);
        assert!(parse_color("#12345").is_none());
        assert!(parse_color("red").is_none());
        assert_eq!(color_text(parse_color("#3a8fd6").unwrap()), "#3a8fd6");
        assert_eq!(color_text(parse_color("#3a8fd680").unwrap()), "#3a8fd680");
    }

    #[test]
    fn built_in_skins_are_clean() {
        for (id, text) in BUILT_IN {
            let (skin, warnings) = parse(id, text).unwrap();
            assert!(warnings.is_empty(), "{id}: {warnings:?}");
            assert!(!skin.colors.is_empty(), "{id}");
            assert!(
                skin.is_theme() || skin.name.contains('–'),
                "{id}: name says which view it dresses"
            );
        }
    }

    #[test]
    fn mistakes_are_skipped() {
        let text = "name = T\nlayout = silver\ncase-1 = #fff\nnope = #fff\nled = blue\n\
                    theme.accent = #f00\nbogus.x = #fff\njunk line\nmono = Fira Mono";
        let (skin, w) = parse("t", text).unwrap();
        assert_eq!(skin.colors.len(), 3);
        assert_eq!(skin.colors[1].0, Palette::Theme);
        assert_eq!(skin.colors[2].2, Val::Str("Fira Mono".into()));
        assert_eq!(w.len(), 4, "{w:?}");
        assert!(parse("t", "name = T\nlayout = round").is_err());
        assert!(parse("t", "case-1 = #fff").is_err());
    }

    #[test]
    fn themes_fit_the_media_player_and_compact() {
        let (theme, _) = parse("t", "name = T\nlayout = any\naccent = #f00").unwrap();
        assert!(theme.is_theme());
        assert!(theme.fits("player") && theme.fits("compact"));
        assert!(!theme.fits("classic") && !theme.fits("silver"));
        assert_eq!(theme.view_index(), -1);
        assert_eq!(theme.colors[0].0, Palette::Theme);
    }

    #[test]
    fn new_colours_follow_the_old_ones() {
        let (skin, _) = parse(
            "t",
            "name = T\nlayout = any\nbg-2 = #102030\nsurface = #405060",
        )
        .unwrap();
        let defaults: Colours = vec![
            (
                Palette::Theme,
                "title-bar",
                Val::Color(parse_color("#000").unwrap()),
            ),
            (
                Palette::Theme,
                "playlist",
                Val::Color(parse_color("#111").unwrap()),
            ),
        ];
        let c = |key| match resolved(&skin, &defaults, Palette::Theme, key) {
            Some(Val::Color(c)) => color_text(c),
            v => panic!("{v:?}"),
        };
        assert_eq!(c("title-bar"), "#102030");
        assert_eq!(c("controls"), "#405060");
        assert_eq!(c("playlist"), "#111111");
    }

    #[test]
    fn shades_move_along() {
        let col = |s| parse_color(s).unwrap();
        // The leader itself takes the new colour, keeping its opacity.
        let to = col("#3dffa8");
        let moved = shade_along(col("#39d0ff"), to, col("#39d0ffd0"));
        assert_eq!(color_text(moved), "#3dffa8d0");
        // Dim text stays dimmer than light text...
        let dim = shade_along(col("#eef1fa"), col("#ffffff"), col("#9aa3bd"));
        assert!(
            dim.to_hsva().value < 0.85 && dim.to_hsva().saturation < 0.05,
            "{dim:?}"
        );
        // ...and lighter than dark text on a light background.
        let dim = shade_along(col("#eef1fa"), col("#202020"), col("#9aa3bd"));
        assert!(dim.to_hsva().value > 0.3, "{dim:?}");
    }

    #[test]
    fn simple_groups_name_real_colours() {
        for layout in crate::settings::VIEW_MODES {
            for (label, p, keys) in simple_groups(layout) {
                for key in keys {
                    assert!(
                        p.props().iter().any(|prop| prop.key == *key),
                        "{layout}: {label}: {key}"
                    );
                }
            }
        }
    }

    #[test]
    fn plain_views_come_first_in_view_order() {
        let (all, _) = load_all();
        for (i, (id, _)) in PLAIN.iter().enumerate() {
            assert_eq!(all[i].id, *id);
            assert_eq!(all[i].view_index(), i as i32);
        }
        assert!(all.len() >= PLAIN.len() + BUILT_IN.len());
    }
}
