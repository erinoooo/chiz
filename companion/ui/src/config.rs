//! `config.json`: located via `$CHIZ_CONFIG`, the data dir, or beside the
//! executable. Missing file = defaults; a broken file is reported and
//! ignored; out-of-range values fall back per field.

use std::path::{Path, PathBuf};

use chiz_core::failsafe::DEFAULT_PANIC_HOTKEY;

#[derive(Clone, Debug)]
pub struct Config {
    pub control_port: u16,
    pub pen_port: u16,
    /// Accept connections from non-private addresses (spec 12.7). Off.
    pub allow_public: bool,
    pub panic_hotkey: String,
    pub desktop_w: i32,
    pub desktop_h: i32,
    pub area: (f64, f64, f64, f64),
    pub keep_aspect: bool,
    pub rotation: u16,
    pub pressure_curve: [f64; 4],
}

impl Default for Config {
    fn default() -> Self {
        Self {
            control_port: 47800,
            pen_port: 47801,
            allow_public: false,
            panic_hotkey: DEFAULT_PANIC_HOTKEY.into(),
            desktop_w: 1920,
            desktop_h: 1080,
            area: (0.0, 0.0, 1.0, 1.0),
            keep_aspect: true,
            rotation: 0,
            pressure_curve: [0.25, 0.25, 0.75, 0.75],
        }
    }
}

/// `$CHIZ_CONFIG`, else `config.json` in the data dir, else beside the exe.
pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("CHIZ_CONFIG") {
        return p.into();
    }
    let user = crate::identity::data_dir().join("config.json");
    if user.exists() {
        return user;
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("config.json")))
        .unwrap_or(user)
}

impl Config {
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                eprintln!("cannot read {}: {e}; using defaults", path.display());
                return Self::default();
            }
        };
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => {
                eprintln!("config: {}", path.display());
                Self::from_json(&v)
            }
            Err(e) => {
                eprintln!("{} is not valid JSON ({e}); using defaults", path.display());
                Self::default()
            }
        }
    }

    pub fn from_json(v: &serde_json::Value) -> Self {
        let mut cfg = Self::default();
        let port = |key: &str, cur: u16| -> u16 {
            match v.pointer(&format!("/ports/{key}")).and_then(|p| p.as_u64()) {
                None => cur,
                Some(p) => match u16::try_from(p).ok().filter(|p| *p != 0) {
                    Some(p) => p,
                    None => {
                        eprintln!("ports.{key} {p} out of range; using {cur}");
                        cur
                    }
                },
            }
        };
        cfg.control_port = port("control", cfg.control_port);
        cfg.pen_port = port("pen", cfg.pen_port);
        if let Some(a) = v.get("allow_public_source").and_then(|a| a.as_bool()) {
            cfg.allow_public = a;
        }
        if let Some(h) = v.get("panic_hotkey").and_then(|h| h.as_str()) {
            cfg.panic_hotkey = h.to_string();
        }
        if let Some(w) = v.pointer("/desktop/w").and_then(|w| w.as_i64()) {
            cfg.desktop_w = w.clamp(1, 65535) as i32;
        }
        if let Some(h) = v.pointer("/desktop/h").and_then(|h| h.as_i64()) {
            cfg.desktop_h = h.clamp(1, 65535) as i32;
        }
        if let Some(a) = v.pointer("/mapping/area") {
            let g = |k: &str| a.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
            let (mut w, mut h) = (g("w"), g("h"));
            if !(w > 0.0 && w <= 1.0) {
                w = 1.0;
            }
            if !(h > 0.0 && h <= 1.0) {
                h = 1.0;
            }
            let x = g("x").clamp(0.0, 1.0 - w);
            let y = g("y").clamp(0.0, 1.0 - h);
            cfg.area = (x, y, w, h);
        }
        if let Some(k) = v.pointer("/mapping/keep_aspect").and_then(|k| k.as_bool()) {
            cfg.keep_aspect = k;
        }
        if let Some(r) = v.pointer("/mapping/rotation").and_then(|r| r.as_u64()) {
            if matches!(r, 0 | 90 | 180 | 270) {
                cfg.rotation = r as u16;
            }
        }
        if let Some(c) = v.get("pressure_curve").and_then(|c| c.as_array()) {
            if c.len() == 4 {
                for (i, x) in c.iter().enumerate() {
                    if let Some(f) = x.as_f64().filter(|f| (0.0..=1.0).contains(f)) {
                        cfg.pressure_curve[i] = f;
                    }
                }
            }
        }
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_range_checks() {
        let c = Config::from_json(&serde_json::json!({
            "ports": {"control": 70000, "pen": 5000},
            "mapping": {"area": {"x": 0.9, "y": 0, "w": 0.5, "h": 2}, "rotation": 45},
            "pressure_curve": [0.1, 2.0, 0.3, 0.4],
        }));
        assert_eq!(c.control_port, 47800); // out of range -> default
        assert_eq!(c.pen_port, 5000);
        assert_eq!(c.area, (0.5, 0.0, 0.5, 1.0)); // clamped inside the screen
        assert_eq!(c.rotation, 0); // invalid rotation ignored
        assert_eq!(c.pressure_curve, [0.1, 0.25, 0.3, 0.4]); // bad element kept default
    }
}
