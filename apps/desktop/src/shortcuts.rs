//! Your own keyboard shortcuts and mouse actions.
//!
//! Every key press first asks [`lookup`]: a combination you bound runs its
//! action (any command palette action, or nothing, which turns a built-in
//! key off); then your scripts (scripts.rs) are asked; anything else falls
//! through to the built-in keys in app.slint.
//! Bindings live in `shortcuts.conf` as `Ctrl+Shift+X = action-id` lines.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::platform::Key;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::settings::{self, Settings};
use crate::{AppWindow, ShortcutRow, palette, scripts};

/// The action that does nothing: binding a key to it turns the key off.
pub const NOTHING: &str = "nothing";

thread_local! {
    static BINDINGS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    /// The ids behind the list of actions on the settings page.
    static CHOICES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Names for the keys that don't type a character.
const NAMED: &[(Key, &str)] = &[
    (Key::LeftArrow, "Left"),
    (Key::RightArrow, "Right"),
    (Key::UpArrow, "Up"),
    (Key::DownArrow, "Down"),
    (Key::PageUp, "PgUp"),
    (Key::PageDown, "PgDn"),
    (Key::Home, "Home"),
    (Key::End, "End"),
    (Key::Insert, "Insert"),
    (Key::Delete, "Delete"),
    (Key::Backspace, "Backspace"),
    (Key::Return, "Enter"),
    (Key::Tab, "Tab"),
    (Key::Escape, "Esc"),
    (Key::F1, "F1"),
    (Key::F2, "F2"),
    (Key::F3, "F3"),
    (Key::F4, "F4"),
    (Key::F5, "F5"),
    (Key::F6, "F6"),
    (Key::F7, "F7"),
    (Key::F8, "F8"),
    (Key::F9, "F9"),
    (Key::F10, "F10"),
    (Key::F11, "F11"),
    (Key::F12, "F12"),
];

const MODIFIERS: &[Key] = &[
    Key::Shift,
    Key::ShiftR,
    Key::Control,
    Key::ControlR,
    Key::Alt,
    Key::AltGr,
    Key::Meta,
    Key::MetaR,
];

/// "Ctrl+Shift+Left", "Alt+X", "Space", "+"; None for a modifier on its own
/// or a key we can't name.
pub fn combo(text: &str, ctrl: bool, alt: bool, shift: bool) -> Option<String> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if MODIFIERS.iter().any(|m| char::from(*m) == c) {
        return None;
    }
    let (name, shift_counts) = if let Some((_, n)) = NAMED.iter().find(|(k, _)| char::from(*k) == c)
    {
        ((*n).to_owned(), true)
    } else if c == ' ' {
        ("Space".to_owned(), true)
    } else if c.is_alphabetic() {
        // Shift makes "a" into "A": the name is the letter, Shift says which.
        (c.to_uppercase().collect(), true)
    } else if c.is_control() || ('\u{f700}'..='\u{f8ff}').contains(&c) {
        return None;
    } else {
        // "+" or "?" already says Shift was held.
        (c.to_string(), false)
    };
    let shift = shift && shift_counts || (c.is_alphabetic() && c.is_uppercase());
    let mut out = String::new();
    if ctrl {
        out += "Ctrl+";
    }
    if alt {
        out += "Alt+";
    }
    if shift {
        out += "Shift+";
    }
    out += &name;
    Some(out)
}

fn file() -> Option<PathBuf> {
    settings::config_dir().map(|d| d.join("shortcuts.conf"))
}

pub fn parse(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        // "+ = mute" binds "+": split at the last " = ".
        let Some((k, a)) = line.rsplit_once(" = ").or_else(|| line.split_once('=')) else {
            continue;
        };
        let (k, a) = (k.trim(), a.trim());
        if k.is_empty() || !(a == NOTHING || palette::has_action(a) || scripts::is_action(a)) {
            continue;
        }
        out.retain(|(kk, _)| kk != k);
        out.push((k.to_owned(), a.to_owned()));
    }
    out
}

fn save(list: &[(String, String)]) {
    let Some(f) = file() else { return };
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let mut text =
        String::from("# Super Media Player shortcuts: key = action (Settings → Shortcuts)\n");
    for (k, a) in list {
        text += &format!("{k} = {a}\n");
    }
    let _ = std::fs::write(f, text);
}

fn title_of(action: &str) -> String {
    if action == NOTHING {
        "Nothing (key turned off)".into()
    } else if scripts::is_action(action) {
        scripts::title(action)
    } else {
        palette::action_title(action).unwrap_or(action).to_owned()
    }
}

/// The actions a key can do, in the order the settings list shows them.
fn choices() -> Vec<(String, String)> {
    let mut v = vec![(NOTHING.to_owned(), "Nothing (turn the key off)".to_owned())];
    v.extend(
        palette::actions()
            .into_iter()
            .map(|(id, t)| (id.to_owned(), t.to_owned())),
    );
    v.extend(scripts::actions().into_iter().map(|(id, t, _)| (id, t)));
    v
}

/// Fill the list of actions; scripts add theirs as they start.
pub fn show_actions(ui: &AppWindow) {
    let list = choices();
    let ids: Vec<String> = list.iter().map(|(id, _)| id.clone()).collect();
    if CHOICES.with_borrow(|c| *c == ids) {
        return;
    }
    CHOICES.with_borrow_mut(|c| *c = ids);
    ui.set_shortcut_actions(ModelRc::new(VecModel::from(
        list.into_iter()
            .map(|(_, t)| SharedString::from(t))
            .collect::<Vec<_>>(),
    )));
}

fn show(ui: &AppWindow) {
    let rows: Vec<ShortcutRow> = BINDINGS.with_borrow(|b| {
        b.iter()
            .map(|(k, a)| ShortcutRow {
                keys: k.into(),
                action: title_of(a).into(),
            })
            .collect()
    });
    ui.set_shortcut_rows(ModelRc::new(VecModel::from(rows)));
}

pub fn install(ui: &AppWindow, settings: Rc<RefCell<Settings>>) {
    let list = file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|t| parse(&t))
        .unwrap_or_default();
    BINDINGS.with_borrow_mut(|b| *b = list);
    show(ui);
    show_actions(ui);

    let weak = ui.as_weak();
    ui.on_key_binding(move |text, ctrl, alt, shift| {
        let Some(ui) = weak.upgrade() else {
            return false;
        };
        let Some(k) = combo(&text, ctrl, alt, shift) else {
            return false;
        };
        let script = scripts::wanted(&k);
        if script == scripts::Wanted::Forced {
            scripts::press(&k);
            return true;
        }
        let Some(action) =
            BINDINGS.with_borrow(|b| b.iter().find(|(kk, _)| *kk == k).map(|(_, a)| a.clone()))
        else {
            if script == scripts::Wanted::Ordinary {
                scripts::press(&k);
                return true;
            }
            return false;
        };
        if action != NOTHING {
            palette::run(&ui, &action);
        }
        true
    });
    ui.on_shortcut_capture(|text, ctrl, alt, shift| {
        combo(&text, ctrl, alt, shift).unwrap_or_default().into()
    });
    let weak = ui.as_weak();
    ui.on_shortcut_add(move |keys, index| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(id) = CHOICES.with_borrow(|c| c.get(index.max(0) as usize).cloned()) else {
            return;
        };
        if keys.is_empty() {
            ui.set_toast("Click the key box and press the keys first".into());
            return;
        }
        BINDINGS.with_borrow_mut(|b| {
            b.retain(|(k, _)| k.as_str() != keys.as_str());
            b.push((keys.to_string(), id));
            save(b);
        });
        show(&ui);
    });
    let weak = ui.as_weak();
    ui.on_shortcut_remove(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        BINDINGS.with_borrow_mut(|b| {
            if (i as usize) < b.len() {
                b.remove(i as usize);
                save(b);
            }
        });
        show(&ui);
    });
    let weak = ui.as_weak();
    ui.on_mouse_action(move |what| {
        let Some(ui) = weak.upgrade() else { return };
        let s = settings.borrow();
        let action = match what.as_str() {
            "wheel-up" | "wheel-down" => {
                let up = what.as_str() == "wheel-up";
                match s.mouse_wheel.as_str() {
                    "seek" => ui.invoke_seek_relative(if up { 5.0 } else { -5.0 }),
                    "volume" => {
                        let v = ui.get_volume() + if up { 5.0 } else { -5.0 };
                        ui.invoke_set_volume(v.clamp(0.0, 150.0));
                    }
                    _ => {}
                }
                return;
            }
            "double" => s.mouse_double.clone(),
            "middle" => s.mouse_middle.clone(),
            _ => return,
        };
        drop(s);
        match action.as_str() {
            "fullscreen" => ui.invoke_toggle_fullscreen(),
            "pause" => ui.invoke_toggle_pause(),
            "mute" => ui.invoke_toggle_mute(),
            "next" => ui.invoke_next(),
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(combo("a", false, false, false).as_deref(), Some("A"));
        assert_eq!(combo("A", false, false, true).as_deref(), Some("Shift+A"));
        assert_eq!(combo("a", true, false, false).as_deref(), Some("Ctrl+A"));
        assert_eq!(combo(" ", false, false, false).as_deref(), Some("Space"));
        assert_eq!(combo("+", false, false, true).as_deref(), Some("+"));
        let left: String = char::from(Key::LeftArrow).into();
        assert_eq!(
            combo(&left, true, false, true).as_deref(),
            Some("Ctrl+Shift+Left")
        );
        let f5: String = char::from(Key::F5).into();
        assert_eq!(combo(&f5, false, true, false).as_deref(), Some("Alt+F5"));
        let shift: String = char::from(Key::Shift).into();
        assert_eq!(combo(&shift, false, false, true), None);
    }

    #[test]
    fn config_lines() {
        let l = parse(
            "# c\nCtrl+A = mute\n+ = fullscreen\nX = no-such-action\nG = script:\nH = script:a/b\nCtrl+A = nothing\n",
        );
        let s = |k: &str, a: &str| (k.to_owned(), a.to_owned());
        assert_eq!(
            l,
            vec![
                s("+", "fullscreen"),
                s("H", "script:a/b"),
                s("Ctrl+A", "nothing")
            ]
        );
    }
}
