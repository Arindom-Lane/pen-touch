//! The gesture brain. Raw pen reports in, high-level commands out.
//!
//! Every gesture is decided BEFORE anything is sent, so apps never see a half-started touch:
//!
//! | what you do                          | what the apps get                                        |
//! |--------------------------------------|----------------------------------------------------------|
//! | tap                                  | a left click                                             |
//! | tap-tap / tap-tap-tap                | double / triple click (select word / line, open file)    |
//! | drag straight away                   | scroll (native touch scroll, or mouse wheel)             |
//! | hold, then drag                      | a REAL mouse drag: select text, rubber-band files, drag & drop |
//! | hold still                           | right click                                              |
//! | pen side buttons                     | right / middle click                                     |
//!
//! Hold-then-drag uses the mouse, not touch, on purpose: toolkits like Qt Widgets (Dolphin) turn
//! a touch hold into a fake mouse press and then misread the cursor's own movement as "button
//! released", which left a 1x1 selection stuck. A genuine mouse button has no such problem.
//! When you let go, the next drag scrolls again.
//!
//! Everything is pure logic driven by explicit timestamps, so it is fully unit-tested.

/// Interval of the small animations (touch catch-up, wheel smoothing, cursor easing).
pub const STEP: f64 = 1.0 / 120.0;

const AXIS_LOCK_RATIO: f64 = 1.8;
const AXIS_BREAKOUT_MM: f64 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Btn {
    Left,
    Right,
    Middle,
}

impl Btn {
    fn idx(self) -> usize {
        match self {
            Btn::Left => 0,
            Btn::Right => 1,
            Btn::Middle => 2,
        }
    }
}

/// Commands for the virtual devices. Positions are in tablet units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Out {
    TouchDown(f64, f64),
    TouchMove(f64, f64),
    TouchUp,
    /// Put the mouse cursor here.
    Pointer(f64, f64),
    Button(Btn, bool),
    /// Mouse wheel in high-resolution units (120 = one notch): (horizontal, vertical).
    Wheel(f64, f64),
}

#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// How far (mm) the pen may wobble before a touch counts as a drag.
    pub slop_mm: f64,
    /// Hold this long, then drag = mouse drag (select).
    pub hold_select: f64,
    /// Hold still this long = right click.
    pub long_press: f64,
    /// Contacts shorter than this are noise, not taps.
    pub min_tap: f64,
    /// A lift shorter than this is pressure flicker and is bridged.
    pub lift_debounce: f64,
    /// A second tap within this time and `double_tap_mm` snaps onto the first (double/triple click).
    pub double_tap_time: f64,
    pub double_tap_mm: f64,
    /// Smooth hand-over from the tap circle to the pen when a touch scroll starts.
    pub catchup_tau: f64,
    /// How long a synthetic click stays pressed.
    pub click_hold: f64,
    /// After the cursor was parked, it glides back to the pen with this time constant.
    pub ease_tau: f64,
    /// Wheel mode: the output eases toward the pen with this time constant (0 = raw).
    pub wheel_smoothing: f64,
    /// true = scroll with a native touch contact; false = mouse wheel.
    pub touch_scroll: bool,
    /// Cursor follows the pen while hovering (and while touch-scrolling).
    pub follow_hover: bool,
    /// Wheel mode: notches for a drag across the full tablet height.
    pub wheel_speed: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            slop_mm: 2.0,
            hold_select: 0.35,
            long_press: 0.55,
            min_tap: 0.025,
            lift_debounce: 0.03,
            double_tap_time: 0.45,
            double_tap_mm: 3.0,
            catchup_tau: 0.040,
            click_hold: 0.025,
            ease_tau: 0.060,
            wheel_smoothing: 0.030,
            touch_scroll: true,
            follow_hover: true,
            wheel_speed: 20.0,
        }
    }
}

/// Tablet size: units per millimetre and total range, per axis.
#[derive(Clone, Copy, Debug)]
pub struct Geom {
    pub upm_x: f64,
    pub upm_y: f64,
    pub x_range: f64,
    pub y_range: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Idle,
    /// Contact, not yet classified. Nothing has been sent except parking the cursor.
    Pending,
    /// Held still long enough that moving now means "select".
    Hold,
    Scroll,
    /// Mouse button held, cursor follows the pen.
    Drag,
    /// The right click already happened; ignore the rest of this contact.
    RightDone,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy)]
enum Timer {
    Hold,
    Long,
    Lift,
    ClickUp,
    RClickUp,
    Anim,
}

pub struct Gestures {
    p: Params,
    g: Geom,
    st: St,
    in_range: bool,
    pen: (f64, f64),
    t_contact: f64,
    anchor: (f64, f64),
    lift_t: Option<f64>,
    last_tap: Option<(f64, f64, f64)>,
    down: [bool; 3],
    click_lock: bool,
    // timers (absolute times)
    hold_at: Option<f64>,
    long_at: Option<f64>,
    click_up_at: Option<f64>,
    rclick_up_at: Option<f64>,
    anim_at: Option<f64>,
    // touch scroll
    touch_down: bool,
    off: (f64, f64),
    rep: (f64, f64),
    last_t: f64,
    // wheel scroll
    wl: (f64, f64),
    lock: Option<Axis>,
    pend: (f64, f64),
    pend_t: f64,
    // cursor
    pp: (f64, f64),
    ease: bool,
    ease_t: f64,
}

impl Gestures {
    pub fn new(p: Params, g: Geom) -> Gestures {
        Gestures {
            p,
            g,
            st: St::Idle,
            in_range: false,
            pen: (0.0, 0.0),
            t_contact: 0.0,
            anchor: (0.0, 0.0),
            lift_t: None,
            last_tap: None,
            down: [false; 3],
            click_lock: false,
            hold_at: None,
            long_at: None,
            click_up_at: None,
            rclick_up_at: None,
            anim_at: None,
            touch_down: false,
            off: (0.0, 0.0),
            rep: (0.0, 0.0),
            last_t: 0.0,
            wl: (0.0, 0.0),
            lock: None,
            pend: (0.0, 0.0),
            pend_t: 0.0,
            pp: (0.0, 0.0),
            ease: false,
            ease_t: 0.0,
        }
    }

    fn mm(&self, dx: f64, dy: f64) -> f64 {
        (dx / self.g.upm_x).hypot(dy / self.g.upm_y)
    }

    fn point(&mut self, p: (f64, f64), out: &mut Vec<Out>) {
        self.pp = p;
        out.push(Out::Pointer(p.0, p.1));
    }

    fn button(&mut self, b: Btn, down: bool, out: &mut Vec<Out>) {
        if self.down[b.idx()] != down {
            self.down[b.idx()] = down;
            out.push(Out::Button(b, down));
        }
    }

    fn arm_anim(&mut self, now: f64) {
        if self.anim_at.is_none() {
            self.anim_at = Some(now + STEP);
        }
    }

    // ------------------------------------------------------------------ public API

    /// One pen report. `contact` = tip touching; `in_range` = pen is near the tablet.
    pub fn update(&mut self, now: f64, x: f64, y: f64, contact: bool, in_range: bool, out: &mut Vec<Out>) {
        self.run_timers(now, out);
        self.pen = (x, y);
        self.in_range = in_range;
        if self.st == St::Idle {
            if contact && in_range {
                self.begin(now, out);
            } else {
                self.hover(now, out);
            }
            return;
        }
        if !contact {
            if self.lift_t.is_none() {
                self.lift_t = Some(now);
            }
            return;
        }
        self.lift_t = None; // contact came back: it was pressure flicker
        self.contact_frame(now, out);
    }

    /// A pen side button: lower = right click, upper = middle click.
    pub fn stylus(&mut self, btn: Btn, down: bool, out: &mut Vec<Out>) {
        if down && self.st == St::Idle && !self.click_lock {
            let p = self.pen;
            self.point(p, out);
        }
        self.button(btn, down, out);
    }

    /// Run timers without a pen report.
    pub fn tick(&mut self, now: f64, out: &mut Vec<Out>) {
        self.run_timers(now, out);
    }

    pub fn next_deadline(&self) -> Option<f64> {
        let lift = self.lift_t.map(|t| t + self.p.lift_debounce);
        [self.hold_at, self.long_at, self.click_up_at, self.rclick_up_at, self.anim_at, lift]
            .into_iter()
            .flatten()
            .fold(None, |a: Option<f64>, t| Some(a.map_or(t, |a| a.min(t))))
    }

    /// Release everything (switched off, tablet unplugged, shutting down).
    pub fn reset(&mut self, out: &mut Vec<Out>) {
        if self.touch_down {
            out.push(Out::TouchUp);
            self.touch_down = false;
        }
        for b in [Btn::Left, Btn::Right, Btn::Middle] {
            self.button(b, false, out);
        }
        self.st = St::Idle;
        self.lift_t = None;
        self.last_tap = None;
        self.click_lock = false;
        self.ease = false;
        self.pend = (0.0, 0.0);
        self.hold_at = None;
        self.long_at = None;
        self.click_up_at = None;
        self.rclick_up_at = None;
        self.anim_at = None;
    }

    // ------------------------------------------------------------------ timers

    fn due(&self, now: f64) -> Option<(f64, Timer)> {
        let lift = self.lift_t.map(|t| t + self.p.lift_debounce);
        let all = [
            (self.hold_at, Timer::Hold),
            (self.long_at, Timer::Long),
            (self.click_up_at, Timer::ClickUp),
            (self.rclick_up_at, Timer::RClickUp),
            (self.anim_at, Timer::Anim),
            (lift, Timer::Lift),
        ];
        all.into_iter()
            .filter_map(|(t, k)| t.filter(|t| *t <= now).map(|t| (t, k)))
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
    }

    fn run_timers(&mut self, now: f64, out: &mut Vec<Out>) {
        for _ in 0..200 {
            let Some((_, which)) = self.due(now) else { return };
            match which {
                Timer::Hold => {
                    self.hold_at = None;
                    self.on_hold();
                }
                Timer::Long => {
                    self.long_at = None;
                    self.on_long(now, out);
                }
                Timer::ClickUp => {
                    self.click_up_at = None;
                    self.click_lock = false;
                    self.button(Btn::Left, false, out);
                }
                Timer::RClickUp => {
                    self.rclick_up_at = None;
                    self.button(Btn::Right, false, out);
                }
                Timer::Anim => {
                    self.anim_at = None;
                    self.animate(now, out);
                }
                Timer::Lift => self.commit_lift(now, out),
            }
        }
    }

    fn on_hold(&mut self) {
        if self.st != St::Pending {
            return;
        }
        if let Some(lt) = self.lift_t {
            self.hold_at = Some(lt + self.p.lift_debounce + 1e-3); // maybe flicker: look again
            return;
        }
        self.st = St::Hold;
    }

    fn on_long(&mut self, now: f64, out: &mut Vec<Out>) {
        if !matches!(self.st, St::Pending | St::Hold) {
            return;
        }
        if let Some(lt) = self.lift_t {
            self.long_at = Some(lt + self.p.lift_debounce + 1e-3);
            return;
        }
        self.st = St::RightDone;
        self.hold_at = None;
        self.button(Btn::Right, true, out);
        self.rclick_up_at = Some(now + self.p.click_hold);
    }

    // ------------------------------------------------------------------ gesture flow

    fn begin(&mut self, now: f64, out: &mut Vec<Out>) {
        self.st = St::Pending;
        self.t_contact = now;
        self.lift_t = None;
        self.ease = false;
        self.touch_down = false;
        let mut a = self.pen;
        if let Some((t, x, y)) = self.last_tap {
            if now - t <= self.p.double_tap_time && self.mm(a.0 - x, a.1 - y) <= self.p.double_tap_mm {
                a = (x, y); // a quick repeat tap lands exactly on the first: that is a double / triple click
            }
        }
        self.anchor = a;
        self.hold_at = Some(now + self.p.hold_select);
        self.long_at = Some(now + self.p.long_press);
        self.point(a, out); // park the cursor exactly where a click would land
    }

    fn moved_beyond_slop(&self) -> bool {
        self.mm(self.pen.0 - self.anchor.0, self.pen.1 - self.anchor.1) > self.p.slop_mm
    }

    fn contact_frame(&mut self, now: f64, out: &mut Vec<Out>) {
        match self.st {
            St::Pending => {
                if self.moved_beyond_slop() {
                    if now < self.t_contact + self.p.hold_select {
                        self.start_scroll(now, out);
                    } else {
                        self.start_drag(out);
                    }
                }
            }
            St::Hold => {
                if self.moved_beyond_slop() {
                    self.start_drag(out);
                }
            }
            St::Scroll => self.scroll_frame(now, out),
            St::Drag => {
                let p = self.pen;
                self.point(p, out);
            }
            St::RightDone | St::Idle => {}
        }
    }

    fn start_drag(&mut self, out: &mut Vec<Out>) {
        self.st = St::Drag;
        self.hold_at = None;
        self.long_at = None;
        let a = self.anchor;
        self.point(a, out);
        self.button(Btn::Left, true, out); // press where the hold began ...
        let p = self.pen;
        self.point(p, out); // ... and the cursor follows the pen from here
    }

    fn start_scroll(&mut self, now: f64, out: &mut Vec<Out>) {
        self.st = St::Scroll;
        self.hold_at = None;
        self.long_at = None;
        if self.p.touch_scroll {
            self.touch_down = true;
            out.push(Out::TouchDown(self.anchor.0, self.anchor.1));
            // start the contact where the tap began and glide it to the pen: no lurch
            self.off = (self.anchor.0 - self.pen.0, self.anchor.1 - self.pen.1);
            self.rep = self.anchor;
            self.last_t = now;
            self.arm_anim(now);
        } else {
            self.wl = self.pen;
            self.pend = (0.0, 0.0);
            self.lock = None;
            let ddx = (self.pen.0 - self.anchor.0).abs() / self.g.upm_x;
            let ddy = (self.pen.1 - self.anchor.1).abs() / self.g.upm_y;
            if ddy >= ddx * AXIS_LOCK_RATIO {
                self.lock = Some(Axis::Vertical);
            } else if ddx >= ddy * AXIS_LOCK_RATIO {
                self.lock = Some(Axis::Horizontal);
            }
        }
    }

    fn scroll_frame(&mut self, now: f64, out: &mut Vec<Out>) {
        if self.p.touch_scroll {
            self.touch_follow(now, out);
            if self.p.follow_hover {
                let p = self.pen;
                self.point(p, out);
            }
            return;
        }
        // wheel mode: the cursor stays parked so every app scrolls what was under the pen
        let (mut dx, mut dy) = (self.pen.0 - self.wl.0, self.pen.1 - self.wl.1);
        self.wl = self.pen;
        match self.lock {
            Some(Axis::Vertical) => {
                if (self.pen.0 - self.anchor.0).abs() / self.g.upm_x >= AXIS_BREAKOUT_MM {
                    self.lock = None;
                } else {
                    dx = 0.0;
                }
            }
            Some(Axis::Horizontal) => {
                if (self.pen.1 - self.anchor.1).abs() / self.g.upm_y >= AXIS_BREAKOUT_MM {
                    self.lock = None;
                } else {
                    dy = 0.0;
                }
            }
            None => {}
        }
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        // natural scrolling: the content follows the pen
        let kx = 120.0 * self.p.wheel_speed / self.g.x_range;
        let ky = 120.0 * self.p.wheel_speed / self.g.y_range;
        let (hx, hy) = (-dx * kx, dy * ky);
        if self.p.wheel_smoothing <= 0.0 {
            out.push(Out::Wheel(hx, hy));
        } else {
            if self.pend == (0.0, 0.0) {
                self.pend_t = now;
            }
            self.pend.0 += hx;
            self.pend.1 += hy;
            self.arm_anim(now);
        }
    }

    fn touch_follow(&mut self, now: f64, out: &mut Vec<Out>) {
        let dt = (now - self.last_t).clamp(0.0, 0.05);
        self.last_t = now;
        let k = (-dt / self.p.catchup_tau).exp();
        self.off = (self.off.0 * k, self.off.1 * k);
        let pos = (self.pen.0 + self.off.0, self.pen.1 + self.off.1);
        if pos != self.rep {
            self.rep = pos;
            out.push(Out::TouchMove(pos.0, pos.1));
        }
    }

    fn flush_wheel(&mut self, out: &mut Vec<Out>) {
        if self.pend != (0.0, 0.0) {
            out.push(Out::Wheel(self.pend.0, self.pend.1));
            self.pend = (0.0, 0.0);
        }
    }

    fn animate(&mut self, now: f64, out: &mut Vec<Out>) {
        let mut again = false;
        if self.st == St::Scroll && self.touch_down {
            self.touch_follow(now, out);
            again |= self.off.0.abs() + self.off.1.abs() > 0.5;
        }
        if self.pend != (0.0, 0.0) {
            let dt = (now - self.pend_t).clamp(0.0, 0.05);
            self.pend_t = now;
            let a = 1.0 - (-dt / self.p.wheel_smoothing).exp();
            let (mut ox, mut oy) = (self.pend.0 * a, self.pend.1 * a);
            self.pend.0 -= ox;
            self.pend.1 -= oy;
            if self.pend.0.hypot(self.pend.1) < 0.5 {
                ox += self.pend.0;
                oy += self.pend.1;
                self.pend = (0.0, 0.0);
            } else {
                again = true;
            }
            out.push(Out::Wheel(ox, oy));
        }
        if self.ease {
            if self.st == St::Idle && self.in_range && !self.click_lock && self.p.follow_hover {
                self.ease_step(now, out);
                again |= self.ease;
            } else {
                self.ease = false;
            }
        }
        if again {
            self.anim_at = Some(now + STEP);
        }
    }

    // ------------------------------------------------------------------ cursor

    fn hover(&mut self, now: f64, out: &mut Vec<Out>) {
        if !self.in_range {
            self.ease = false;
            return;
        }
        if self.click_lock || !self.p.follow_hover {
            return;
        }
        if self.ease {
            self.ease_step(now, out);
        } else {
            let p = self.pen;
            self.point(p, out);
        }
    }

    fn start_ease(&mut self, now: f64) {
        if self.p.follow_hover && self.p.ease_tau > 0.0 {
            self.ease = true;
            self.ease_t = now;
            self.arm_anim(now);
        }
    }

    /// The cursor was parked away from the pen: glide it back instead of jumping.
    fn ease_step(&mut self, now: f64, out: &mut Vec<Out>) {
        let dt = (now - self.ease_t).clamp(0.0, 0.05);
        self.ease_t = now;
        let a = 1.0 - (-dt / self.p.ease_tau).exp();
        let (px, py) = self.pp;
        let np = (px + (self.pen.0 - px) * a, py + (self.pen.1 - py) * a);
        if self.mm(self.pen.0 - np.0, self.pen.1 - np.1) < 0.25 {
            self.ease = false;
            let p = self.pen;
            self.point(p, out);
        } else {
            self.point(np, out);
            self.arm_anim(now);
        }
    }

    // ------------------------------------------------------------------ the end of a contact

    fn commit_lift(&mut self, now: f64, out: &mut Vec<Out>) {
        let lift = self.lift_t.take().unwrap_or(now);
        let st = self.st;
        self.st = St::Idle;
        self.hold_at = None;
        self.long_at = None;
        match st {
            St::Pending | St::Hold => {
                if lift - self.t_contact >= self.p.min_tap {
                    self.click(now, out);
                }
            }
            St::Scroll => {
                if self.touch_down {
                    out.push(Out::TouchUp);
                    self.touch_down = false;
                }
                self.flush_wheel(out);
                self.last_tap = None;
                if !self.p.touch_scroll {
                    self.start_ease(now); // wheel mode parked the cursor
                }
            }
            St::Drag => {
                self.button(Btn::Left, false, out);
                self.last_tap = None;
            }
            St::RightDone => {
                self.last_tap = None;
                self.start_ease(now);
            }
            St::Idle => {}
        }
    }

    fn click(&mut self, now: f64, out: &mut Vec<Out>) {
        let a = self.anchor;
        self.point(a, out);
        self.button(Btn::Left, true, out);
        self.click_lock = true;
        self.click_up_at = Some(now + self.p.click_hold);
        self.last_tap = Some((now, a.0, a.1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Intuos-S-like: 100 units per mm, 152 x 95 mm.
    fn geom() -> Geom {
        Geom { upm_x: 100.0, upm_y: 100.0, x_range: 15200.0, y_range: 9500.0 }
    }

    pub struct Rig {
        pub g: Gestures,
        pub out: Vec<Out>,
        pub t: f64,
    }
    impl Rig {
        pub fn with(p: Params) -> Rig {
            Rig { g: Gestures::new(p, geom()), out: vec![], t: 100.0 }
        }
        pub fn new() -> Rig {
            Rig::with(Params::default())
        }
        pub fn run_to(&mut self, t: f64) {
            let mut guard = 0;
            while let Some(d) = self.g.next_deadline() {
                if d > t {
                    break;
                }
                guard += 1;
                assert!(guard < 20_000, "timer loop does not converge at d={d} t={t}");
                self.t = self.t.max(d);
                let now = self.t;
                self.g.tick(now, &mut self.out);
            }
            self.t = t;
            let now = self.t;
            self.g.tick(now, &mut self.out);
        }
        pub fn pen(&mut self, dt: f64, x: f64, y: f64, contact: bool) {
            self.run_to(self.t + dt);
            let now = self.t;
            self.g.update(now, x, y, contact, true, &mut self.out);
        }
        pub fn take(&mut self) -> Vec<Out> {
            std::mem::take(&mut self.out)
        }
        /// Everything except cursor moves.
        pub fn take_actions(&mut self) -> Vec<Out> {
            self.take().into_iter().filter(|o| !matches!(o, Out::Pointer(..))).collect()
        }
    }

    fn ptr_positions(outs: &[Out]) -> Vec<(f64, f64)> {
        outs.iter().filter_map(|o| if let Out::Pointer(x, y) = o { Some((*x, *y)) } else { None }).collect()
    }

    fn tap(r: &mut Rig, gap: f64, x: f64, y: f64) {
        r.pen(gap, x, y, true);
        r.pen(0.06, x, y, true);
        r.pen(0.01, x, y, false);
    }

    #[test]
    fn a_tap_is_one_left_click_at_the_touch_point_and_sends_no_touch() {
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 3000.0, false); // hovering
        r.take();
        tap(&mut r, 0.1, 5003.0, 3000.0);
        r.run_to(r.t + 0.2);
        let outs = r.take();
        let acts: Vec<Out> = outs.iter().copied().filter(|o| !matches!(o, Out::Pointer(..))).collect();
        assert_eq!(acts, vec![Out::Button(Btn::Left, true), Out::Button(Btn::Left, false)]);
        // the cursor was exactly at the tap point when the button went down
        let down_idx = outs.iter().position(|o| *o == Out::Button(Btn::Left, true)).unwrap();
        assert_eq!(outs[down_idx - 1], Out::Pointer(5003.0, 3000.0));
    }

    #[test]
    fn a_blip_never_clicks() {
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 3000.0, true);
        r.pen(0.008, 5000.0, 3000.0, false);
        r.run_to(r.t + 0.5);
        assert_eq!(r.take_actions(), vec![]);
    }

    #[test]
    fn hand_tremor_inside_the_tolerance_is_still_a_tap() {
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 3000.0, true);
        for i in 0..12 {
            let j = if i % 2 == 0 { 120.0 } else { -120.0 }; // +/- 1.2 mm for ~70 ms
            r.pen(0.006, 5000.0 + j, 3000.0 - j, true);
        }
        r.pen(0.01, 5000.0, 3000.0, false);
        r.run_to(r.t + 0.2);
        assert_eq!(r.take_actions(), vec![Out::Button(Btn::Left, true), Out::Button(Btn::Left, false)]);
    }

    #[test]
    fn press_of_0_4_s_without_moving_is_a_click_not_a_right_click() {
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 3000.0, true);
        r.pen(0.40, 5000.0, 3000.0, true);
        r.pen(0.01, 5000.0, 3000.0, false);
        r.run_to(r.t + 0.2);
        assert_eq!(r.take_actions(), vec![Out::Button(Btn::Left, true), Out::Button(Btn::Left, false)]);
    }

    // ------------------------------------------------------------ double / triple click

    #[test]
    fn quick_repeat_taps_snap_together_for_double_and_triple_click() {
        let mut r = Rig::new();
        tap(&mut r, 0.0, 5000.0, 3000.0);
        tap(&mut r, 0.10, 5060.0, 3040.0); // 0.7 mm away
        tap(&mut r, 0.10, 4950.0, 2980.0);
        r.run_to(r.t + 0.3);
        let outs = r.take();
        let presses: Vec<usize> = outs.iter().enumerate().filter(|(_, o)| **o == Out::Button(Btn::Left, true)).map(|(i, _)| i).collect();
        assert_eq!(presses.len(), 3, "three clicks");
        for i in presses {
            assert_eq!(outs[i - 1], Out::Pointer(5000.0, 3000.0), "every click lands on the first tap");
        }
    }

    #[test]
    fn a_far_or_slow_second_tap_is_a_separate_click() {
        for (dx, gap) in [(600.0, 0.10), (60.0, 0.9)] {
            let mut r = Rig::new();
            tap(&mut r, 0.0, 5000.0, 3000.0);
            tap(&mut r, gap, 5000.0 + dx, 3000.0);
            r.run_to(r.t + 0.3);
            let outs = r.take();
            let second = outs.iter().enumerate().filter(|(_, o)| **o == Out::Button(Btn::Left, true)).nth(1).unwrap().0;
            assert_eq!(outs[second - 1], Out::Pointer(5000.0 + dx, 3000.0), "dx={dx} gap={gap}");
        }
    }

    // ------------------------------------------------------------ hold, then drag = real mouse drag

    #[test]
    fn hold_then_drag_is_a_real_mouse_drag_with_no_touch_at_all() {
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 3000.0, true);
        r.pen(0.40, 5000.0, 3000.0, true); // hold
        assert!(r.take_actions().is_empty(), "nothing pressed while merely holding");
        let mut x = 5000.0;
        for _ in 0..20 {
            x += 60.0;
            r.pen(0.006, x, 3000.0, true);
        }
        r.pen(0.01, x, 3000.0, false);
        r.run_to(r.t + 0.2);
        let outs = r.take();
        let acts: Vec<Out> = outs.iter().copied().filter(|o| !matches!(o, Out::Pointer(..))).collect();
        assert_eq!(acts, vec![Out::Button(Btn::Left, true), Out::Button(Btn::Left, false)], "{acts:?}");
        assert!(!outs.iter().any(|o| matches!(o, Out::TouchDown(..) | Out::TouchMove(..) | Out::TouchUp | Out::Wheel(..))));
        // pressed where the hold began, then the cursor followed the pen all the way
        let press = outs.iter().position(|o| *o == Out::Button(Btn::Left, true)).unwrap();
        assert_eq!(outs[press - 1], Out::Pointer(5000.0, 3000.0));
        let after: Vec<(f64, f64)> = ptr_positions(&outs[press..]);
        assert_eq!(*after.last().unwrap(), (x, 3000.0));
        assert!(after.windows(2).all(|w| w[1].0 >= w[0].0), "cursor moves monotonically with the pen");
    }

    #[test]
    fn after_a_hold_drag_the_next_drag_scrolls_again() {
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 3000.0, true);
        r.pen(0.40, 5000.0, 3000.0, true);
        for i in 1..=10 {
            r.pen(0.006, 5000.0 + 60.0 * i as f64, 3000.0, true);
        }
        r.pen(0.01, 5600.0, 3000.0, false);
        r.run_to(r.t + 0.2);
        r.take();
        r.pen(0.1, 5600.0, 6000.0, true);
        for i in 1..=10 {
            r.pen(0.006, 5600.0, 6000.0 - 60.0 * i as f64, true);
        }
        let acts = r.take_actions();
        assert!(acts.iter().any(|o| matches!(o, Out::TouchDown(..))), "scrolling is back on");
        assert!(!acts.iter().any(|o| matches!(o, Out::Button(..))));
    }

    #[test]
    fn holding_still_is_a_right_click_and_lifting_afterwards_does_not_click() {
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 3000.0, true);
        r.pen(0.70, 5000.0, 3000.0, true);
        assert_eq!(r.take_actions(), vec![Out::Button(Btn::Right, true), Out::Button(Btn::Right, false)]);
        r.pen(0.01, 5000.0, 3000.0, false);
        r.run_to(r.t + 0.3);
        assert_eq!(r.take_actions(), vec![], "no left click on release");
    }

    // ------------------------------------------------------------ touch scroll

    fn drag_up(r: &mut Rig, x: f64, y0: f64, n: usize, step: f64) -> f64 {
        let mut y = y0;
        for _ in 0..n {
            y -= step;
            r.pen(0.006, x, y, true);
        }
        y
    }

    #[test]
    fn dragging_straight_away_scrolls_by_touch_and_the_cursor_follows() {
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 8000.0, true);
        r.take();
        let y = drag_up(&mut r, 5000.0, 8000.0, 50, 60.0);
        let outs = r.take();
        assert_eq!(outs.iter().filter(|o| matches!(o, Out::TouchDown(..))).count(), 1);
        assert_eq!(outs.iter().position(|o| matches!(o, Out::TouchDown(..))), outs.iter().position(|o| matches!(o, Out::TouchDown(..) | Out::TouchMove(..))), "down comes first");
        let Some(Out::TouchDown(dx, dy)) = outs.iter().copied().find(|o| matches!(o, Out::TouchDown(..))) else { unreachable!() };
        assert_eq!((dx, dy), (5000.0, 8000.0), "contact starts where the pen first touched");
        // never a lurch: each move is about one pen step (60 units), not the 2 mm (200 unit) circle at once
        let ys: Vec<f64> = outs.iter().filter_map(|o| if let Out::TouchMove(_, y) = o { Some(*y) } else { None }).collect();
        let mut last = 8000.0;
        for v in &ys {
            assert!((v - last).abs() < 130.0, "step {} -> {}", last, v);
            assert!(*v <= last + 1e-9, "never backwards");
            last = *v;
        }
        // the cursor follows the pen during the scroll (this was the 'static cursor' complaint)
        assert_eq!(*ptr_positions(&outs).last().unwrap(), (5000.0, y));
        assert!(!outs.iter().any(|o| matches!(o, Out::Button(..) | Out::Wheel(..))));
        // lift: contact ends, nothing else
        r.pen(0.01, 5000.0, y, false);
        r.run_to(r.t + 0.5);
        let end = r.take_actions();
        assert_eq!(end.last(), Some(&Out::TouchUp));
        assert!(end.iter().all(|o| matches!(o, Out::TouchUp | Out::TouchMove(..))));
        let Some(Out::TouchMove(_, final_y)) = end.iter().rev().find(|o| matches!(o, Out::TouchMove(..))).copied().or(Some(Out::TouchMove(0.0, last))) else { unreachable!() };
        assert!((final_y - y).abs() < 2.0, "settled on the pen: {final_y} vs {y}");
    }

    #[test]
    fn pressure_flicker_mid_scroll_is_bridged() {
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 8000.0, true);
        let y = drag_up(&mut r, 5000.0, 8000.0, 10, 80.0);
        r.take();
        r.pen(0.006, 5000.0, y, false);
        r.pen(0.006, 5000.0, y, false); // 12 ms gap
        r.pen(0.006, 5000.0, y - 80.0, true);
        r.pen(0.006, 5000.0, y - 160.0, true);
        let acts = r.take_actions();
        assert!(!acts.contains(&Out::TouchUp) && !acts.iter().any(|o| matches!(o, Out::TouchDown(..))), "{acts:?}");
    }

    #[test]
    fn a_slow_start_beyond_the_hold_time_selects_instead_of_scrolling() {
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 8000.0, true);
        r.pen(0.45, 5000.0, 8000.0, true);
        for i in 1..=5 {
            r.pen(0.006, 5000.0, 8000.0 - 100.0 * i as f64, true);
        }
        let acts = r.take_actions();
        assert!(acts.contains(&Out::Button(Btn::Left, true)));
        assert!(!acts.iter().any(|o| matches!(o, Out::TouchDown(..))));
    }

    // ------------------------------------------------------------ wheel scroll (fallback)

    fn wheel_params() -> Params {
        Params { touch_scroll: false, ..Params::default() }
    }

    #[test]
    fn wheel_scroll_parks_the_cursor_scrolls_naturally_and_eases_back() {
        let mut r = Rig::with(wheel_params());
        r.pen(0.0, 5000.0, 8000.0, true);
        r.take();
        let y = drag_up(&mut r, 5000.0, 8000.0, 50, 20.0); // slow: below the flick speed
        let during = r.take();
        assert!(ptr_positions(&during).is_empty(), "cursor stays parked so the wheel hits what was under the pen");
        assert!(!during.iter().any(|o| matches!(o, Out::TouchDown(..) | Out::Button(..))));
        r.pen(0.01, 5000.0, y, false);
        r.run_to(r.t + 1.5);
        let all: Vec<Out> = during.into_iter().chain(r.take()).collect();
        let (hx, hy) = all.iter().fold((0.0, 0.0), |a, o| if let Out::Wheel(x, y) = o { (a.0 + x, a.1 + y) } else { a });
        assert!(hy < 0.0 && hx == 0.0, "drag up = content follows = negative wheel ({hx}, {hy})");
        // 42 frames x 20 units after the 2 mm slop (10 frames): 840-ish units * 120*20/9500
        let ideal = -20.0 * 40.0 * (120.0 * 20.0 / 9500.0);
        assert!((hy - ideal).abs() < 3.0 * 20.0 * (120.0 * 20.0 / 9500.0) + 1.0, "{hy} vs {ideal}");
    }

    #[test]
    fn wheel_cursor_glides_back_to_the_pen_without_one_big_jump() {
        let mut r = Rig::with(wheel_params());
        r.pen(0.0, 5000.0, 8000.0, true);
        let y = drag_up(&mut r, 5000.0, 8000.0, 40, 40.0);
        r.pen(0.01, 5000.0, y, false);
        r.take();
        r.run_to(r.t + 1.0);
        let pts = ptr_positions(&r.take());
        assert!(pts.len() >= 6, "several small steps, got {}", pts.len());
        assert_eq!(*pts.last().unwrap(), (5000.0, y));
        let start_gap = (8000.0 - y).abs();
        let biggest = pts.windows(2).map(|w| (w[1].1 - w[0].1).abs()).fold(0.0, f64::max);
        assert!(biggest < 0.7 * start_gap, "biggest step {biggest} of {start_gap}");
    }

    #[test]
    fn wheel_smoothing_spreads_the_motion_but_loses_nothing() {
        let total = |smoothing: f64| {
            let mut r = Rig::with(Params { touch_scroll: false, wheel_smoothing: smoothing, ..Params::default() });
            r.pen(0.0, 5000.0, 8000.0, true);
            let y = drag_up(&mut r, 5000.0, 8000.0, 60, 30.0);
            r.pen(0.01, 5000.0, y, false);
            r.run_to(r.t + 1.0);
            let outs = r.take();
            let n = outs.iter().filter(|o| matches!(o, Out::Wheel(..))).count();
            (outs.iter().fold(0.0, |a, o| if let Out::Wheel(_, v) = o { a + v } else { a }), n)
        };
        let (raw, n_raw) = total(0.0);
        let (smooth, n_smooth) = total(0.03);
        assert!((raw - smooth).abs() < 1.0, "{raw} vs {smooth}");
        assert!(n_smooth > n_raw / 2, "released at a steady rate ({n_smooth} vs {n_raw} events)");
    }

    #[test]
    fn wheel_axis_lock_ignores_sideways_wobble_but_allows_a_real_diagonal() {
        let wob = |dx: &dyn Fn(usize) -> f64| {
            let mut r = Rig::with(wheel_params());
            r.pen(0.0, 5000.0, 8000.0, true);
            let (mut x, mut y) = (5000.0, 8000.0);
            for i in 0..40 {
                x += dx(i);
                y -= 40.0;
                r.pen(0.01, x, y, true);
            }
            r.pen(0.01, x, y, false);
            r.run_to(r.t + 1.0);
            r.take().iter().fold((0.0, 0.0), |a, o| if let Out::Wheel(h, v) = o { (a.0 + h, a.1 + v) } else { a })
        };
        let (h, v) = wob(&|i| if i % 2 == 0 { 6.0 } else { -6.0 });
        assert!(h == 0.0 && v < 0.0);
        let (h, v) = wob(&|_| 40.0);
        assert!(h < 0.0 && v < 0.0, "diagonal scrolls both ways ({h}, {v})");
    }

    // ------------------------------------------------------------ cursor, buttons, safety

    #[test]
    fn hover_moves_the_cursor_only_while_the_pen_is_in_range() {
        let mut r = Rig::new();
        let mut out = vec![];
        r.g.update(1.0, 100.0, 200.0, false, true, &mut out);
        assert_eq!(out, vec![Out::Pointer(100.0, 200.0)]);
        out.clear();
        r.g.update(1.1, 300.0, 400.0, false, false, &mut out);
        assert!(out.is_empty(), "pen out of range: leave the cursor alone");
    }

    #[test]
    fn without_cursor_following_the_pointer_still_jumps_to_the_pen_for_clicks() {
        let mut r = Rig::with(Params { follow_hover: false, ..Params::default() });
        r.pen(0.0, 100.0, 100.0, false);
        r.pen(0.01, 5000.0, 3000.0, false);
        assert_eq!(r.take(), vec![], "no hover cursor");
        tap(&mut r, 0.1, 5000.0, 3000.0);
        r.run_to(r.t + 0.2);
        let outs = r.take();
        let press = outs.iter().position(|o| *o == Out::Button(Btn::Left, true)).unwrap();
        assert_eq!(outs[press - 1], Out::Pointer(5000.0, 3000.0));
    }

    #[test]
    fn the_cursor_holds_still_while_a_click_is_pressed() {
        let mut r = Rig::new();
        tap(&mut r, 0.0, 5000.0, 3000.0);
        // lift is committed after the debounce; the pen is already drifting away while the button is down
        r.run_to(r.t + 0.031);
        r.take();
        r.pen(0.005, 5400.0, 3000.0, false);
        assert_eq!(ptr_positions(&r.take()), vec![], "no cursor motion between press and release");
        r.run_to(r.t + 0.1);
        r.pen(0.005, 5400.0, 3000.0, false);
        assert!(ptr_positions(&r.take()).contains(&(5400.0, 3000.0)), "afterwards it follows again");
    }

    #[test]
    fn pen_side_buttons_are_right_and_middle_click_at_the_pen() {
        let mut r = Rig::new();
        r.pen(0.0, 7000.0, 4000.0, false);
        r.take();
        r.g.stylus(Btn::Right, true, &mut r.out);
        r.g.stylus(Btn::Right, false, &mut r.out);
        r.g.stylus(Btn::Middle, true, &mut r.out);
        r.g.stylus(Btn::Middle, false, &mut r.out);
        assert_eq!(
            r.take(),
            vec![
                Out::Pointer(7000.0, 4000.0),
                Out::Button(Btn::Right, true),
                Out::Button(Btn::Right, false),
                Out::Pointer(7000.0, 4000.0),
                Out::Button(Btn::Middle, true),
                Out::Button(Btn::Middle, false),
            ]
        );
    }

    #[test]
    fn reset_releases_a_touch_a_drag_and_every_button() {
        // while touch-scrolling
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 8000.0, true);
        drag_up(&mut r, 5000.0, 8000.0, 10, 80.0);
        r.take();
        r.g.reset(&mut r.out);
        assert_eq!(r.take(), vec![Out::TouchUp]);
        assert_eq!(r.g.next_deadline(), None);
        // while mouse-dragging
        let mut r = Rig::new();
        r.pen(0.0, 5000.0, 3000.0, true);
        r.pen(0.4, 5000.0, 3000.0, true);
        r.pen(0.006, 5300.0, 3000.0, true);
        r.take();
        r.g.reset(&mut r.out);
        assert_eq!(r.take(), vec![Out::Button(Btn::Left, false)]);
        // idle: harmless
        r.g.reset(&mut r.out);
        assert!(r.take().is_empty());
    }

    #[test]
    fn nothing_is_scheduled_when_idle_and_a_steady_hold_waits_only_for_its_timers() {
        let mut r = Rig::new();
        assert_eq!(r.g.next_deadline(), None);
        r.pen(0.0, 5000.0, 3000.0, true);
        assert!(r.g.next_deadline().is_some());
        r.run_to(r.t + 1.0); // right-click fired and released
        r.pen(0.0, 5000.0, 3000.0, true);
        assert_eq!(r.g.next_deadline(), None);
    }
}
