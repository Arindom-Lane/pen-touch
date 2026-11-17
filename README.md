# Pen Touch

Use a pen tablet (Wacom or any evdev tablet) like a **native touch screen** on Linux. 

Project fully vibe coded, ty.


* One big **on/off switch** in a small window. It stays on or off across logouts and reboots until you change it.
* A single **AppImage** that runs on practically any x86-64 distro (needs glibc 2.17+, i.e. anything from the last ~10 years).
* **No modes.** Want pressure for drawing? Flip the switch off and the tablet is back to normal instantly.

## Gestures

| You do                      | You get                                                                 |
|-----------------------------|-------------------------------------------------------------------------|
| **tap**                     | left click                                                              |
| **double-tap**              | double click: select a word (or open a file)                            |
| **triple-tap**              | triple click: select a line / paragraph                                 |
| **drag** straight away      | scroll - mouse wheel (default, works in every app) or native touch scrolling (see Options) |
| **hold, then drag**         | a **real mouse drag**: select text, rubber-band several files, drag & drop. Let go and scrolling is back on |
| **hold still**              | right click                                                             |
| pen **side buttons**        | lower = right click, upper = middle click                               |

The cursor follows the pen while it hovers. In wheel mode (default) it stays where you started while you scroll - that is what makes every app scroll the right thing - and glides back when you lift. In touch mode it follows the pen even while scrolling.

*Why hold-then-drag is a real mouse drag and not touch:* toolkits such as Qt Widgets (Dolphin) turn a touch
hold into a fake mouse press, then treat the cursor's own movement as "button released" - which left a 1x1
selection stuck and made multi-select impossible. A genuine mouse button has no such problem.

## Use it

1. Download `PenTouch-x86_64.AppImage`, `chmod +x` it, run it.
2. First run only: if it says **One-time setup needed**, press **Fix permissions** (asks for your password once).
   This installs a udev rule so *your user* may create the virtual devices and read the tablet. No restart.
3. Flip the switch. Close the window if you like - it keeps running and starts at login.

From a terminal: `PenTouch.AppImage on | off | toggle | status`, and `PenTouch.AppImage single-click on|off`.

## Options (in the window)

* **Start automatically when I log in** (default on).
* **Cursor follows the pen** while hovering (and while touch-scrolling). Off = the cursor jumps to the pen only when you tap or hold.
* **Scrolling** - *Mouse wheel* (default): works in literally every app, Dolphin included; has a speed slider.
  *Touch*: the app's own native touch scrolling, and the cursor follows the pen - but apps without native touch
  scrolling (Dolphin, other Qt Widgets apps) treat a touch drag as a selection, so use the wheel there.
* **Single click opens files and folders** - flips your desktop's own setting (KDE, GNOME, Cinnamon, MATE, XFCE),
  so it also applies to a normal mouse. Reopen a file-manager window if it does not change at once.
* **Tap tolerance** - how far the pen may wobble during a tap/hold (default 2 mm). 1 mm of pen is ~12 px on screen,
  so without this, hand tremor would cancel taps and long-presses.
* **Tablet rotation** 0/90/180/270.
* **Cursor area** - several monitors: which part of the desktop the cursor covers. For touch scrolling also pick the
  output in your system's touch-screen settings (KDE: System Settings > Input Devices > Touchscreen).

## How it works

```
 pen tablet (grabbed) --> gesture engine --> "Pen Cursor (virtual)"        absolute mouse: cursor, clicks, drags, wheel
                                         \-> "Pen Touchscreen (virtual)"   multitouch screen: native scrolling
```

Every gesture is decided *before* anything is sent, so apps never see a half-started touch. The **background
service** (`pen-touch --daemon`) owns the devices and obeys the saved switch (`~/.config/pen-touch/config.toml`);
it starts at login (`~/.config/autostart/pen-touch.desktop`). The window is only a remote control for it.

## Build it yourself

See **BUILDING.md** - one command: `./build-appimage.sh --setup`.

## Troubleshooting

* **"needs permission"** - press *Fix permissions*, or run `sudo ./PenTouch.AppImage --setup-permissions`.
* **Nothing happens** - `PenTouch.AppImage status` shows what the service sees.
* **Cursor and touches land on different screens** - set the *Output* for "Pen Touchscreen (virtual)" in your
  system's touch settings, and the *Cursor area* in the window to the same monitor (wheel scrolling, the default,
  only needs the cursor area).
* **AppImage will not start (FUSE)** - `APPIMAGE_EXTRACT_AND_RUN=1 ./PenTouch-x86_64.AppImage`.
