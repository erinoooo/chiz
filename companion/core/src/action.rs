//! Button action dispatch (spec 7, 8): button events -> platform effects.
//!
//! The tablet decides *when* a button activates and sends `button down/up`;
//! the PC runs the action on `down`. Unknown button ids are ignored. The
//! runner is pure (returns effects) so it is unit-testable; `ui` maps
//! effects to `SendInput`/uinput calls on a worker thread (never the pen
//! path). All held keys/buttons are released via `release_all` when the
//! session ends, the profile changes, or the app quits.

use std::collections::{HashMap, HashSet};

use super::profile::{parse_keys, plan_key_press, Action, Button, MacroStep};

/// OS injection surface (spec 8 platform interface, key/mouse subset).
/// Monitor/pen/foreground halves live in the platform crates; this trait
/// covers what button actions need.
pub trait Platform {
    fn inject_key(&mut self, key: &str, down: bool);
    fn inject_mouse(&mut self, button: &str, down: bool);
    fn sleep_ms(&mut self, ms: u32) {
        let _ = ms;
    }
}

/// Effects for the UI layer to execute in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Key { key: String, down: bool },
    Mouse { button: String, down: bool },
    WaitMs(u32),
    /// New toggle state to report to the tablet (`button_state`) + speak
    /// "<label> on/off" when speech is enabled.
    ToggleState { id: String, on: bool },
    /// Eraser mode flipped (forces eraser flag on pen events while on).
    Eraser(bool),
    /// Switch profile: id, `next`, `prev`, or `auto` (unlock).
    SwitchProfile { target: String },
}

/// Tracks held keys/mouse buttons, toggle states, and eraser mode.
#[derive(Default)]
pub struct ActionRunner {
    held_keys: Vec<String>, // press order; released in reverse
    held_mouse: HashSet<String>,
    toggles: HashMap<String, bool>,
    pub eraser_on: bool,
}

impl ActionRunner {
    /// Dispatch a `button` message phase for a known button. Unknown ids
    /// never reach here (caller ignores them). Returns ordered effects.
    pub fn on_button(&mut self, button: &Button, phase: &str) -> Vec<Effect> {
        match button.kind.as_str() {
            "tap" | "toggle" => {
                // Tablet sends `up` immediately after `down` for these; the
                // PC acts on `down` and ignores `up`.
                if phase != "down" {
                    return vec![];
                }
                self.fire(button)
            }
            "hold" => {
                if phase == "down" {
                    self.press_hold(button)
                } else if phase == "up" {
                    self.release_hold()
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    fn fire(&mut self, button: &Button) -> Vec<Effect> {
        match &button.action {
            Action::None => vec![],
            Action::Key(k) => {
                if button.kind == "toggle" {
                    let on = !self.toggles.get(&button.id).copied().unwrap_or(false);
                    self.toggles.insert(button.id.clone(), on);
                    let keys_str = if on {
                        &k.keys
                    } else {
                        k.keys_off.as_ref().unwrap_or(&k.keys)
                    };
                    let keys = parse_keys(keys_str).unwrap_or_default();
                    let mut fx: Vec<Effect> = plan_key_press(&keys, false, false)
                        .into_iter()
                        .map(|(key, down)| Effect::Key { key, down })
                        .collect();
                    fx.push(Effect::ToggleState {
                        id: button.id.clone(),
                        on,
                    });
                    fx
                } else {
                    let keys = parse_keys(&k.keys).unwrap_or_default();
                    plan_key_press(&keys, false, false)
                        .into_iter()
                        .map(|(key, down)| Effect::Key { key, down })
                        .collect()
                }
            }
            Action::Macro(m) => {
                let mut fx = vec![];
                for s in &m.steps {
                    match s {
                        MacroStep::Keys { keys } => {
                            for (key, down) in plan_key_press(&parse_keys(keys).unwrap_or_default(), false, false) {
                                fx.push(Effect::Key { key, down });
                            }
                        }
                        MacroStep::Wait { wait_ms } => fx.push(Effect::WaitMs(*wait_ms)),
                    }
                }
                fx
            }
            Action::EraserToggle => {
                // Only valid on toggle buttons (validated at load).
                let on = !self.toggles.get(&button.id).copied().unwrap_or(false);
                self.toggles.insert(button.id.clone(), on);
                self.eraser_on = on;
                vec![
                    Effect::Eraser(on),
                    Effect::ToggleState {
                        id: button.id.clone(),
                        on,
                    },
                ]
            }
            Action::Mouse(m) => vec![Effect::Mouse {
                button: m.button.clone(),
                down: true,
            }, Effect::Mouse {
                button: m.button.clone(),
                down: false,
            }],
            Action::Profile(p) => vec![Effect::SwitchProfile {
                target: p.target.clone(),
            }],
        }
    }

    fn press_hold(&mut self, button: &Button) -> Vec<Effect> {
        match &button.action {
            Action::Key(k) => {
                let keys = parse_keys(&k.keys).unwrap_or_default();
                let seq = plan_key_press(&keys, true, false);
                self.held_keys = seq.iter().filter(|(_, d)| *d).map(|(k, _)| k.clone()).collect();
                seq.into_iter().map(|(key, down)| Effect::Key { key, down }).collect()
            }
            Action::Mouse(m) => {
                self.held_mouse.insert(m.button.clone());
                vec![Effect::Mouse {
                    button: m.button.clone(),
                    down: true,
                }]
            }
            // Macro on hold is rejected by validation; ignore defensively.
            _ => vec![],
        }
    }

    fn release_hold(&mut self) -> Vec<Effect> {
        let mut fx: Vec<Effect> = self
            .held_keys
            .iter()
            .rev()
            .map(|k| Effect::Key {
                key: k.clone(),
                down: false,
            })
            .collect();
        for b in self.held_mouse.drain().collect::<Vec<_>>() {
            fx.push(Effect::Mouse { button: b, down: false });
        }
        self.held_keys.clear();
        fx
    }

    /// Release every held key/button (session end, profile change, quit).
    /// A hold still engaged when the profile changes is released first.
    pub fn release_all(&mut self) -> Vec<Effect> {
        self.release_hold()
    }

    /// Apply effects to a platform (used by platform crates' tests too).
    pub fn execute<P: Platform>(&self, platform: &mut P, effects: &[Effect]) {
        for fx in effects {
            match fx {
                Effect::Key { key, down } => platform.inject_key(key, *down),
                Effect::Mouse { button, down } => platform.inject_mouse(button, *down),
                Effect::WaitMs(ms) => platform.sleep_ms(*ms),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Action as A, Button as B, KeyAction, MouseAction, ProfileAction};

    fn btn(id: &str, kind: &str, action: A) -> B {
        B {
            id: id.into(),
            label: id.into(),
            speak: String::new(),
            kind: kind.into(),
            col: 0,
            row: 0,
            colspan: 1,
            rowspan: 1,
            action,
        }
    }

    fn key_btn(id: &str, kind: &str, keys: &str) -> B {
        btn(id, kind, A::Key(KeyAction {
            keys: keys.into(),
            keys_off: None,
        }))
    }

    #[test]
    fn tap_key_sequence() {
        let mut r = ActionRunner::default();
        let b = key_btn("undo", "tap", "ctrl+z");
        let fx = r.on_button(&b, "down");
        assert_eq!(
            fx,
            vec![
                Effect::Key { key: "ctrl".into(), down: true },
                Effect::Key { key: "z".into(), down: true },
                Effect::Key { key: "z".into(), down: false },
                Effect::Key { key: "ctrl".into(), down: false },
            ]
        );
        assert!(r.on_button(&b, "up").is_empty()); // tap up ignored
    }

    #[test]
    fn hold_press_release_and_session_release() {
        let mut r = ActionRunner::default();
        let b = key_btn("pan", "hold", "space");
        assert_eq!(r.on_button(&b, "down"), vec![Effect::Key { key: "space".into(), down: true }]);
        assert_eq!(r.on_button(&b, "up"), vec![Effect::Key { key: "space".into(), down: false }]);
        // Held at session end -> released.
        r.on_button(&b, "down");
        assert_eq!(r.release_all(), vec![Effect::Key { key: "space".into(), down: false }]);
        assert!(r.release_all().is_empty());
    }

    #[test]
    fn toggle_key_uses_keys_off_and_reports_state() {
        let mut r = ActionRunner::default();
        let b = btn(
            "brush-toggle",
            "toggle",
            A::Key(KeyAction {
                keys: "b".into(),
                keys_off: Some("v".into()),
            }),
        );
        let on = r.on_button(&b, "down");
        assert!(on.contains(&Effect::ToggleState { id: "brush-toggle".into(), on: true }));
        assert!(on.contains(&Effect::Key { key: "b".into(), down: true }));
        let off = r.on_button(&b, "down");
        assert!(off.contains(&Effect::ToggleState { id: "brush-toggle".into(), on: false }));
        assert!(off.contains(&Effect::Key { key: "v".into(), down: true }));
    }

    #[test]
    fn eraser_toggle_flips_mode() {
        let mut r = ActionRunner::default();
        let b = btn("eraser", "toggle", A::EraserToggle);
        assert_eq!(
            r.on_button(&b, "down"),
            vec![
                Effect::Eraser(true),
                Effect::ToggleState { id: "eraser".into(), on: true }
            ]
        );
        assert!(r.eraser_on);
        r.on_button(&b, "down");
        assert!(!r.eraser_on);
    }

    #[test]
    fn macro_steps_and_mouse() {
        use crate::profile::{MacroAction, MacroStep};
        let mut r = ActionRunner::default();
        let b = btn(
            "m",
            "tap",
            A::Macro(MacroAction {
                steps: vec![
                    MacroStep::Keys { keys: "ctrl+s".into() },
                    MacroStep::Wait { wait_ms: 100 },
                ],
            }),
        );
        let fx = r.on_button(&b, "down");
        assert_eq!(fx[0], Effect::Key { key: "ctrl".into(), down: true });
        assert!(fx.contains(&Effect::WaitMs(100)));
        let click = btn("c", "tap", A::Mouse(MouseAction { button: "right".into() }));
        assert_eq!(
            r.on_button(&click, "down"),
            vec![
                Effect::Mouse { button: "right".into(), down: true },
                Effect::Mouse { button: "right".into(), down: false },
            ]
        );
        let sw = btn("s", "tap", A::Profile(ProfileAction { target: "krita".into() }));
        assert_eq!(
            r.on_button(&sw, "down"),
            vec![Effect::SwitchProfile { target: "krita".into() }]
        );
    }

    #[test]
    fn execute_drives_platform() {
        struct Rec {
            log: Vec<(String, bool)>,
            waits: Vec<u32>,
        }
        impl Platform for Rec {
            fn inject_key(&mut self, key: &str, down: bool) {
                self.log.push((key.into(), down));
            }
            fn inject_mouse(&mut self, _b: &str, _d: bool) {}
            fn sleep_ms(&mut self, ms: u32) {
                self.waits.push(ms);
            }
        }
        let r = ActionRunner::default();
        let mut p = Rec { log: vec![], waits: vec![] };
        r.execute(&mut p, &[
            Effect::Key { key: "ctrl".into(), down: true },
            Effect::WaitMs(50),
            Effect::Key { key: "ctrl".into(), down: false },
        ]);
        assert_eq!(p.log, vec![("ctrl".to_string(), true), ("ctrl".to_string(), false)]);
        assert_eq!(p.waits, vec![50]);
    }
}
