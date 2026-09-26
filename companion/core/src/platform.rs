//! OS platform interface (spec 8): the core talks to the OS only through
//! this. Windows and Linux implement it; everything else is shared.
//!
//! ```text
//! Monitor    { id, name, x, y, w, h, primary }  // physical px, vdesk coords
//! PenEvent   { phase, x_px, y_px, pressure 0..1, tilt_x/y deg,
//!              distance 0..1, eraser, barrel1, barrel2 }
//! monitors() -> Vec<Monitor>
//! inject_pen(PenEvent)
//! inject_key(key, down)        // key names from section 11
//! inject_mouse(button, down)   // left/right/middle at current pointer
//! foreground_app() -> Option<String>
//! release_all()                // lift pen, leave, release keys+buttons
//! ```

#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub id: String,
    pub name: String,
    pub x: i64,
    pub y: i64,
    pub w: u32,
    pub h: u32,
    pub primary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PenPhase {
    Hover,
    Down,
    Move,
    Up,
    Leave,
}

#[derive(Clone, Copy, Debug)]
pub struct PenEvent {
    pub phase: PenPhase,
    pub x_px: i64,
    pub y_px: i64,
    pub pressure: f64, // 0..1, AFTER the curve
    pub tilt_x: i8,
    pub tilt_y: i8,
    pub distance: f64, // 0..1
    pub eraser: bool,
    pub barrel1: bool,
    pub barrel2: bool,
}

pub trait Platform {
    fn monitors(&mut self) -> Vec<Monitor>;
    fn inject_pen(&mut self, ev: PenEvent);
    fn inject_key(&mut self, key: &str, down: bool);
    fn inject_mouse(&mut self, button: &str, down: bool);
    fn foreground_app(&mut self) -> Option<String>;
    fn release_all(&mut self);
}

/// UDP-loss repair (spec 8): tracks `in_range`/`in_contact` (+ tool) and
/// repairs the sequence before each `inject_pen`. Both platform layers use
/// these rules; the tablet-side `cancel` maps to up, then leave.
#[derive(Default, Debug)]
pub struct PenState {
    pub in_range: bool,
    pub in_contact: bool,
    pub eraser_in_range: Option<bool>,
}

impl PenState {
    /// Repair a record phase; returns the phases to inject in order.
    /// `tool_switch` reports when the eraser end changed mid-contact (the
    /// caller injects up+leave between the two tools).
    pub fn repair(&mut self, phase: PenPhase, eraser: bool) -> Vec<PenPhase> {
        use PenPhase::*;
        match phase {
            Down | Move => {
                let mut out = vec![];
                if !self.in_range {
                    out.push(Hover);
                    self.in_range = true;
                    self.eraser_in_range = Some(eraser);
                } else if Some(eraser) != self.eraser_in_range {
                    if self.in_contact {
                        // Tool switch mid-contact: up + leave first (caller
                        // re-emits hover for the new tool from this return).
                        self.in_contact = false;
                        self.in_range = false;
                        return vec![Up, Leave, Hover, phase];
                    }
                    out.push(Leave);
                    out.push(Hover);
                    self.eraser_in_range = Some(eraser);
                }
                if phase == Move && !self.in_contact {
                    out.push(Down);
                    self.in_contact = true;
                }
                if phase == Down {
                    self.in_contact = true;
                }
                out.push(phase);
                out
            }
            Hover => {
                let mut out = vec![];
                if self.in_contact {
                    out.push(Up);
                    self.in_contact = false;
                }
                if !self.in_range {
                    self.in_range = true;
                    self.eraser_in_range = Some(eraser);
                }
                out.push(Hover);
                out
            }
            Up => {
                if !self.in_contact {
                    return vec![]; // duplicate up: ignore
                }
                self.in_contact = false;
                vec![Up]
            }
            Leave => {
                if !self.in_range {
                    return vec![]; // duplicate leave: ignore
                }
                let mut out = vec![];
                if self.in_contact {
                    out.push(Up);
                    self.in_contact = false;
                }
                self.in_range = false;
                out.push(Leave);
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_vectors() {
        use PenPhase::*;
        let mut s = PenState::default();
        assert_eq!(s.repair(Down, false), vec![Hover, Down]); // down w/o hover
        let mut s = PenState {
            in_range: true,
            in_contact: false,
            eraser_in_range: Some(false),
        };
        assert_eq!(s.repair(Move, false), vec![Down, Move]); // move w/o down
        let mut s = PenState {
            in_range: true,
            in_contact: true,
            eraser_in_range: Some(false),
        };
        assert_eq!(s.repair(Hover, false), vec![Up, Hover]); // hover in contact
        assert_eq!(PenState::default().repair(Up, false), vec![]); // dup up
        assert_eq!(PenState::default().repair(Leave, false), vec![]); // dup leave
        // Tool switch mid-contact forces up+leave first.
        let mut s = PenState {
            in_range: true,
            in_contact: true,
            eraser_in_range: Some(false),
        };
        assert_eq!(s.repair(Move, true), vec![Up, Leave, Hover, Move]);
    }
}
