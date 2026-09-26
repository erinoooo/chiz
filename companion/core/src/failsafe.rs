//! Emergency off switch (failsafe): guarantees the user can always force
//! the cursor and keys to release, even if the app locks up.
//!
//! Layers (independent of each other and of the pen path):
//! 1. Automatic: the pen watchdogs (750 ms down / 1000 ms hover) already
//!    lift a stuck pen. On top, this module counts *consecutive* stuck-pen
//!    trips: after 3 in a row with no clean input, pen input auto-pauses
//!    (status banner, manual resume required). Buttons keep working.
//! 2. PC panic: a global OS hotkey (default `ctrl+alt+shift+f12`) plus a
//!    tray "Emergency release" item. Both run outside the session tasks and
//!    do only: release_all + pause pen + banner. Never touches UDP/TCP.
//! 3. Tablet panic gesture: 3-finger tap on the strip (see android spec)
//!    stops UDP first, then closes control with `bye`. The server's session
//!    teardown releases everything even if the gesture packets are lost
//!    (dead connection <= 5 s, pen watchdog <= 750 ms).
//!
//! While paused (manual, panic, or auto), the companion drops decoded pen
//! records and injects `leave` once; buttons keep working (spec 8 Pause).

use super::profile::parse_keys;

/// Why pen input is currently paused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PauseReason {
    Manual,
    Panic,
    AutoStuck,
}

/// Consecutive stuck-pen watchdog trips before auto-pause.
pub const STUCK_TRIPS_TO_AUTOPAUSE: u32 = 3;

/// Default panic hotkey. F12 exists on every keyboard; with three modifiers
/// it is near-impossible to hit by accident. Configurable via
/// `panic_hotkey` in config.json (same `keys` grammar as buttons).
pub const DEFAULT_PANIC_HOTKEY: &str = "ctrl+alt+shift+f12";

#[derive(Debug)]
pub struct Failsafe {
    paused: Option<PauseReason>,
    stuck_trips: u32,
    /// Set when paused so the injector emits one `leave` on transition.
    pub leave_pending: bool,
    pub panic_hotkey: Vec<String>,
}

impl Default for Failsafe {
    fn default() -> Self {
        Self {
            paused: None,
            stuck_trips: 0,
            leave_pending: false,
            panic_hotkey: parse_keys(DEFAULT_PANIC_HOTKEY).expect("default hotkey"),
        }
    }
}

impl Failsafe {
    /// Attempt to pause. Returns true if this call transitioned to paused.
    pub fn pause(&mut self, reason: PauseReason) -> bool {
        if self.paused.is_some() {
            return false;
        }
        self.paused = Some(reason);
        self.leave_pending = true;
        true
    }

    pub fn resume(&mut self) {
        self.paused = None;
        self.stuck_trips = 0;
        self.leave_pending = false;
    }

    pub fn paused(&self) -> Option<PauseReason> {
        self.paused
    }

    /// Panic entry point for the hotkey/tray thread. Idempotent: safe to
    /// hammer repeatedly. Returns true if pen was newly paused (caller then
    /// runs release_all + banner).
    pub fn emergency_release(&mut self) -> bool {
        self.stuck_trips = 0;
        self.pause(PauseReason::Panic)
    }

    /// Call when a watchdog fires a stuck-pen repair (`up+leave`/`leave`).
    /// Auto-pauses after STUCK_TRIPS_TO_AUTOPAUSE consecutive trips.
    /// Returns the reason if this call paused.
    pub fn note_stuck_trip(&mut self) -> Option<PauseReason> {
        self.stuck_trips += 1;
        if self.stuck_trips >= STUCK_TRIPS_TO_AUTOPAUSE {
            self.stuck_trips = 0;
            self.pause(PauseReason::AutoStuck);
            return Some(PauseReason::AutoStuck);
        }
        None
    }

    /// Call when a clean (non-repaired) pen record is injected.
    pub fn note_clean_input(&mut self) {
        self.stuck_trips = 0;
    }

    /// Should a decoded pen record be dropped? (Buttons always pass.)
    pub fn drop_pen(&self) -> bool {
        self.paused.is_some()
    }

    /// Validate a configured panic hotkey string.
    pub fn set_hotkey(&mut self, s: &str) -> Result<(), String> {
        self.panic_hotkey = parse_keys(s)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_resume_latch() {
        let mut f = Failsafe::default();
        assert!(!f.drop_pen());
        assert!(f.pause(PauseReason::Manual));
        assert!(!f.pause(PauseReason::Panic)); // already paused: no re-transition
        assert!(f.drop_pen());
        assert!(f.leave_pending); // injector emits one leave
        f.resume();
        assert!(!f.drop_pen());
    }

    #[test]
    fn panic_is_idempotent() {
        let mut f = Failsafe::default();
        assert!(f.emergency_release());
        assert!(!f.emergency_release());
        assert_eq!(f.paused(), Some(PauseReason::Panic));
    }

    #[test]
    fn three_stuck_trips_autopause() {
        let mut f = Failsafe::default();
        assert_eq!(f.note_stuck_trip(), None);
        assert_eq!(f.note_stuck_trip(), None);
        assert_eq!(f.note_stuck_trip(), Some(PauseReason::AutoStuck));
        assert!(f.drop_pen());
        // Clean input resets the counter.
        f.resume();
        f.note_stuck_trip();
        f.note_clean_input();
        f.note_stuck_trip();
        assert_eq!(f.note_stuck_trip(), None); // only 2 since reset
    }

    #[test]
    fn default_hotkey_parses_and_custom_validates() {
        let f = Failsafe::default();
        assert_eq!(f.panic_hotkey, vec!["ctrl", "alt", "shift", "f12"]);
        let mut g = Failsafe::default();
        assert!(g.set_hotkey("ctrl+alt+p").is_ok());
        assert!(g.set_hotkey("ctrl+bogus").is_err());
    }
}
