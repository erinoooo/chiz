//! Profiles, key strings, and profile selection (spec 8, 11).
//! Mirrors protocol/chiz_proto.py key tables + validate_profile.

use std::collections::{HashMap, HashSet};

pub const PROFILES_VERSION: u32 = 1;

// ------------------------------------------------------------- key table

/// (Windows VK, Linux evdev) codes for a key name (lowercase).
pub fn key_codes(name: &str) -> Option<(u16, u16)> {
    Some(match name {
        "ctrl" => (0xA2, 29),
        "shift" => (0xA0, 42),
        "alt" => (0xA4, 56),
        "meta" => (0x5B, 125),
        "space" => (0x20, 57),
        "enter" => (0x0D, 28),
        "tab" => (0x09, 15),
        "esc" => (0x1B, 1),
        "backspace" => (0x08, 14),
        "delete" => (0x2E, 111),
        "insert" => (0x2D, 110),
        "home" => (0x24, 102),
        "end" => (0x23, 107),
        "pageup" => (0x21, 104),
        "pagedown" => (0x22, 109),
        "left" => (0x25, 105),
        "up" => (0x26, 103),
        "right" => (0x27, 106),
        "down" => (0x28, 108),
        "minus" => (0xBD, 12),
        "equal" => (0xBB, 13),
        "bracketleft" => (0xDB, 26),
        "bracketright" => (0xDD, 27),
        "comma" => (0xBC, 51),
        "period" => (0xBE, 52),
        "slash" => (0xBF, 53),
        "backslash" => (0xDC, 43),
        "semicolon" => (0xBA, 39),
        "quote" => (0xDE, 40),
        "grave" => (0xC0, 41),
        _ => {
            if let Some(n) = name.strip_prefix('f') {
                if let Ok(i) = n.parse::<u32>() {
                    if (1..=10).contains(&i) {
                        return Some((0x6F + i as u16, 58 + i as u16));
                    }
                    if i == 11 {
                        return Some((0x7A, 87));
                    }
                    if i == 12 {
                        return Some((0x7B, 88));
                    }
                    if (13..=24).contains(&i) {
                        return Some((0x7C + i as u16 - 13, 183 + i as u16 - 13));
                    }
                }
                return None;
            }
            if name.len() == 1 {
                let c = name.as_bytes()[0];
                if c.is_ascii_digit() {
                    let d = (c - b'0') as u16;
                    let ev = if d == 0 { 11 } else { 2 + d - 1 };
                    return Some((0x30 + d, ev));
                }
                if c.is_ascii_lowercase() {
                    let ev = match c {
                        b'a' => 30, b'b' => 48, b'c' => 46, b'd' => 32,
                        b'e' => 18, b'f' => 33, b'g' => 34, b'h' => 35,
                        b'i' => 23, b'j' => 36, b'k' => 37, b'l' => 38,
                        b'm' => 50, b'n' => 49, b'o' => 24, b'p' => 25,
                        b'q' => 16, b'r' => 19, b's' => 31, b't' => 20,
                        b'u' => 22, b'v' => 47, b'w' => 17, b'x' => 45,
                        b'y' => 21, b'z' => 44, _ => return None,
                    };
                    return Some((0x41 + (c - b'a') as u16, ev));
                }
            }
            return None;
        }
    })
}

pub fn is_modifier(name: &str) -> bool {
    matches!(name, "ctrl" | "shift" | "alt" | "meta")
}

/// Keys needing KEYEVENTF_EXTENDEDKEY on Windows (spec 11).
pub fn is_extended(name: &str) -> bool {
    matches!(
        name,
        "insert" | "delete" | "home" | "end" | "pageup" | "pagedown" | "left" | "up" | "right" | "down" | "meta"
    )
}

/// Parse a `keys` string: tokens joined by `+`, all but the last MUST be
/// modifiers, each combo has one non-modifier key (spec 11). A modifier-only
/// string (e.g. `shift`) is valid for hold buttons.
pub fn parse_keys(s: &str) -> Result<Vec<String>, String> {
    let toks: Vec<String> = s.split('+').map(|t| t.to_lowercase()).collect();
    if toks.is_empty() || toks.iter().any(|t| t.is_empty()) {
        return Err("empty key token".into());
    }
    for t in &toks {
        if key_codes(t).is_none() {
            return Err(format!("unknown key {t:?}"));
        }
    }
    if toks.len() > 1 && toks[..toks.len() - 1].iter().any(|t| !is_modifier(t)) {
        return Err("only trailing key may be non-modifier".into());
    }
    Ok(toks)
}

// ------------------------------------------------------------- profile types

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Grid {
    pub cols: i64,
    pub rows: i64,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct KeyAction {
    pub keys: String,
    #[serde(default)]
    pub keys_off: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum MacroStep {
    Keys { keys: String },
    Wait { wait_ms: u32 },
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct MacroAction {
    pub steps: Vec<MacroStep>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct ProfileAction {
    pub target: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct MouseAction {
    pub button: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type")]
pub enum Action {
    #[serde(rename = "key")]
    Key(KeyAction),
    #[serde(rename = "macro")]
    Macro(MacroAction),
    #[serde(rename = "profile")]
    Profile(ProfileAction),
    #[serde(rename = "eraser_toggle")]
    EraserToggle,
    #[serde(rename = "mouse")]
    Mouse(MouseAction),
    #[serde(rename = "none")]
    None,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Button {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub speak: String,
    pub kind: String,
    pub col: i64,
    pub row: i64,
    #[serde(default = "one")]
    pub colspan: i64,
    #[serde(default = "one")]
    pub rowspan: i64,
    pub action: Action,
}

fn one() -> i64 {
    1
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Profile {
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub apps: Vec<String>,
    #[serde(default)]
    pub default: bool,
    pub grid: Grid,
    pub buttons: Vec<Button>,
}

impl Button {
    pub fn speak_text(&self) -> &str {
        if self.speak.is_empty() {
            &self.label
        } else {
            &self.speak
        }
    }
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Validate one profile file (spec 11 table). Cross-profile rules
/// (unique ids, at most one default) are checked by `validate_set`.
pub fn validate_profile(p: &Profile) -> Vec<String> {
    let mut errs = vec![];
    if p.version > PROFILES_VERSION {
        errs.push("unsupported version".into());
        return errs;
    }
    if !valid_id(&p.id) {
        errs.push("bad profile id".into());
    }
    if p.name.is_empty() || p.name.chars().count() > 24 {
        errs.push("bad profile name".into());
    }
    if !(1..=3).contains(&p.grid.cols) || !(1..=12).contains(&p.grid.rows) {
        errs.push("bad grid".into());
    }
    let mut seen = HashSet::new();
    let mut cells = HashSet::new();
    for b in &p.buttons {
        if !valid_id(&b.id) {
            errs.push(format!("bad button id {:?}", b.id));
        }
        if !seen.insert(b.id.clone()) {
            errs.push(format!("duplicate button id {:?}", b.id));
        }
        if b.label.chars().count() > 16 {
            errs.push(format!("label too long {:?}", b.id));
        }
        if b.speak_text().chars().count() > 24 {
            errs.push(format!("speak too long {:?}", b.id));
        }
        if !matches!(b.kind.as_str(), "tap" | "hold" | "toggle") {
            errs.push(format!("bad kind {:?}", b.id));
        }
        let inside = b.col >= 0
            && b.row >= 0
            && b.colspan >= 1
            && b.rowspan >= 1
            && b.col + b.colspan <= p.grid.cols
            && b.row + b.rowspan <= p.grid.rows;
        if !inside {
            errs.push(format!("button out of grid {:?}", b.id));
            continue;
        }
        for c in b.col..b.col + b.colspan {
            for r in b.row..b.row + b.rowspan {
                if !cells.insert((c, r)) {
                    errs.push(format!("overlapping button {:?}", b.id));
                }
            }
        }
        match &b.action {
            Action::Key(k) => {
                if let Err(e) = parse_keys(&k.keys) {
                    errs.push(format!("bad keys {:?}: {e}", b.id));
                }
                if let Some(off) = &k.keys_off {
                    if let Err(e) = parse_keys(off) {
                        errs.push(format!("bad keys_off {:?}: {e}", b.id));
                    }
                }
            }
            Action::Macro(m) => {
                if b.kind == "hold" {
                    errs.push(format!("macro not allowed on hold {:?}", b.id));
                }
                if m.steps.len() > 20 {
                    errs.push(format!("macro too long {:?}", b.id));
                }
                for s in &m.steps {
                    match s {
                        MacroStep::Wait { wait_ms } => {
                            if *wait_ms > 2000 {
                                errs.push(format!("bad wait_ms {:?}", b.id));
                            }
                        }
                        MacroStep::Keys { keys } => {
                            if let Err(e) = parse_keys(keys) {
                                errs.push(format!("bad macro keys {:?}: {e}", b.id));
                            }
                        }
                    }
                }
            }
            Action::EraserToggle => {
                if b.kind != "toggle" {
                    errs.push(format!("eraser_toggle requires toggle {:?}", b.id));
                }
            }
            Action::Mouse(m) => {
                if !matches!(m.button.as_str(), "left" | "right" | "middle") {
                    errs.push(format!("bad mouse button {:?}", b.id));
                }
            }
            Action::Profile(_) | Action::None => {}
        }
    }
    errs
}

/// Cross-profile rules: unique ids, at most one default.
pub fn validate_set(profiles: &[Profile]) -> Vec<String> {
    let mut errs = vec![];
    let mut seen = HashSet::new();
    for p in profiles {
        if !seen.insert(p.id.clone()) {
            errs.push(format!("duplicate profile id {:?}", p.id));
        }
    }
    if profiles.iter().filter(|p| p.default).count() > 1 {
        errs.push("more than one default profile".into());
    }
    errs
}

/// Profile selection (spec 8): manual lock wins; else first app match
/// (case-insensitive); else the default profile.
pub fn select_profile<'a>(
    profiles: &'a [Profile],
    foreground_app: Option<&str>,
    locked_id: Option<&str>,
) -> Option<&'a Profile> {
    if let Some(locked) = locked_id {
        return profiles.iter().find(|p| p.id == locked);
    }
    if let Some(fa) = foreground_app {
        if let Some(hit) = profiles
            .iter()
            .find(|p| p.apps.iter().any(|a| a.eq_ignore_ascii_case(fa)))
        {
            return Some(hit);
        }
    }
    profiles.iter().find(|p| p.default)
}

/// Key press order (spec 8): modifiers down in order, key down/up,
/// modifiers up in reverse (~2 ms apart). Hold keeps keys down until up.
pub fn plan_key_press(keys: &[String], hold: bool, release: bool) -> Vec<(String, bool)> {
    let mods: Vec<&String> = keys[..keys.len().saturating_sub(1)]
        .iter()
        .filter(|k| is_modifier(k))
        .collect();
    // Note: parse_keys guarantees only the trailing token may be a
    // non-modifier, so everything before it is a modifier.
    let main = keys.last().cloned().unwrap_or_default();
    if release {
        let mut seq: Vec<(String, bool)> = mods.iter().rev().map(|m| ((*m).clone(), false)).collect();
        if !is_modifier(&main) || !mods.iter().any(|m| *m == &main) {
            if !main.is_empty() && !mods.contains(&&main) {
                seq.push((main, false));
            }
        }
        return seq;
    }
    let mut seq: Vec<(String, bool)> = mods.iter().map(|m| ((*m).clone(), true)).collect();
    let main_is_mod = is_modifier(&main);
    if main_is_mod && keys.len() == 1 {
        seq.push((main, true)); // modifier-only hold, e.g. Space-pan style
    } else if !main_is_mod {
        seq.push((main.clone(), true));
        if !hold {
            seq.push((main, false));
        }
    } else {
        // e.g. ["ctrl","shift"] hold: modifiers already down; nothing more.
        if !hold {
            seq.push((main, false));
        }
    }
    if !hold {
        seq.extend(mods.iter().rev().map(|m| ((*m).clone(), false)));
    }
    seq
}

pub fn button_map(p: &Profile) -> HashMap<&str, &Button> {
    p.buttons.iter().map(|b| (b.id.as_str(), b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_parser_vectors() {
        for good in ["ctrl+z", "ctrl+shift+z", "space", "shift", "b", "ctrl+equal", "f12", "meta+tab"] {
            assert!(parse_keys(good).is_ok(), "{good}");
        }
        for bad in ["ctrl+foo", "z+ctrl", "ctrl++z", "", "plus"] {
            assert!(parse_keys(bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse_keys("ctrl+z").unwrap(), vec!["ctrl", "z"]);
    }

    #[test]
    fn key_codes_spot() {
        assert_eq!(key_codes("ctrl"), Some((0xA2, 29)));
        assert_eq!(key_codes("z"), Some((0x5A, 44)));
        assert_eq!(key_codes("0"), Some((0x30, 11)));
        assert_eq!(key_codes("1"), Some((0x31, 2)));
        assert_eq!(key_codes("f1"), Some((0x70, 59)));
        assert_eq!(key_codes("f12"), Some((0x7B, 88)));
        assert_eq!(key_codes("bogus"), None);
        assert!(is_extended("delete") && !is_extended("space"));
    }

    fn good_profile() -> Profile {
        serde_json::from_value(serde_json::json!({
            "version": 1, "id": "krita", "name": "Krita",
            "apps": ["krita.exe"], "default": false,
            "grid": {"cols": 1, "rows": 8},
            "buttons": [
                {"id": "undo", "label": "Undo", "speak": "Undo", "kind": "tap",
                 "col": 0, "row": 0, "colspan": 1, "rowspan": 1,
                 "action": {"type": "key", "keys": "ctrl+z"}},
                {"id": "pan", "label": "Pan", "speak": "Pan", "kind": "hold",
                 "col": 0, "row": 4, "colspan": 1, "rowspan": 1,
                 "action": {"type": "key", "keys": "space"}}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn profile_validation() {
        assert!(validate_profile(&good_profile()).is_empty());
        let mut dup = good_profile();
        dup.buttons[1].id = "undo".into();
        dup.buttons[1].col = 0;
        dup.buttons[1].row = 0;
        let errs = validate_profile(&dup);
        assert!(errs.iter().any(|e| e.contains("duplicate")));
        assert!(errs.iter().any(|e| e.contains("overlapping")));
        let mut badkeys = good_profile();
        if let Action::Key(k) = &mut badkeys.buttons[0].action {
            k.keys = "ctrl+bogus".into();
        }
        assert!(validate_profile(&badkeys).iter().any(|e| e.contains("bad keys")));
        let mut holdmacro = good_profile();
        holdmacro.buttons[1].action = Action::Macro(MacroAction {
            steps: vec![MacroStep::Keys { keys: "a".into() }],
        });
        assert!(validate_profile(&holdmacro).iter().any(|e| e.contains("macro not allowed")));
    }

    #[test]
    fn default_profile_file_validates() {
        for file in ["../profiles/default.json", "../profiles/krita.json"] {
            let text = std::fs::read_to_string(file).expect("profile file");
            let p: Profile = serde_json::from_str(&text).expect("profile parses");
            assert!(validate_profile(&p).is_empty(), "{file}: {:?}", validate_profile(&p));
        }
    }

    #[test]
    fn selection_and_press_plans() {
        let gen: Profile = serde_json::from_value(serde_json::json!({
            "version": 1, "id": "gen", "name": "General", "apps": [], "default": true,
            "grid": {"cols": 1, "rows": 1},
            "buttons": [{"id": "a", "label": "A", "kind": "tap", "col": 0, "row": 0,
                         "colspan": 1, "rowspan": 1, "action": {"type": "none"}}]
        }))
        .unwrap();
        let krita = good_profile();
        let set = vec![gen, krita];
        assert_eq!(select_profile(&set, Some("KRITA.EXE"), None).unwrap().id, "krita");
        assert_eq!(select_profile(&set, Some("nope"), None).unwrap().id, "gen");
        assert_eq!(select_profile(&set, Some("krita.exe"), Some("gen")).unwrap().id, "gen");
        let tap = plan_key_press(&["ctrl".into(), "z".into()], false, false);
        assert_eq!(
            tap,
            vec![
                ("ctrl".to_string(), true),
                ("z".to_string(), true),
                ("z".to_string(), false),
                ("ctrl".to_string(), false)
            ]
        );
        let hold = plan_key_press(&["space".into()], true, false);
        assert_eq!(hold, vec![("space".to_string(), true)]);
    }
}
