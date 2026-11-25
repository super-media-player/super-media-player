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

/// Skins that ship with the player: (id, file).
const BUILT_IN: &[(&str, &str)] = &[
    ("aurora", include_str!("../skins/aurora.skin")),
    ("ember", include_str!("../skins/ember.skin")),
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

    /// Index of its layout in `settings::VIEW_MODES`.
    pub fn view_index(&self) -> i32 {
        crate::settings::VIEW_MODES
            .iter()
            .position(|v| *v == self.layout)
            .unwrap_or(0) as i32
    }
}

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
    if !crate::settings::VIEW_MODES.contains(&skin.layout.as_str()) {
        return Err(format!(
            "layout must be one of {}",
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
/// skin's. The classic deck's own playlist window (`pw`) follows along.
pub fn apply(ui: &AppWindow, pw: Option<&PlaylistWindow>, skin: &Skin) {
    let defaults = defaults(ui);
    for p in Palette::ALL {
        let props = p.props();
        let value = |key: &str| {
            skin.colors
                .iter()
                .rev()
                .find(|(sp, k, _)| *sp == p && k == key)
                .map(|(_, _, v)| v.clone())
                .or_else(|| {
                    defaults
                        .iter()
                        .find(|(dp, k, _)| *dp == p && *k == key)
                        .map(|(_, _, v)| v.clone())
                })
        };
        for prop in &props {
            if let Some(v) = value(prop.key) {
                (prop.set)(ui, &v);
            }
        }
    }
    if let Some(pw) = pw {
        for prop in classic_props::<PlaylistWindow>() {
            if let Some(src) = classic_props::<AppWindow>()
                .iter()
                .find(|p| p.key == prop.key)
            {
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
                skin.name.contains('–'),
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
    fn plain_views_come_first_in_view_order() {
        let (all, _) = load_all();
        for (i, (id, _)) in PLAIN.iter().enumerate() {
            assert_eq!(all[i].id, *id);
            assert_eq!(all[i].view_index(), i as i32);
        }
        assert!(all.len() >= PLAIN.len() + BUILT_IN.len());
    }
}
