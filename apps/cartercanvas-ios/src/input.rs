//! UIKit touches, Apple Pencil samples, pointer hover and hardware keys, turned into egui input.
//!
//! - **Apple Pencil** reaches the canvas at once: each sample writes pressure, tilt and roll into
//!   the app's stylus feed (as the desktop tablet monitors and the web runner do) and moves the
//!   egui pointer, so brushes get every coalesced sample with its pressure.
//! - **One finger** presses after [`FINGER_DELAY`] (or as soon as it moves [`FINGER_SLOP`]
//!   points): a second finger landing within that time turns the touch into a two-finger gesture
//!   instead of a stroke that would have to be undone.
//! - **Two or more fingers** are a gesture: egui's multi-touch gets their touch events (PhotoCraft
//!   zooms and rotates the view from them), the centroid's movement pans the view as a scroll,
//!   and a quick tap with two fingers is Undo, with three Redo.

use std::collections::HashMap;

use egui::{Event, Modifiers, PointerButton, Pos2, TouchDeviceId, TouchId, TouchPhase, Vec2};
use photocraft_ui_egui::stylus::{PenSample, StylusFeed};

/// How long a lone finger waits for a second one before it presses (seconds).
pub const FINGER_DELAY: f64 = 0.06;
/// How far (points) a lone finger moves before it presses without waiting.
pub const FINGER_SLOP: f32 = 12.0;
/// A gesture shorter than this (seconds) that moved less than [`TAP_SLOP`] is a tap.
const TAP_TIME: f64 = 0.3;
const TAP_SLOP: f32 = 16.0;

const FINGER_DEVICE: TouchDeviceId = TouchDeviceId(1);

/// One touch sample from the host, in points within the view.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CcTouch {
    /// Stable for the lifetime of the touch.
    pub id: u64,
    /// 0 began, 1 moved, 2 ended, 3 cancelled.
    pub phase: u32,
    /// 0 finger, 1 Apple Pencil.
    pub kind: u32,
    pub x: f32,
    pub y: f32,
    /// Pencil pressure 0..1 (`force / maximumPossibleForce`); ignored for fingers.
    pub pressure: f32,
    /// Pencil altitude in radians (0 = flat on the glass, π/2 = upright).
    pub altitude: f32,
    /// Pencil azimuth in radians in the view's coordinates (0 = pointing right).
    pub azimuth: f32,
    /// Pencil Pro barrel roll in radians; 0 where the Pencil doesn't report it.
    pub roll: f32,
    /// `UITouch.timestamp` in seconds.
    pub timestamp: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Began,
    Moved,
    Ended,
    Cancelled,
}

fn phase(p: u32) -> Phase {
    match p {
        0 => Phase::Began,
        1 => Phase::Moved,
        2 => Phase::Ended,
        _ => Phase::Cancelled,
    }
}

/// W3C tilt (degrees, -90..90 on each axis) from the Pencil's altitude and azimuth.
pub fn pen_tilt(altitude: f32, azimuth: f32) -> (f32, f32) {
    let alt = altitude.clamp(0.0, std::f32::consts::FRAC_PI_2);
    if alt >= std::f32::consts::FRAC_PI_2 - 1e-4 {
        return (0.0, 0.0);
    }
    let cot = alt.cos() / alt.sin().max(1e-6);
    let tx = (azimuth.cos() * cot).atan().to_degrees();
    let ty = (azimuth.sin() * cot).atan().to_degrees();
    (tx, ty)
}

#[derive(Clone, Debug)]
struct Pending {
    id: u64,
    began: f64,
    start: Pos2,
    moves: Vec<Pos2>,
}

#[derive(Clone, Debug)]
struct Gesture {
    fingers: HashMap<u64, Pos2>,
    began: f64,
    max_fingers: usize,
    travel: f32,
    centroid: Option<Pos2>,
}

impl Gesture {
    fn centroid(&self) -> Option<Pos2> {
        if self.fingers.is_empty() {
            return None;
        }
        let sum = self.fingers.values().fold(Vec2::ZERO, |acc, p| acc + p.to_vec2());
        Some((sum / self.fingers.len() as f32).to_pos2())
    }
}

/// What drives egui's pointer right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pointer {
    None,
    Finger(u64),
    Pencil(u64),
}

/// Input gathered between frames.
pub struct InputState {
    events: Vec<Event>,
    modifiers: Modifiers,
    pointer: Pointer,
    pending: Option<Pending>,
    gesture: Option<Gesture>,
    /// Commands the host should act on after the frame (Undo/Redo taps produce key events; this
    /// stays empty unless a gesture needs feedback).
    pub haptics: Vec<u32>,
    feed: StylusFeed,
}

impl InputState {
    pub fn new(feed: StylusFeed) -> Self {
        Self { events: Vec::new(), modifiers: Modifiers::NONE, pointer: Pointer::None, pending: None, gesture: None, haptics: Vec::new(), feed }
    }

    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    pub fn set_modifiers(&mut self, m: Modifiers) {
        self.modifiers = m;
    }

    pub fn push(&mut self, e: Event) {
        self.events.push(e);
    }

    /// This frame's events, after pressing a lone finger whose wait is over.
    pub fn take_events(&mut self, now: f64) -> Vec<Event> {
        if let Some(p) = &self.pending
            && now - p.began >= FINGER_DELAY
        {
            self.commit_pending();
        }
        std::mem::take(&mut self.events)
    }

    /// Whether a lone finger is still waiting, so the host keeps drawing frames until it presses.
    pub fn waiting(&self) -> bool {
        self.pending.is_some()
    }

    fn press(&mut self, pos: Pos2) {
        self.events.push(Event::PointerMoved(pos));
        self.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: self.modifiers });
    }

    fn release(&mut self, pos: Pos2) {
        self.events.push(Event::PointerMoved(pos));
        self.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: self.modifiers });
    }

    fn touch_event(&mut self, id: u64, phase: TouchPhase, pos: Pos2) {
        self.events.push(Event::Touch { device_id: FINGER_DEVICE, id: TouchId(id), phase, pos, force: None });
    }

    /// Press the waiting finger: its touch start, the press where it went down, and the moves
    /// it made while waiting.
    fn commit_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        self.pointer = Pointer::Finger(p.id);
        self.touch_event(p.id, TouchPhase::Start, p.start);
        self.press(p.start);
        for pos in p.moves {
            self.touch_event(p.id, TouchPhase::Move, pos);
            self.events.push(Event::PointerMoved(pos));
        }
    }

    pub fn touches(&mut self, touches: &[CcTouch]) {
        for t in touches {
            if t.kind == 1 {
                self.pencil(t);
            } else {
                self.finger(t);
            }
        }
    }

    fn pencil(&mut self, t: &CcTouch) {
        let pos = Pos2::new(t.x, t.y);
        let ph = phase(t.phase);
        // Fingers already gesturing keep the canvas; a Pencil touch then waits for them to lift.
        if self.gesture.is_some() && ph == Phase::Began {
            return;
        }
        match ph {
            Phase::Began => {
                // The Pencil wins over a finger still waiting to press (palm or resting finger).
                self.pending = None;
                if let Pointer::Finger(_) = self.pointer {
                    self.release(pos);
                }
                self.feed.set(Some(self.sample(t)));
                self.pointer = Pointer::Pencil(t.id);
                self.press(pos);
            }
            Phase::Moved if self.pointer == Pointer::Pencil(t.id) => {
                self.feed.set(Some(self.sample(t)));
                self.events.push(Event::PointerMoved(pos));
            }
            Phase::Ended | Phase::Cancelled if self.pointer == Pointer::Pencil(t.id) => {
                // The last sample keeps its pressure through the release, as the web runner does.
                self.feed.set(Some(self.sample(t)));
                self.release(pos);
                self.feed.set(None);
                self.pointer = Pointer::None;
            }
            _ => {}
        }
    }

    fn sample(&self, t: &CcTouch) -> PenSample {
        let (tilt_x, tilt_y) = pen_tilt(t.altitude, t.azimuth);
        PenSample { pressure: t.pressure, tilt_x, tilt_y, rotation: t.roll.to_degrees(), eraser: false }
    }

    fn finger(&mut self, t: &CcTouch) {
        let pos = Pos2::new(t.x, t.y);
        match phase(t.phase) {
            Phase::Began => self.finger_began(t.id, pos, t.timestamp),
            Phase::Moved => self.finger_moved(t.id, pos),
            Phase::Ended => self.finger_ended(t.id, pos, t.timestamp, false),
            Phase::Cancelled => self.finger_ended(t.id, pos, t.timestamp, true),
        }
    }

    fn finger_began(&mut self, id: u64, pos: Pos2, time: f64) {
        if matches!(self.pointer, Pointer::Pencil(_)) {
            // A resting hand while the Pencil draws.
            return;
        }
        if let Some(g) = &mut self.gesture {
            g.fingers.insert(id, pos);
            g.max_fingers = g.max_fingers.max(g.fingers.len());
            self.touch_event(id, TouchPhase::Start, pos);
            return;
        }
        if let Some(p) = self.pending.take() {
            // Second finger in time: a gesture, and the first finger never pressed.
            let first = p.moves.last().copied().unwrap_or(p.start);
            self.start_gesture(&[(p.id, first), (id, pos)], p.began);
            return;
        }
        if let Pointer::Finger(first) = self.pointer {
            // A second finger after the first one pressed: end its drag and gesture from here.
            self.release(pos);
            self.touch_event(first, TouchPhase::End, pos);
            self.pointer = Pointer::None;
            self.start_gesture(&[(id, pos)], time);
            return;
        }
        self.pending = Some(Pending { id, began: time, start: pos, moves: Vec::new() });
    }

    fn start_gesture(&mut self, fingers: &[(u64, Pos2)], began: f64) {
        let mut g = Gesture { fingers: HashMap::new(), began, max_fingers: 0, travel: 0.0, centroid: None };
        for &(id, pos) in fingers {
            g.fingers.insert(id, pos);
        }
        g.max_fingers = g.fingers.len();
        g.centroid = g.centroid();
        if let Some(c) = g.centroid {
            self.events.push(Event::PointerMoved(c));
        }
        self.gesture = Some(g);
        for &(id, pos) in fingers {
            self.touch_event(id, TouchPhase::Start, pos);
        }
    }

    fn finger_moved(&mut self, id: u64, pos: Pos2) {
        if let Some(p) = &mut self.pending
            && p.id == id
        {
            p.moves.push(pos);
            if (pos - p.start).length() >= FINGER_SLOP {
                self.commit_pending();
            }
            return;
        }
        if self.pointer == Pointer::Finger(id) {
            self.touch_event(id, TouchPhase::Move, pos);
            self.events.push(Event::PointerMoved(pos));
            return;
        }
        let Some(g) = &mut self.gesture else { return };
        if !g.fingers.contains_key(&id) {
            return;
        }
        g.fingers.insert(id, pos);
        let before = g.centroid;
        let after = g.centroid();
        g.centroid = after;
        self.touch_event(id, TouchPhase::Move, pos);
        if let (Some(a), Some(b)) = (before, after) {
            let delta = b - a;
            if let Some(g) = &mut self.gesture {
                g.travel += delta.length();
            }
            self.events.push(Event::PointerMoved(b));
            if delta != Vec2::ZERO {
                self.events.push(Event::MouseWheel {
                    source: egui::MouseWheelSource::Unknown,
                    unit: egui::MouseWheelUnit::Point,
                    delta,
                    phase: TouchPhase::Move,
                    modifiers: Modifiers::NONE,
                });
            }
        }
    }

    fn finger_ended(&mut self, id: u64, pos: Pos2, time: f64, cancelled: bool) {
        if let Some(p) = &self.pending
            && p.id == id
        {
            if cancelled {
                self.pending = None;
                return;
            }
            // A quick tap: press and release where it lifted.
            self.commit_pending();
            self.touch_event(id, TouchPhase::End, pos);
            self.release(pos);
            self.events.push(Event::PointerGone);
            self.pointer = Pointer::None;
            return;
        }
        if self.pointer == Pointer::Finger(id) {
            self.touch_event(id, if cancelled { TouchPhase::Cancel } else { TouchPhase::End }, pos);
            self.release(pos);
            self.events.push(Event::PointerGone);
            self.pointer = Pointer::None;
            return;
        }
        let Some(g) = &mut self.gesture else { return };
        if g.fingers.remove(&id).is_none() {
            return;
        }
        let done = g.fingers.is_empty();
        let tap = done && !cancelled && time - g.began <= TAP_TIME && g.travel <= TAP_SLOP;
        let fingers = g.max_fingers;
        g.centroid = g.centroid();
        self.touch_event(id, if cancelled { TouchPhase::Cancel } else { TouchPhase::End }, pos);
        if done {
            self.gesture = None;
            self.events.push(Event::PointerGone);
            if tap {
                self.tap_shortcut(fingers);
            }
        }
    }

    /// Two-finger tap: Undo. Three-finger tap: Redo (⌘Z / ⇧⌘Z, as the keyboard does).
    fn tap_shortcut(&mut self, fingers: usize) {
        let modifiers = match fingers {
            2 => Modifiers::COMMAND,
            3 => Modifiers::COMMAND | Modifiers::SHIFT,
            _ => return,
        };
        let modifiers = Modifiers { mac_cmd: true, ..modifiers };
        for pressed in [true, false] {
            self.events.push(Event::Key { key: egui::Key::Z, physical_key: Some(egui::Key::Z), pressed, repeat: false, modifiers });
        }
        self.haptics.push(0);
    }

    /// The Pencil hovering above the glass, or a trackpad or mouse pointer: moves the pointer when
    /// nothing is pressed (brush outlines follow it). `active` false: it left.
    pub fn hover(&mut self, x: f32, y: f32, active: bool) {
        if self.pointer != Pointer::None || self.gesture.is_some() || self.pending.is_some() {
            return;
        }
        self.events.push(if active { Event::PointerMoved(Pos2::new(x, y)) } else { Event::PointerGone });
    }

    /// Trackpad or mouse button (indirect pointer): `button` 0 primary, 1 secondary, 2 middle.
    pub fn pointer_button(&mut self, x: f32, y: f32, button: u32, pressed: bool) {
        let button = match button {
            1 => PointerButton::Secondary,
            2 => PointerButton::Middle,
            _ => PointerButton::Primary,
        };
        let pos = Pos2::new(x, y);
        self.events.push(Event::PointerMoved(pos));
        self.events.push(Event::PointerButton { pos, button, pressed, modifiers: self.modifiers });
    }

    /// Trackpad two-finger scroll or a mouse wheel, in points (content direction).
    pub fn scroll(&mut self, dx: f32, dy: f32) {
        self.events.push(Event::MouseWheel {
            source: egui::MouseWheelSource::Unknown,
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(dx, dy),
            phase: TouchPhase::Move,
            modifiers: self.modifiers,
        });
    }

    /// Trackpad pinch: multiply the zoom by `factor` around the pointer.
    pub fn pinch(&mut self, x: f32, y: f32, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.events.push(Event::PointerMoved(Pos2::new(x, y)));
            self.events.push(Event::Zoom(factor));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: u64, phase: u32, kind: u32, x: f32, y: f32, time: f64) -> CcTouch {
        CcTouch { id, phase, kind, x, y, pressure: 0.5, altitude: 1.0, azimuth: 0.0, roll: 0.0, timestamp: time }
    }

    fn presses(events: &[Event]) -> usize {
        events.iter().filter(|e| matches!(e, Event::PointerButton { pressed: true, .. })).count()
    }

    #[test]
    fn a_lone_finger_presses_after_the_delay() {
        let mut s = InputState::new(StylusFeed::default());
        s.touches(&[t(1, 0, 0, 10.0, 10.0, 0.0)]);
        assert_eq!(presses(&s.take_events(0.01)), 0);
        assert_eq!(presses(&s.take_events(0.1)), 1);
    }

    #[test]
    fn a_moving_finger_presses_at_once_where_it_started() {
        let mut s = InputState::new(StylusFeed::default());
        s.touches(&[t(1, 0, 0, 10.0, 10.0, 0.0), t(1, 1, 0, 40.0, 10.0, 0.01)]);
        let ev = s.take_events(0.01);
        assert_eq!(presses(&ev), 1);
        assert!(ev.iter().any(|e| matches!(e, Event::PointerButton { pos, pressed: true, .. } if *pos == Pos2::new(10.0, 10.0))));
    }

    #[test]
    fn two_fingers_in_time_never_press_and_a_quick_tap_is_undo() {
        let mut s = InputState::new(StylusFeed::default());
        s.touches(&[t(1, 0, 0, 10.0, 10.0, 0.0), t(2, 0, 0, 60.0, 10.0, 0.02), t(1, 2, 0, 10.0, 10.0, 0.1), t(2, 2, 0, 60.0, 10.0, 0.1)]);
        let ev = s.take_events(0.2);
        assert_eq!(presses(&ev), 0);
        assert!(ev.iter().any(|e| matches!(e, Event::Key { key: egui::Key::Z, pressed: true, modifiers, .. } if modifiers.command && !modifiers.shift)));
    }

    #[test]
    fn three_finger_tap_is_redo() {
        let mut s = InputState::new(StylusFeed::default());
        s.touches(&[t(1, 0, 0, 0.0, 0.0, 0.0), t(2, 0, 0, 30.0, 0.0, 0.01), t(3, 0, 0, 60.0, 0.0, 0.02)]);
        s.touches(&[t(1, 2, 0, 0.0, 0.0, 0.1), t(2, 2, 0, 30.0, 0.0, 0.1), t(3, 2, 0, 60.0, 0.0, 0.1)]);
        let ev = s.take_events(0.2);
        assert!(ev.iter().any(|e| matches!(e, Event::Key { key: egui::Key::Z, pressed: true, modifiers, .. } if modifiers.command && modifiers.shift)));
    }

    #[test]
    fn two_finger_drag_pans_and_is_no_tap() {
        let mut s = InputState::new(StylusFeed::default());
        s.touches(&[t(1, 0, 0, 0.0, 0.0, 0.0), t(2, 0, 0, 40.0, 0.0, 0.01)]);
        s.touches(&[t(1, 1, 0, 0.0, 50.0, 0.05), t(2, 1, 0, 40.0, 50.0, 0.05)]);
        s.touches(&[t(1, 2, 0, 0.0, 50.0, 0.1), t(2, 2, 0, 40.0, 50.0, 0.1)]);
        let ev = s.take_events(0.2);
        let pan: f32 = ev.iter().filter_map(|e| if let Event::MouseWheel { delta, .. } = e { Some(delta.y) } else { None }).sum();
        assert!((pan - 50.0).abs() < 0.01, "{pan}");
        assert!(!ev.iter().any(|e| matches!(e, Event::Key { .. })));
    }

    #[test]
    fn pencil_presses_at_once_with_its_pressure() {
        let feed = StylusFeed::default();
        let mut s = InputState::new(feed.clone());
        s.touches(&[t(9, 0, 1, 5.0, 5.0, 0.0)]);
        let ev = s.take_events(0.0);
        assert_eq!(presses(&ev), 1);
        assert_eq!(feed.get().map(|p| p.pressure), Some(0.5));
        s.touches(&[t(9, 2, 1, 6.0, 6.0, 0.05)]);
        assert_eq!(feed.get(), None);
    }

    #[test]
    fn upright_pencil_has_no_tilt_and_a_flat_one_tilts_toward_its_azimuth() {
        assert_eq!(pen_tilt(std::f32::consts::FRAC_PI_2, 1.0), (0.0, 0.0));
        let (x, y) = pen_tilt(0.3, 0.0);
        assert!(x > 60.0 && y.abs() < 1e-3, "{x} {y}");
    }
}
