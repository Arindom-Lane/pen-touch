//! Raw pen events in, device commands out. No hardware in here: the output side is the `Sink`
//! trait, so the whole chain is testable with a fake.

use crate::gesture::{Btn, Geom, Gestures, Out, Params};
use std::io;

// Linux input ABI numbers (linux/input-event-codes.h).
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const BTN_TOUCH: u16 = 0x14a;
pub const BTN_STYLUS: u16 = 0x14b;
pub const BTN_STYLUS2: u16 = 0x14c;
/// BTN_TOOL_PEN, RUBBER, BRUSH, PENCIL, AIRBRUSH: "some pen tool is in range".
pub const TOOL_CODES: [u16; 5] = [0x140, 0x141, 0x142, 0x143, 0x144];
/// Range of the virtual touch screen and pointer axes.
pub const OUT_MAX: i32 = 32767;

/// One raw event from the physical pen, reduced to what we use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Raw {
    Abs(u16, i32),
    Key(u16, i32),
    /// SYN_REPORT: the events so far form one complete report.
    Syn,
}

/// The virtual devices.
pub trait Sink {
    fn touch_down(&mut self, x: i32, y: i32) -> io::Result<()>;
    fn touch_move(&mut self, x: i32, y: i32) -> io::Result<()>;
    fn touch_up(&mut self) -> io::Result<()>;
    fn pointer_to(&mut self, x: i32, y: i32) -> io::Result<()>;
    fn button(&mut self, b: Btn, down: bool) -> io::Result<()>;
    /// Wheel in high-resolution units (120 = one notch). The sink carries any remainder.
    fn wheel(&mut self, hx: f64, hy: f64) -> io::Result<()>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub tolerance_mm: f64,
    pub rotation: u16,
    /// Cursor follows the pen while hovering (and while touch-scrolling).
    pub show_cursor: bool,
    pub cursor_area: [f64; 4],
    /// Scroll with the mouse wheel instead of a native touch contact.
    pub scroll_wheel: bool,
    pub scroll_speed: f64,
}

impl Settings {
    pub fn from_config(c: &crate::config::Config) -> Settings {
        Settings {
            tolerance_mm: c.tap_tolerance_mm,
            rotation: c.rotation,
            show_cursor: c.show_cursor,
            cursor_area: c.cursor_area,
            scroll_wheel: c.scroll_method == crate::config::ScrollMethod::Wheel,
            scroll_speed: c.scroll_speed,
        }
    }
}

/// Size and resolution of the physical tablet.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub x_min: i32,
    pub x_max: i32,
    pub y_min: i32,
    pub y_max: i32,
    pub upm_x: f64,
    pub upm_y: f64,
}

impl Geometry {
    /// `res_*` is the kernel's units-per-mm (0 = unknown; then assume a 150 x 100 mm active area).
    pub fn new(x_min: i32, x_max: i32, y_min: i32, y_max: i32, res_x: i32, res_y: i32) -> Geometry {
        let xr = (x_max - x_min).max(1) as f64;
        let yr = (y_max - y_min).max(1) as f64;
        Geometry {
            x_min,
            x_max,
            y_min,
            y_max,
            upm_x: if res_x > 0 { res_x as f64 } else { xr / 150.0 },
            upm_y: if res_y > 0 { res_y as f64 } else { yr / 100.0 },
        }
    }
}

/// Rotate a point in the unit square: "the tablet is turned `rot` degrees clockwise".
pub fn rotate(fx: f64, fy: f64, rot: u16) -> (f64, f64) {
    match rot {
        90 => (1.0 - fy, fx),
        180 => (1.0 - fx, 1.0 - fy),
        270 => (fy, 1.0 - fx),
        _ => (fx, fy),
    }
}

/// Turns fractional high-resolution wheel movement into the whole numbers the kernel wants,
/// carrying remainders so slow, small movements are never lost.
#[derive(Default)]
pub struct WheelAcc {
    frac: [f64; 2],
    notch: [i32; 2],
}

/// Whole-number wheel events: `hires_*` (120 per notch) and `ticks_*` (whole notches); h = horizontal, v = vertical.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct WheelEv {
    pub hires_h: i32,
    pub ticks_h: i32,
    pub hires_v: i32,
    pub ticks_v: i32,
}

impl WheelAcc {
    pub fn push(&mut self, hx: f64, hy: f64) -> WheelEv {
        let mut ev = WheelEv::default();
        for (i, amount) in [hx, hy].into_iter().enumerate() {
            self.frac[i] += amount;
            let n = self.frac[i] as i32; // truncates toward zero: symmetric for +/-
            if n == 0 {
                continue;
            }
            self.frac[i] -= n as f64;
            self.notch[i] += n;
            let ticks = self.notch[i] / 120;
            self.notch[i] -= ticks * 120;
            if i == 0 {
                ev.hires_h = n;
                ev.ticks_h = ticks;
            } else {
                ev.hires_v = n;
                ev.ticks_v = ticks;
            }
        }
        ev
    }
}

pub struct Pipeline<S: Sink> {
    sink: S,
    gest: Gestures,
    geom: Geometry,
    settings: Settings,
    x: i32,
    y: i32,
    touch: bool,
    tool: Option<u16>,
    outs: Vec<Out>,
}

impl<S: Sink> Pipeline<S> {
    pub fn new(sink: S, geom: Geometry, settings: Settings) -> Pipeline<S> {
        let params = Params {
            slop_mm: settings.tolerance_mm,
            touch_scroll: !settings.scroll_wheel,
            follow_hover: settings.show_cursor,
            wheel_speed: settings.scroll_speed,
            ..Params::default()
        };
        let g = Geom {
            upm_x: geom.upm_x,
            upm_y: geom.upm_y,
            x_range: (geom.x_max - geom.x_min).max(1) as f64,
            y_range: (geom.y_max - geom.y_min).max(1) as f64,
        };
        Pipeline { sink, gest: Gestures::new(params, g), geom, settings, x: geom.x_min, y: geom.y_min, touch: false, tool: None, outs: Vec::new() }
    }

    #[cfg(test)]
    pub fn sink(&self) -> &S {
        &self.sink
    }

    /// Normalised (0..1, after rotation) position for a tablet point.
    fn unit(&self, x: f64, y: f64) -> (f64, f64) {
        let xr = (self.geom.x_max - self.geom.x_min).max(1) as f64;
        let yr = (self.geom.y_max - self.geom.y_min).max(1) as f64;
        let fx = ((x - self.geom.x_min as f64) / xr).clamp(0.0, 1.0);
        let fy = ((y - self.geom.y_min as f64) / yr).clamp(0.0, 1.0);
        rotate(fx, fy, self.settings.rotation)
    }

    fn touch_xy(&self, x: f64, y: f64) -> (i32, i32) {
        let (fx, fy) = self.unit(x, y);
        ((fx * OUT_MAX as f64).round() as i32, (fy * OUT_MAX as f64).round() as i32)
    }

    fn cursor_xy(&self, x: f64, y: f64) -> (i32, i32) {
        let (fx, fy) = self.unit(x, y);
        let [x0, y0, x1, y1] = self.settings.cursor_area;
        let cx = x0 + fx * (x1 - x0);
        let cy = y0 + fy * (y1 - y0);
        ((cx * OUT_MAX as f64).round() as i32, (cy * OUT_MAX as f64).round() as i32)
    }

    pub fn feed(&mut self, ev: Raw, now: f64) -> io::Result<()> {
        match ev {
            Raw::Abs(ABS_X, v) => self.x = v,
            Raw::Abs(ABS_Y, v) => self.y = v,
            Raw::Key(BTN_TOUCH, v) => self.touch = v != 0,
            Raw::Key(BTN_STYLUS, v) => {
                self.gest.stylus(Btn::Right, v != 0, &mut self.outs);
                self.apply()?;
            }
            Raw::Key(BTN_STYLUS2, v) => {
                self.gest.stylus(Btn::Middle, v != 0, &mut self.outs);
                self.apply()?;
            }
            Raw::Key(c, v) if TOOL_CODES.contains(&c) => {
                if v != 0 {
                    self.tool = Some(c);
                } else if self.tool == Some(c) {
                    self.tool = None;
                }
            }
            Raw::Syn => {
                let in_range = self.tool.is_some();
                let contact = self.touch && in_range;
                self.gest.update(now, self.x as f64, self.y as f64, contact, in_range, &mut self.outs);
                self.apply()?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn tick(&mut self, now: f64) -> io::Result<()> {
        self.gest.tick(now, &mut self.outs);
        self.apply()
    }

    pub fn next_deadline(&self) -> Option<f64> {
        self.gest.next_deadline()
    }

    /// Release every finger and button that is still down.
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.gest.reset(&mut self.outs);
        self.apply()
    }

    fn apply(&mut self) -> io::Result<()> {
        let outs = std::mem::take(&mut self.outs);
        let mut result = Ok(());
        for o in &outs {
            let r = match *o {
                Out::TouchDown(x, y) => {
                    let (ix, iy) = self.touch_xy(x, y);
                    self.sink.touch_down(ix, iy)
                }
                Out::TouchMove(x, y) => {
                    let (ix, iy) = self.touch_xy(x, y);
                    self.sink.touch_move(ix, iy)
                }
                Out::TouchUp => self.sink.touch_up(),
                Out::Pointer(x, y) => {
                    let (ix, iy) = self.cursor_xy(x, y);
                    self.sink.pointer_to(ix, iy)
                }
                Out::Button(b, down) => self.sink.button(b, down),
                Out::Wheel(hx, hy) => self.sink.wheel(hx, hy),
            };
            if r.is_err() && result.is_ok() {
                result = r;
            }
        }
        self.outs = outs;
        self.outs.clear();
        result
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Clone, Copy)]
    pub enum Cmd {
        Down(i32, i32),
        Move(i32, i32),
        Up,
        Ptr(i32, i32),
        Btn(Btn, bool),
        Wheel(f64, f64),
    }

    #[derive(Default)]
    pub struct FakeSink {
        pub cmds: Vec<Cmd>,
    }
    impl Sink for FakeSink {
        fn touch_down(&mut self, x: i32, y: i32) -> io::Result<()> {
            self.cmds.push(Cmd::Down(x, y));
            Ok(())
        }
        fn touch_move(&mut self, x: i32, y: i32) -> io::Result<()> {
            self.cmds.push(Cmd::Move(x, y));
            Ok(())
        }
        fn touch_up(&mut self) -> io::Result<()> {
            self.cmds.push(Cmd::Up);
            Ok(())
        }
        fn pointer_to(&mut self, x: i32, y: i32) -> io::Result<()> {
            self.cmds.push(Cmd::Ptr(x, y));
            Ok(())
        }
        fn button(&mut self, b: Btn, down: bool) -> io::Result<()> {
            self.cmds.push(Cmd::Btn(b, down));
            Ok(())
        }
        fn wheel(&mut self, hx: f64, hy: f64) -> io::Result<()> {
            self.cmds.push(Cmd::Wheel(hx, hy));
            Ok(())
        }
    }

    pub fn geom() -> Geometry {
        Geometry::new(0, 15200, 0, 9500, 100, 100)
    }
    pub fn settings() -> Settings {
        Settings {
            tolerance_mm: 2.0,
            rotation: 0,
            show_cursor: true,
            cursor_area: [0.0, 0.0, 1.0, 1.0],
            scroll_wheel: false,
            scroll_speed: 20.0,
        }
    }
    pub const PEN: u16 = 0x140;
    pub const RUBBER: u16 = 0x141;

    pub fn report(p: &mut Pipeline<FakeSink>, now: f64, evs: &[Raw]) {
        for e in evs {
            p.feed(*e, now).unwrap();
        }
        p.feed(Raw::Syn, now).unwrap();
    }

    /// Advance the clock to `t`, firing every timer on the way (like the real event loop).
    fn run_to(p: &mut Pipeline<FakeSink>, t: f64) {
        while let Some(d) = p.next_deadline() {
            if d > t {
                break;
            }
            p.tick(d).unwrap();
        }
        p.tick(t).unwrap();
    }

    fn actions(p: &Pipeline<FakeSink>) -> Vec<Cmd> {
        p.sink().cmds.iter().copied().filter(|c| !matches!(c, Cmd::Ptr(..))).collect()
    }

    #[test]
    fn rotation_maps_corners_as_documented() {
        assert_eq!(rotate(0.0, 0.0, 0), (0.0, 0.0));
        assert_eq!(rotate(0.0, 0.0, 90), (1.0, 0.0));
        assert_eq!(rotate(0.0, 0.0, 180), (1.0, 1.0));
        assert_eq!(rotate(0.0, 0.0, 270), (0.0, 1.0));
        let (a, b) = rotate(0.3, 0.8, 90);
        let (c, d) = rotate(a, b, 270);
        assert!((c - 0.3).abs() < 1e-9 && (d - 0.8).abs() < 1e-9);
    }

    #[test]
    fn a_tap_is_a_left_click_at_the_scaled_position_with_no_touch_events() {
        let mut p = Pipeline::new(FakeSink::default(), geom(), settings());
        report(&mut p, 0.0, &[Raw::Key(PEN, 1), Raw::Abs(ABS_X, 7600), Raw::Abs(ABS_Y, 4750)]);
        report(&mut p, 0.01, &[Raw::Key(BTN_TOUCH, 1)]);
        report(&mut p, 0.08, &[Raw::Key(BTN_TOUCH, 0)]);
        run_to(&mut p, 0.5);
        assert_eq!(actions(&p), vec![Cmd::Btn(Btn::Left, true), Cmd::Btn(Btn::Left, false)]);
        let press = p.sink().cmds.iter().position(|c| *c == Cmd::Btn(Btn::Left, true)).unwrap();
        assert_eq!(p.sink().cmds[press - 1], Cmd::Ptr(16384, 16384));
    }

    #[test]
    fn a_drag_scrolls_by_touch_while_the_cursor_follows() {
        let mut p = Pipeline::new(FakeSink::default(), geom(), settings());
        report(&mut p, 0.0, &[Raw::Key(PEN, 1), Raw::Abs(ABS_X, 7600), Raw::Abs(ABS_Y, 8000), Raw::Key(BTN_TOUCH, 1)]);
        for i in 1..=30 {
            report(&mut p, 0.006 * i as f64, &[Raw::Abs(ABS_Y, 8000 - 60 * i)]);
        }
        report(&mut p, 0.2, &[Raw::Key(BTN_TOUCH, 0)]);
        run_to(&mut p, 0.9);
        let acts = actions(&p);
        assert!(matches!(acts[0], Cmd::Down(..)) && acts.last() == Some(&Cmd::Up));
        assert!(!acts.iter().any(|c| matches!(c, Cmd::Btn(..) | Cmd::Wheel(..))));
        let last_ptr = p.sink().cmds.iter().rev().find_map(|c| if let Cmd::Ptr(_, y) = c { Some(*y) } else { None }).unwrap();
        assert_eq!(last_ptr, (6200.0f64 / 9500.0 * OUT_MAX as f64).round() as i32, "cursor followed the pen to its final spot");
    }

    #[test]
    fn wheel_mode_sends_wheel_events_and_no_touch() {
        let mut s = settings();
        s.scroll_wheel = true;
        let mut p = Pipeline::new(FakeSink::default(), geom(), s);
        report(&mut p, 0.0, &[Raw::Key(PEN, 1), Raw::Abs(ABS_X, 7600), Raw::Abs(ABS_Y, 8000), Raw::Key(BTN_TOUCH, 1)]);
        for i in 1..=30 {
            report(&mut p, 0.006 * i as f64, &[Raw::Abs(ABS_Y, 8000 - 60 * i)]);
        }
        report(&mut p, 0.2, &[Raw::Key(BTN_TOUCH, 0)]);
        run_to(&mut p, 1.5);
        let acts = actions(&p);
        assert!(acts.iter().all(|c| matches!(c, Cmd::Wheel(..))), "{acts:?}");
        let sum: f64 = acts.iter().map(|c| if let Cmd::Wheel(_, v) = c { *v } else { 0.0 }).sum();
        assert!(sum < 0.0);
    }

    #[test]
    fn side_buttons_are_right_and_middle_click() {
        let mut p = Pipeline::new(FakeSink::default(), geom(), settings());
        report(&mut p, 0.0, &[Raw::Key(PEN, 1), Raw::Abs(ABS_X, 100), Raw::Abs(ABS_Y, 100)]);
        report(&mut p, 0.01, &[Raw::Key(BTN_STYLUS, 1)]);
        report(&mut p, 0.02, &[Raw::Key(BTN_STYLUS, 0)]);
        report(&mut p, 0.03, &[Raw::Key(BTN_STYLUS2, 1)]);
        report(&mut p, 0.04, &[Raw::Key(BTN_STYLUS2, 0)]);
        assert_eq!(
            actions(&p),
            vec![Cmd::Btn(Btn::Right, true), Cmd::Btn(Btn::Right, false), Cmd::Btn(Btn::Middle, true), Cmd::Btn(Btn::Middle, false)]
        );
    }

    #[test]
    fn cursor_follows_hover_can_be_switched_off_and_clicks_still_land() {
        let mut s = settings();
        s.show_cursor = false;
        let mut p = Pipeline::new(FakeSink::default(), geom(), s);
        report(&mut p, 0.0, &[Raw::Key(PEN, 1), Raw::Abs(ABS_X, 100), Raw::Abs(ABS_Y, 100)]);
        assert!(p.sink().cmds.is_empty(), "no hover cursor");
        report(&mut p, 0.1, &[Raw::Abs(ABS_X, 5000), Raw::Abs(ABS_Y, 3000), Raw::Key(BTN_TOUCH, 1)]);
        report(&mut p, 0.2, &[Raw::Key(BTN_TOUCH, 0)]);
        run_to(&mut p, 0.9);
        let press = p.sink().cmds.iter().position(|c| *c == Cmd::Btn(Btn::Left, true)).unwrap();
        let want = ((5000.0f64 / 15200.0 * OUT_MAX as f64).round() as i32, (3000.0f64 / 9500.0 * OUT_MAX as f64).round() as i32);
        assert_eq!(p.sink().cmds[press - 1], Cmd::Ptr(want.0, want.1));
    }

    #[test]
    fn cursor_area_applies_to_the_pointer_and_rotation_to_everything() {
        let mut s = settings();
        s.cursor_area = [0.5, 0.0, 1.0, 1.0];
        let mut p = Pipeline::new(FakeSink::default(), geom(), s);
        report(&mut p, 0.0, &[Raw::Key(PEN, 1), Raw::Abs(ABS_X, 0), Raw::Abs(ABS_Y, 0)]);
        report(&mut p, 0.01, &[Raw::Abs(ABS_X, 15200)]);
        assert_eq!(p.sink().cmds[0], Cmd::Ptr((0.5 * OUT_MAX as f64).round() as i32, 0));
        assert_eq!(p.sink().cmds[1], Cmd::Ptr(OUT_MAX, 0));

        let mut s = settings();
        s.rotation = 180;
        let mut p = Pipeline::new(FakeSink::default(), geom(), s);
        report(&mut p, 0.0, &[Raw::Key(PEN, 1), Raw::Abs(ABS_X, 0), Raw::Abs(ABS_Y, 0)]);
        assert_eq!(p.sink().cmds[0], Cmd::Ptr(OUT_MAX, OUT_MAX));
    }

    #[test]
    fn no_click_while_the_pen_is_out_of_range() {
        let mut p = Pipeline::new(FakeSink::default(), geom(), settings());
        report(&mut p, 0.0, &[Raw::Key(BTN_TOUCH, 1), Raw::Abs(ABS_X, 5), Raw::Abs(ABS_Y, 5)]);
        run_to(&mut p, 0.9);
        assert!(p.sink().cmds.is_empty());
    }

    #[test]
    fn eraser_end_works_like_the_tip() {
        let mut p = Pipeline::new(FakeSink::default(), geom(), settings());
        report(&mut p, 0.0, &[Raw::Key(RUBBER, 1), Raw::Abs(ABS_X, 1000), Raw::Abs(ABS_Y, 1000), Raw::Key(BTN_TOUCH, 1)]);
        report(&mut p, 0.08, &[Raw::Key(BTN_TOUCH, 0), Raw::Key(RUBBER, 0)]);
        run_to(&mut p, 0.5);
        assert_eq!(actions(&p), vec![Cmd::Btn(Btn::Left, true), Cmd::Btn(Btn::Left, false)]);
    }

    #[test]
    fn shutdown_releases_a_held_mouse_button_and_is_idempotent() {
        let mut p = Pipeline::new(FakeSink::default(), geom(), settings());
        report(&mut p, 0.0, &[Raw::Key(PEN, 1), Raw::Abs(ABS_X, 500), Raw::Abs(ABS_Y, 500), Raw::Key(BTN_TOUCH, 1)]);
        run_to(&mut p, 0.45);
        report(&mut p, 0.5, &[Raw::Abs(ABS_X, 900)]); // hold, then move: mouse drag
        assert!(p.sink().cmds.contains(&Cmd::Btn(Btn::Left, true)));
        p.shutdown().unwrap();
        assert_eq!(actions(&p).last(), Some(&Cmd::Btn(Btn::Left, false)));
        p.shutdown().unwrap();
        assert_eq!(p.sink().cmds.iter().filter(|c| **c == Cmd::Btn(Btn::Left, false)).count(), 1);
    }

    #[test]
    fn unknown_resolution_falls_back_to_a_sensible_size() {
        let g = Geometry::new(0, 15000, 0, 10000, 0, 0);
        assert_eq!((g.upm_x, g.upm_y), (100.0, 100.0));
    }

    #[test]
    fn wheel_accumulator_never_loses_small_motion_and_is_symmetric() {
        let mut a = WheelAcc::default();
        let mut hires = 0;
        let mut ticks = 0;
        for _ in 0..1000 {
            let e = a.push(0.0, -0.25); // a quarter of a hi-res unit per report
            hires += e.hires_v;
            ticks += e.ticks_v;
        }
        assert_eq!(hires, -250);
        assert_eq!(ticks, -2, "250 hi-res units = 2 whole notches (-240) with 10 left over");
        let mut b = WheelAcc::default();
        let pos: i32 = (0..1000).map(|_| b.push(0.0, 0.25).hires_v).sum();
        assert_eq!(pos, 250, "same magnitude in the other direction");
        let e = WheelAcc::default().push(130.0, 0.0);
        assert_eq!((e.hires_h, e.ticks_h), (130, 1));
    }
}
