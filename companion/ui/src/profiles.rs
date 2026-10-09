//! Button-strip profiles (spec 8, 11): loading, selection (manual lock >
//! foreground app match > default) and the wire form sent to the tablet.

use chiz_core::profile::{self, Profile};

pub struct ProfileSet {
    pub profiles: Vec<Profile>,
    /// Manual lock (profile id); `None` = follow the foreground app.
    pub locked: Option<String>,
    /// Last stable foreground app, set by the foreground watcher.
    pub foreground: Option<String>,
}

impl ProfileSet {
    pub fn new(profiles: Vec<Profile>) -> Self {
        Self {
            profiles,
            locked: None,
            foreground: None,
        }
    }

    pub fn current(&self) -> Option<&Profile> {
        profile::select_profile(
            &self.profiles,
            self.foreground.as_deref(),
            self.locked.as_deref(),
        )
    }

    pub fn current_id(&self) -> Option<String> {
        self.current().map(|p| p.id.clone())
    }

    /// Wire message for the active profile.
    pub fn current_wire(&self, reason: &str) -> Option<serde_json::Value> {
        self.current()
            .map(|p| wire(p, reason, self.foreground.as_deref()))
    }

    /// Record a new foreground app. Returns true if the active profile changed.
    pub fn set_foreground(&mut self, app: Option<String>) -> bool {
        let before = self.current_id();
        self.foreground = app;
        before != self.current_id()
    }

    /// `auto`, `next`, `prev` or a profile id. Returns (id, reason) when the
    /// target is valid; releasing held inputs is the caller's job.
    pub fn switch(&mut self, target: &str) -> Option<(String, &'static str)> {
        if target == "auto" {
            self.locked = None;
            return self.current_id().map(|id| (id, "app"));
        }
        let ids: Vec<&str> = self.profiles.iter().map(|p| p.id.as_str()).collect();
        if ids.is_empty() {
            return None;
        }
        let id = match target {
            "next" | "prev" => {
                let cur = self.current_id().unwrap_or_default();
                let i = ids.iter().position(|x| *x == cur).unwrap_or(0);
                let n = ids.len();
                let j = if target == "next" {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                ids[j].to_string()
            }
            other if ids.contains(&other) => other.to_string(),
            _ => return None,
        };
        self.locked = Some(id.clone());
        Some((id, "manual"))
    }
}

/// Load `*.json` profiles: a `profiles` folder beside the executable, then
/// CWD-relative dev locations, then the profiles built into the binary.
pub fn load() -> ProfileSet {
    const EMBEDDED: &[&str] = &[
        include_str!("../../profiles/default.json"),
        include_str!("../../profiles/krita.json"),
    ];
    let mut dirs: Vec<std::path::PathBuf> = vec![];
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("profiles")))
    {
        dirs.push(dir);
    }
    dirs.extend(["companion/profiles", "profiles", "../profiles"].map(Into::into));
    let mut profiles = vec![];
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for p in entries.flatten().map(|e| e.path()) {
            if p.extension().is_none_or(|x| x != "json") {
                continue;
            }
            match std::fs::read_to_string(&p)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str::<Profile>(&t).map_err(|e| e.to_string()))
            {
                Ok(prof) if !profile::validate_profile(&prof).is_empty() => {
                    eprintln!("profile skipped (invalid): {}", p.display())
                }
                Ok(prof) if p.file_stem().is_none_or(|s| s != prof.id.as_str()) => {
                    eprintln!("profile skipped (file name != id): {}", p.display())
                }
                Ok(prof) => profiles.push(prof),
                Err(e) => eprintln!("profile skipped ({e}): {}", p.display()),
            }
        }
        if !profiles.is_empty() {
            eprintln!("profiles loaded from {}", dir.display());
            break;
        }
    }
    if profiles.is_empty() {
        profiles = EMBEDDED
            .iter()
            .filter_map(|t| serde_json::from_str::<Profile>(t).ok())
            .filter(|p| profile::validate_profile(p).is_empty())
            .collect();
        eprintln!("profiles: using the {} built-in defaults", profiles.len());
    }
    profiles.sort_by(|a, b| a.id.cmp(&b.id));
    for e in profile::validate_set(&profiles) {
        eprintln!("profile set: {e}");
    }
    ProfileSet::new(profiles)
}

/// Wire-format `profile` message (spec 11). The tablet ignores unknown fields.
pub fn wire(p: &Profile, reason: &str, app: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "t": "profile",
        "profile": {
            "version": 1, "id": p.id, "name": p.name, "apps": p.apps,
            "default": p.default,
            "grid": {"cols": p.grid.cols, "rows": p.grid.rows},
            "buttons": p.buttons.iter().map(|b| serde_json::json!({
                "id": b.id, "label": b.label, "speak": b.speak_text(),
                "kind": b.kind, "col": b.col, "row": b.row,
                "colspan": b.colspan, "rowspan": b.rowspan,
                "action": action_wire(&b.action),
            })).collect::<Vec<_>>(),
        },
        "reason": reason,
        "app": app,
    })
}

fn action_wire(a: &profile::Action) -> serde_json::Value {
    use profile::{Action, MacroStep};
    match a {
        Action::Key(k) => {
            let mut v = serde_json::json!({"type": "key", "keys": k.keys});
            if let Some(off) = &k.keys_off {
                v["keys_off"] = off.clone().into();
            }
            v
        }
        Action::Macro(m) => {
            serde_json::json!({"type": "macro", "steps": m.steps.iter().map(|s| match s {
            MacroStep::Keys { keys } => serde_json::json!({"keys": keys}),
            MacroStep::Wait { wait_ms } => serde_json::json!({"wait_ms": wait_ms}),
        }).collect::<Vec<_>>()})
        }
        Action::Profile(p) => serde_json::json!({"type": "profile", "target": p.target}),
        Action::EraserToggle => serde_json::json!({"type": "eraser_toggle"}),
        Action::Mouse(m) => serde_json::json!({"type": "mouse", "button": m.button}),
        Action::None => serde_json::json!({"type": "none"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set() -> ProfileSet {
        let ps = load(); // built-ins or the repo's profiles: default + krita
        assert!(ps.profiles.len() >= 2);
        ps
    }

    #[test]
    fn selection_lock_and_foreground() {
        let mut ps = set();
        assert_eq!(ps.current_id().as_deref(), Some("default"));
        assert!(ps.set_foreground(Some("krita".into())));
        assert_eq!(ps.current_id().as_deref(), Some("krita"));
        assert!(!ps.set_foreground(Some("krita".into()))); // no change
        assert_eq!(ps.switch("default"), Some(("default".into(), "manual")));
        assert!(!ps.set_foreground(Some("krita.exe".into()))); // locked wins
        assert_eq!(ps.switch("auto"), Some(("krita".into(), "app")));
        assert_eq!(ps.switch("nope"), None);
    }

    #[test]
    fn wire_has_buttons_and_app() {
        let ps = set();
        let w = ps.current_wire("connect").unwrap();
        assert_eq!(w["t"], "profile");
        assert_eq!(w["reason"], "connect");
        assert!(w["profile"]["buttons"].as_array().unwrap().len() >= 4);
        assert!(w["app"].is_null());
    }
}
