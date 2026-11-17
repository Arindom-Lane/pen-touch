//! "Single click to open files and folders".
//!
//! Faking this by sending two clicks per tap would break buttons and text selection, so this
//! flips the desktop's OWN setting instead (the same switch you find in System Settings / the
//! file manager's preferences). That means it also applies to a normal mouse, which is how
//! every desktop defines it.
//!
//! All commands go through the `Runner` trait so they can be tested without a real desktop.

use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    Kde,
    Gnome,
    Cinnamon,
    Mate,
    Xfce,
    Unknown,
}

impl Desktop {
    /// Parse `$XDG_CURRENT_DESKTOP` (e.g. "KDE", "ubuntu:GNOME", "X-Cinnamon", "XFCE").
    pub fn detect(xdg_current_desktop: &str) -> Desktop {
        let u = xdg_current_desktop.to_ascii_uppercase();
        let has = |name: &str| u.split(':').any(|p| p.contains(name));
        if has("KDE") || has("PLASMA") {
            Desktop::Kde
        } else if has("CINNAMON") {
            Desktop::Cinnamon
        } else if has("MATE") {
            Desktop::Mate
        } else if has("XFCE") {
            Desktop::Xfce
        } else if has("GNOME") || has("UNITY") || has("BUDGIE") || has("PANTHEON") {
            Desktop::Gnome
        } else {
            Desktop::Unknown
        }
    }

    pub fn from_env() -> Desktop {
        Desktop::detect(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
    }

    pub fn file_manager(self) -> &'static str {
        match self {
            Desktop::Kde => "Dolphin",
            Desktop::Gnome => "Files (Nautilus)",
            Desktop::Cinnamon => "Nemo",
            Desktop::Mate => "Caja",
            Desktop::Xfce => "Thunar",
            Desktop::Unknown => "your file manager",
        }
    }
}

/// Runs a program; `Ok(stdout)` only if it started and exited with success.
pub trait Runner {
    fn run(&self, prog: &str, args: &[&str]) -> Result<String, String>;
}

pub struct RealRunner;

impl Runner for RealRunner {
    fn run(&self, prog: &str, args: &[&str]) -> Result<String, String> {
        match Command::new(prog).args(args).output() {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
}

fn word(on: bool, yes: &'static str, no: &'static str) -> &'static str {
    if on {
        yes
    } else {
        no
    }
}

/// gsettings schema holding `click-policy` for the GTK-family file managers.
fn gsettings_schema(d: Desktop) -> Option<&'static str> {
    match d {
        Desktop::Gnome => Some("org.gnome.nautilus.preferences"),
        Desktop::Cinnamon => Some("org.nemo.preferences"),
        Desktop::Mate => Some("org.mate.caja.preferences"),
        _ => None,
    }
}

/// Is single-click-to-open on right now? `None` = unknown (unsupported desktop or tool missing).
pub fn read_single_click(r: &dyn Runner, d: Desktop) -> Option<bool> {
    match d {
        Desktop::Kde => {
            for tool in ["kreadconfig6", "kreadconfig5"] {
                if let Ok(v) = r.run(tool, &["--file", "kdeglobals", "--group", "KDE", "--key", "SingleClick", "--default", "false"]) {
                    return Some(v.trim().eq_ignore_ascii_case("true"));
                }
            }
            None
        }
        Desktop::Xfce => {
            let v = r.run("xfconf-query", &["-c", "thunar", "-p", "/misc-single-click"]).ok()?;
            match v.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            }
        }
        d => {
            let schema = gsettings_schema(d)?;
            let v = r.run("gsettings", &["get", schema, "click-policy"]).ok()?;
            if v.contains("single") {
                Some(true)
            } else if v.contains("double") {
                Some(false)
            } else {
                None
            }
        }
    }
}

/// Turn single-click-to-open on or off. Returns a message for the user.
pub fn write_single_click(r: &dyn Runner, d: Desktop, on: bool) -> Result<String, String> {
    let note = format!(
        "Single click is now {} in {}. If an open window does not change, reopen it.",
        word(on, "ON", "OFF"),
        d.file_manager()
    );
    match d {
        Desktop::Kde => {
            let val = word(on, "true", "false");
            let mut last = String::new();
            for tool in ["kwriteconfig6", "kwriteconfig5"] {
                match r.run(tool, &["--file", "kdeglobals", "--group", "KDE", "--key", "SingleClick", val]) {
                    Ok(_) => {
                        // tell running KDE applications to re-read it (harmless if nobody listens)
                        let _ = r.run(
                            "dbus-send",
                            &["--session", "--type=signal", "/KGlobalSettings", "org.kde.KGlobalSettings.notifyChange", "int32:3", "int32:0"],
                        );
                        return Ok(note);
                    }
                    Err(e) => last = e,
                }
            }
            Err(format!("Could not change the KDE setting ({last}). Set it in System Settings > General Behavior."))
        }
        Desktop::Xfce => r
            .run("xfconf-query", &["-c", "thunar", "-p", "/misc-single-click", "-n", "-t", "bool", "-s", word(on, "true", "false")])
            .map(|_| note)
            .map_err(|e| format!("Could not change the Thunar setting ({e}).")),
        Desktop::Unknown => Err("Pen Touch does not know how to change this on your desktop. \
                                 Turn on single-click in your file manager's preferences."
            .to_string()),
        d => {
            let schema = gsettings_schema(d).unwrap();
            r.run("gsettings", &["set", schema, "click-policy", word(on, "single", "double")])
                .map(|_| note)
                .map_err(|e| format!("Could not change the setting ({e})."))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records every command; programs in `missing` behave like "command not found".
    struct Fake {
        calls: RefCell<Vec<String>>,
        missing: Vec<&'static str>,
        answer: &'static str,
    }
    impl Fake {
        fn new(answer: &'static str, missing: &[&'static str]) -> Fake {
            Fake { calls: RefCell::new(vec![]), missing: missing.to_vec(), answer }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }
    impl Runner for Fake {
        fn run(&self, prog: &str, args: &[&str]) -> Result<String, String> {
            self.calls.borrow_mut().push(format!("{prog} {}", args.join(" ")));
            if self.missing.contains(&prog) {
                Err(format!("{prog}: not found"))
            } else {
                Ok(self.answer.to_string())
            }
        }
    }

    #[test]
    fn desktops_are_recognised_from_xdg_current_desktop() {
        for (s, d) in [
            ("KDE", Desktop::Kde),
            ("plasma", Desktop::Kde),
            ("GNOME", Desktop::Gnome),
            ("ubuntu:GNOME", Desktop::Gnome),
            ("X-Cinnamon", Desktop::Cinnamon),
            ("MATE", Desktop::Mate),
            ("XFCE", Desktop::Xfce),
            ("Budgie:GNOME", Desktop::Gnome),
            ("sway", Desktop::Unknown),
            ("", Desktop::Unknown),
        ] {
            assert_eq!(Desktop::detect(s), d, "{s}");
        }
    }

    #[test]
    fn kde_writes_kdeglobals_and_notifies() {
        let f = Fake::new("", &[]);
        let msg = write_single_click(&f, Desktop::Kde, true).unwrap();
        assert!(msg.contains("ON") && msg.contains("Dolphin"));
        let c = f.calls();
        assert_eq!(c[0], "kwriteconfig6 --file kdeglobals --group KDE --key SingleClick true");
        assert!(c[1].starts_with("dbus-send --session"));
        let f = Fake::new("", &[]);
        write_single_click(&f, Desktop::Kde, false).unwrap();
        assert!(f.calls()[0].ends_with("SingleClick false"));
    }

    #[test]
    fn kde_falls_back_to_the_plasma_5_tools() {
        let f = Fake::new("", &["kwriteconfig6"]);
        write_single_click(&f, Desktop::Kde, true).unwrap();
        assert!(f.calls().iter().any(|c| c.starts_with("kwriteconfig5 ")));
        let none = Fake::new("", &["kwriteconfig6", "kwriteconfig5"]);
        let e = write_single_click(&none, Desktop::Kde, true).unwrap_err();
        assert!(e.contains("System Settings"), "{e}");
    }

    #[test]
    fn gnome_family_use_their_own_schema() {
        for (d, schema) in [
            (Desktop::Gnome, "org.gnome.nautilus.preferences"),
            (Desktop::Cinnamon, "org.nemo.preferences"),
            (Desktop::Mate, "org.mate.caja.preferences"),
        ] {
            let f = Fake::new("", &[]);
            write_single_click(&f, d, true).unwrap();
            assert_eq!(f.calls(), vec![format!("gsettings set {schema} click-policy single")]);
            let f = Fake::new("", &[]);
            write_single_click(&f, d, false).unwrap();
            assert_eq!(f.calls(), vec![format!("gsettings set {schema} click-policy double")]);
        }
    }

    #[test]
    fn xfce_uses_xfconf() {
        let f = Fake::new("", &[]);
        write_single_click(&f, Desktop::Xfce, true).unwrap();
        assert_eq!(f.calls(), vec!["xfconf-query -c thunar -p /misc-single-click -n -t bool -s true"]);
    }

    #[test]
    fn unknown_desktop_explains_what_to_do_instead() {
        let f = Fake::new("", &[]);
        assert!(write_single_click(&f, Desktop::Unknown, true).unwrap_err().contains("file manager"));
        assert!(f.calls().is_empty(), "no commands guessed");
        assert_eq!(read_single_click(&f, Desktop::Unknown), None);
    }

    #[test]
    fn reading_parses_every_desktops_answer() {
        assert_eq!(read_single_click(&Fake::new("true", &[]), Desktop::Kde), Some(true));
        assert_eq!(read_single_click(&Fake::new("false", &[]), Desktop::Kde), Some(false));
        assert_eq!(read_single_click(&Fake::new("false", &["kreadconfig6"]), Desktop::Kde), Some(false), "plasma 5 fallback");
        assert_eq!(read_single_click(&Fake::new("", &["kreadconfig6", "kreadconfig5"]), Desktop::Kde), None);
        assert_eq!(read_single_click(&Fake::new("'single'", &[]), Desktop::Gnome), Some(true));
        assert_eq!(read_single_click(&Fake::new("'double'", &[]), Desktop::Gnome), Some(false));
        assert_eq!(read_single_click(&Fake::new("'ask'", &[]), Desktop::Gnome), None);
        assert_eq!(read_single_click(&Fake::new("true", &[]), Desktop::Xfce), Some(true));
        assert_eq!(read_single_click(&Fake::new("banana", &[]), Desktop::Xfce), None);
    }

    #[test]
    fn a_failing_command_is_reported_not_swallowed() {
        let f = Fake::new("", &["gsettings"]);
        assert!(write_single_click(&f, Desktop::Gnome, true).is_err());
    }
}
