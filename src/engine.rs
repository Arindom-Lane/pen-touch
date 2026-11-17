//! Runs one tablet "session" (events in, touch out) and keeps retrying across
//! unplug / replug / missing permissions until told to stop.

use crate::input::{self, RealPen, UinputSink};
use crate::pipeline::{Pipeline, Raw, Settings, Sink};
use crate::status::{State, Status};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Source of raw pen events. The real one wraps the kernel device; tests use a script.
pub trait Pen {
    /// Wait up to `timeout` for events. An empty Vec means "nothing yet".
    /// `ENODEV` / `EIO` mean the tablet is gone.
    fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Vec<Raw>>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum End {
    Stopped,
    Disconnected,
}

fn is_gone(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::ENODEV) | Some(libc::EIO) | Some(libc::ENOENT))
}

/// The event loop: wait for the pen or the next timer, whichever comes first.
pub fn session<P: Pen, S: Sink>(
    pen: &mut P,
    pipe: &mut Pipeline<S>,
    stop: &AtomicBool,
    clock: &dyn Fn() -> f64,
) -> io::Result<End> {
    loop {
        if stop.load(Ordering::Relaxed) {
            pipe.shutdown()?;
            return Ok(End::Stopped);
        }
        // Wake at least every 100 ms to notice `stop`; sooner if a timer is due.
        let mut timeout = Duration::from_millis(100);
        if let Some(d) = pipe.next_deadline() {
            timeout = timeout.min(Duration::from_secs_f64((d - clock()).max(0.0)));
        }
        match pen.wait(Some(timeout)) {
            Ok(events) => {
                for ev in events {
                    pipe.feed(ev, clock())?;
                }
            }
            Err(e) if is_gone(&e) => {
                let _ = pipe.shutdown(); // lift any finger before the virtual screen disappears
                return Ok(End::Disconnected);
            }
            Err(e) => return Err(e),
        }
        pipe.tick(clock())?;
    }
}

pub type SharedStatus = Arc<Mutex<Status>>;

fn set(status: &SharedStatus, state: State, device: &str, message: &str) {
    let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
    s.state = state;
    s.device = device.to_string();
    s.message = message.to_string();
}

fn sleep_unless_stopped(stop: &AtomicBool, total: Duration) {
    let step = Duration::from_millis(50);
    let mut left = total;
    while left > Duration::ZERO && !stop.load(Ordering::Relaxed) {
        let d = step.min(left);
        thread::sleep(d);
        left -= d;
    }
}

/// Body of the engine thread: find the tablet, grab it, translate it to touch, and start over
/// if it disappears. Returns only when `stop` is set. Everything is released on the way out.
pub fn run_engine(settings: Settings, stop: Arc<AtomicBool>, status: SharedStatus) {
    let t0 = Instant::now();
    let clock = move || t0.elapsed().as_secs_f64();

    while !stop.load(Ordering::Relaxed) {
        let candidates = input::candidates();
        let Some(cand) = candidates.first() else {
            set(&status, State::Waiting, "", "");
            sleep_unless_stopped(&stop, Duration::from_secs(1));
            continue;
        };

        let mut pen = match RealPen::open(&cand.path) {
            Ok(p) => p,
            Err(e) if input::is_permission_error(&e) => {
                set(&status, State::NoPermission, &cand.name, "This user may not read the tablet");
                sleep_unless_stopped(&stop, Duration::from_secs(2));
                continue;
            }
            Err(e) if e.raw_os_error() == Some(libc::EBUSY) => {
                set(&status, State::Error, &cand.name, "Another program is already using the tablet");
                sleep_unless_stopped(&stop, Duration::from_secs(2));
                continue;
            }
            Err(e) if is_gone(&e) => {
                set(&status, State::Waiting, "", "");
                sleep_unless_stopped(&stop, Duration::from_secs(1));
                continue;
            }
            Err(e) => {
                set(&status, State::Error, &cand.name, &format!("Cannot open the tablet: {e}"));
                sleep_unless_stopped(&stop, Duration::from_secs(2));
                continue;
            }
        };

        let sink = match UinputSink::new() {
            Ok(s) => s,
            Err(e) => {
                pen.release();
                if input::is_permission_error(&e) || input::is_missing_error(&e) {
                    set(&status, State::NoPermission, &cand.name, "Cannot create virtual input devices (/dev/uinput)");
                } else {
                    set(&status, State::Error, &cand.name, &format!("Cannot create virtual devices: {e}"));
                }
                sleep_unless_stopped(&stop, Duration::from_secs(2));
                continue;
            }
        };

        let mut pipe = Pipeline::new(sink, pen.geom, settings.clone());
        for ev in pen.initial_events() {
            let _ = pipe.feed(ev, clock());
        }
        set(&status, State::Active, &pen.name, "");

        let result = session(&mut pen, &mut pipe, &stop, &clock);
        pen.release(); // hand the tablet back to the desktop first ...
        drop(pipe); //    ... then remove the virtual devices

        match result {
            Ok(End::Stopped) => break,
            Ok(End::Disconnected) => {
                set(&status, State::Waiting, "", "");
                sleep_unless_stopped(&stop, Duration::from_secs(1));
            }
            Err(e) => {
                set(&status, State::Error, &cand.name, &format!("{e}"));
                sleep_unless_stopped(&stop, Duration::from_secs(2));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gesture::Btn;
    use crate::pipeline::tests::{geom, settings, Cmd, FakeSink, PEN};
    use crate::pipeline::{ABS_X, ABS_Y, BTN_TOUCH};
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// A scripted tablet on a fake clock: `wait` advances time to the next scripted report,
    /// or to the end of the timeout if nothing is due first - like a real poll().
    struct FakePen {
        clock: Rc<Cell<f64>>,
        script: VecDeque<(f64, Vec<Raw>)>,
        on_empty: Box<dyn Fn() -> io::Result<Vec<Raw>>>,
        calls: Cell<u32>,
    }
    impl Pen for FakePen {
        fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Vec<Raw>> {
            self.calls.set(self.calls.get() + 1);
            assert!(
                self.calls.get() < 200_000,
                "session loop is spinning: now={} timeout={:?} next_event={:?}",
                self.clock.get(),
                timeout,
                self.script.front().map(|(t, _)| *t)
            );
            let now = self.clock.get();
            let limit = now + timeout.map(|d| d.as_secs_f64()).unwrap_or(f64::INFINITY);
            match self.script.front() {
                Some((t, _)) if *t <= limit => {
                    let (t, evs) = self.script.pop_front().unwrap();
                    self.clock.set(t.max(now));
                    Ok(evs)
                }
                Some(_) => {
                    // A full sleep lands just PAST the deadline: Duration truncates to whole
                    // nanoseconds, so without the nudge a timer could stay a hair in the future.
                    self.clock.set(limit + 1e-9);
                    Ok(vec![])
                }
                None => (self.on_empty)(),
            }
        }
    }

    fn rig(script: Vec<(f64, Vec<Raw>)>, on_empty: Box<dyn Fn() -> io::Result<Vec<Raw>>>) -> (FakePen, Rc<Cell<f64>>) {
        let clock = Rc::new(Cell::new(0.0));
        (FakePen { clock: clock.clone(), script: script.into(), on_empty, calls: Cell::new(0) }, clock)
    }

    fn gone() -> Box<dyn Fn() -> io::Result<Vec<Raw>>> {
        Box::new(|| Err(io::Error::from_raw_os_error(libc::ENODEV)))
    }

    fn actions(p: &Pipeline<FakeSink>) -> Vec<Cmd> {
        p.sink().cmds.iter().copied().filter(|c| !matches!(c, Cmd::Ptr(..))).collect()
    }

    /// A pen hovering, then touching, holding 0.4 s and dragging sideways: a mouse drag with the left button held.
    fn hold_and_drag_script() -> Vec<(f64, Vec<Raw>)> {
        let mut v = vec![
            (0.0, vec![Raw::Key(PEN, 1), Raw::Abs(ABS_X, 5000), Raw::Abs(ABS_Y, 3000), Raw::Syn]),
            (0.1, vec![Raw::Key(BTN_TOUCH, 1), Raw::Syn]),
        ];
        for i in 1..=10 {
            v.push((0.5 + 0.006 * i as f64, vec![Raw::Abs(ABS_X, 5000 + 60 * i), Raw::Syn]));
        }
        v
    }

    #[test]
    fn a_tap_flows_through_the_whole_loop_with_real_timers() {
        let (mut pen, clock) = rig(
            vec![
                (0.00, vec![Raw::Key(PEN, 1), Raw::Abs(ABS_X, 7600), Raw::Abs(ABS_Y, 4750), Raw::Syn]),
                (1.00, vec![Raw::Key(BTN_TOUCH, 1), Raw::Syn]),
                (1.08, vec![Raw::Key(BTN_TOUCH, 0), Raw::Syn]),
                (1.30, vec![Raw::Abs(ABS_X, 7610), Raw::Syn]), // still plugged in while the click plays out
            ],
            gone(),
        );
        let mut pipe = Pipeline::new(FakeSink::default(), geom(), settings());
        let stop = AtomicBool::new(false);
        let c = clock.clone();
        let end = session(&mut pen, &mut pipe, &stop, &move || c.get()).unwrap();
        assert_eq!(end, End::Disconnected);
        assert_eq!(actions(&pipe), vec![Cmd::Btn(Btn::Left, true), Cmd::Btn(Btn::Left, false)], "click after the lift debounce, released by its own timer");
        let press = pipe.sink().cmds.iter().position(|c| *c == Cmd::Btn(Btn::Left, true)).unwrap();
        assert_eq!(pipe.sink().cmds[press - 1], Cmd::Ptr(16384, 16384), "pointer exactly at the tap");
    }

    #[test]
    fn unplugging_mid_drag_releases_the_mouse_button() {
        let (mut pen, clock) = rig(hold_and_drag_script(), gone());
        let mut pipe = Pipeline::new(FakeSink::default(), geom(), settings());
        let stop = AtomicBool::new(false);
        let c = clock.clone();
        assert_eq!(session(&mut pen, &mut pipe, &stop, &move || c.get()).unwrap(), End::Disconnected);
        assert_eq!(actions(&pipe), vec![Cmd::Btn(Btn::Left, true), Cmd::Btn(Btn::Left, false)], "no stuck button after the tablet vanishes");
    }

    #[test]
    fn stop_flag_ends_the_session_and_releases_everything() {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let (mut pen, clock) = rig(
            hold_and_drag_script(),
            Box::new(move || {
                s2.store(true, Ordering::Relaxed); // "the user flipped the switch off"
                Ok(vec![])
            }),
        );
        let mut pipe = Pipeline::new(FakeSink::default(), geom(), settings());
        let c = clock.clone();
        assert_eq!(session(&mut pen, &mut pipe, &stop, &move || c.get()).unwrap(), End::Stopped);
        assert_eq!(actions(&pipe), vec![Cmd::Btn(Btn::Left, true), Cmd::Btn(Btn::Left, false)]);
    }

    #[test]
    fn unplugging_mid_touch_scroll_lifts_the_finger() {
        let mut v = vec![
            (0.0, vec![Raw::Key(PEN, 1), Raw::Abs(ABS_X, 5000), Raw::Abs(ABS_Y, 8000), Raw::Syn]),
            (0.1, vec![Raw::Key(BTN_TOUCH, 1), Raw::Syn]),
        ];
        for i in 1..=15 {
            v.push((0.1 + 0.006 * i as f64, vec![Raw::Abs(ABS_Y, 8000 - 70 * i), Raw::Syn]));
        }
        let (mut pen, clock) = rig(v, gone());
        let mut pipe = Pipeline::new(FakeSink::default(), geom(), settings());
        let stop = AtomicBool::new(false);
        let c = clock.clone();
        assert_eq!(session(&mut pen, &mut pipe, &stop, &move || c.get()).unwrap(), End::Disconnected);
        let a = actions(&pipe);
        assert!(matches!(a.first(), Some(Cmd::Down(..))));
        assert_eq!(a.last(), Some(&Cmd::Up), "no stuck finger on the virtual screen");
    }

    #[test]
    fn a_hard_error_is_reported_not_swallowed() {
        let (mut pen, clock) = rig(vec![], Box::new(|| Err(io::Error::from_raw_os_error(libc::EINVAL))));
        let mut pipe = Pipeline::new(FakeSink::default(), geom(), settings());
        let stop = AtomicBool::new(false);
        let c = clock.clone();
        assert!(session(&mut pen, &mut pipe, &stop, &move || c.get()).is_err());
    }

    #[test]
    fn long_idle_stretches_do_not_spin_the_loop() {
        // 10 s of nothing: the loop must wait in <=100 ms slices, not busy-loop
        let waits = Rc::new(Cell::new(0u32));
        let w2 = waits.clone();
        struct Counting(Rc<Cell<f64>>, Rc<Cell<u32>>);
        impl Pen for Counting {
            fn wait(&mut self, t: Option<Duration>) -> io::Result<Vec<Raw>> {
                self.1.set(self.1.get() + 1);
                let now = self.0.get();
                if now >= 10.0 {
                    return Err(io::Error::from_raw_os_error(libc::ENODEV));
                }
                self.0.set(now + t.unwrap().as_secs_f64());
                Ok(vec![])
            }
        }
        let clock = Rc::new(Cell::new(0.0));
        let mut pen = Counting(clock.clone(), w2);
        let mut pipe = Pipeline::new(FakeSink::default(), geom(), settings());
        let stop = AtomicBool::new(false);
        let c = clock.clone();
        session(&mut pen, &mut pipe, &stop, &move || c.get()).unwrap();
        assert!(waits.get() <= 105, "{} waits for 10 s", waits.get());
    }
}
