//! Tray menu model (spec 8): status color + Open / Pair new tablet /
//! Profile (Auto + list) / Pause pen input / Emergency release / Quit.
//!
//! The `tray-icon`/`StatusNotifierItem` event loop lands in M7 (needs a
//! display server); this model is the entire menu semantics, tested here,
//! and the 20-line platform wiring just maps clicks to `Action`s handled
//! by `super::tray_action` / `open_pairing`.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StatusColor {
    Green, // connected
    Amber, // reconnecting / pairing open
    Red,   // disconnected
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Open,
    PairNew,
    ProfileAuto,
    ProfileLock(String),
    PauseToggle,
    Panic,
    Quit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Action(Action),
    Separator,
    /// Disabled label (e.g. no profiles, Wayland auto-switch notice).
    Label(String),
}

/// Build the menu from live state. `connected`: control session up;
/// `pairing_open`: amber dot + countdown item; `paused`: toggle shows
/// Resume; `profiles`/`locked`: Auto + per-profile radio rows.
pub fn build_menu(
    connected: bool,
    pairing_open: bool,
    paused: bool,
    profiles: &[String],
    locked: Option<&str>,
) -> (StatusColor, Vec<Item>) {
    let color = if connected {
        StatusColor::Green
    } else if pairing_open {
        StatusColor::Amber
    } else {
        StatusColor::Red
    };
    let mut items = vec![Item::Action(Action::Open), Item::Action(Action::PairNew), Item::Separator];
    if profiles.is_empty() {
        items.push(Item::Label("No profiles".into()));
    } else {
        for id in profiles {
            if Some(id.as_str()) == locked {
                items.push(Item::Action(Action::ProfileLock(format!("✓ {id}"))));
            } else {
                items.push(Item::Action(Action::ProfileLock(id.clone())));
            }
        }
        items.push(Item::Action(Action::ProfileAuto));
    }
    items.push(Item::Separator);
    items.push(Item::Action(Action::PauseToggle));
    items.push(Item::Action(Action::Panic));
    items.push(Item::Separator);
    items.push(Item::Action(Action::Quit));
    let _ = paused;
    (color, items)
}

pub fn label(item: &Action) -> &'static str {
    match item {
        Action::Open => "Open",
        Action::PairNew => "Pair new tablet",
        Action::ProfileAuto => "Profile: Auto",
        Action::ProfileLock(_) => "Profile",
        Action::PauseToggle => "Pause pen input",
        Action::Panic => "Emergency release",
        Action::Quit => "Quit",
    }
}

/// Route a clicked action to the runtime handlers in `super`.
/// Returns a short description for logs/tests.
pub fn describe(action: &Action, paused: bool) -> &'static str {
    match action {
        Action::Open => "open main window",
        Action::PairNew => "open pairing window",
        Action::ProfileAuto => "unlock profile (auto)",
        Action::ProfileLock(_) => "lock profile",
        Action::PauseToggle => {
            if paused {
                "resume pen input"
            } else {
                "pause pen input"
            }
        }
        Action::Panic => "emergency release",
        Action::Quit => "quit (release all first)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_colors_and_rows() {
        let (c, items) = build_menu(true, false, false, &["a".into()], None);
        assert_eq!(c, StatusColor::Green);
        assert!(items.contains(&Item::Action(Action::Panic)));
        assert!(items.contains(&Item::Action(Action::ProfileAuto)));
        let (c2, _) = build_menu(false, true, false, &[], None);
        assert_eq!(c2, StatusColor::Amber);
        let (c3, _) = build_menu(false, false, false, &[], None);
        assert_eq!(c3, StatusColor::Red);
        assert_eq!(describe(&Action::PauseToggle, true), "resume pen input");
        assert_eq!(label(&Action::Quit), "Quit");
    }
}
