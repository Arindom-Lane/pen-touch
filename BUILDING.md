# Building the AppImage yourself

You never need anyone else to build this for you. Everything is in this folder and one script
does the whole job. This guide explains what happens, so you can change things and rebuild with confidence.

## The short version

On any x86-64 Linux PC with internet access (Ubuntu, Debian, Fedora, Arch, ... anything):

```sh
tar xzf pen-touch-source.tar.gz
cd pen-touch
./build-appimage.sh --setup      # first time: installs what is missing, then builds
```

That's it. About 10 minutes the first time (it compiles every library), under 2 minutes after that.
The result is `PenTouch-x86_64.AppImage` next to the script.

Every time after, just:

```sh
./build-appimage.sh              # rebuild after you changed something
```

You need only `curl` and `python3` (with `venv`; on Debian/Ubuntu: `sudo apt install curl python3 python3-venv`).
Everything else is installed by `--setup`, and everything it installs lives inside this folder
(`.build-tools/`) or in `~/.cargo` and `~/.rustup` (Rust itself). Delete those to undo.

## What the script does (so it is not magic)

| Step | What | Why |
|------|------|-----|
| 1 | Installs **Rust** with rustup if `cargo` is missing | the language the program is written in |
| 2 | Installs **zig** into `.build-tools/venv` | used only as the *linker*, to make the program run on old systems |
| 3 | Installs **cargo-zigbuild** | the glue that lets cargo use zig |
| 4 | `cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17` | compiles the program so it needs only glibc 2.17 (2012) or newer |
| 5 | Builds the folder **AppDir/** | an AppImage is just a folder with a program, an icon and a launcher entry |
| 6 | Packs `AppDir/` with **appimagetool** | squashes the folder into one self-mounting file |

Why zig and glibc 2.17? A program compiled on a new system refuses to start on older ones
("GLIBC_2.38 not found"). Linking against 2.17 makes one file work everywhere. The window itself uses
the system's own X11/Wayland/OpenGL libraries at run time, so nothing graphical needs to be bundled.

## The edit - test - rebuild loop

```sh
cargo test                       # ~80 automated tests, takes seconds after the first build
cargo run -- status              # run the program straight from the source folder
cargo run                        # open the window
./build-appimage.sh              # make a new AppImage when you are happy
```

`cargo test` is the safety net: the gesture logic is tested without any hardware, so if you tweak a
number and a test fails, you know before you ever plug anything in.

### Where to change what

| You want to change | Edit |
|--------------------|------|
| hold time before select (0.35 s), right-click time (0.55 s), tap tolerance, double-tap timing | `src/gesture.rs` - `impl Default for Params` |
| what a gesture *does* (tap, drag, hold...) | `src/gesture.rs` - the `Gestures` struct |
| rotation, cursor area, how positions map to the screen | `src/pipeline.rs` |
| the window: options, texts, layout | `src/gui.rs` |
| the saved settings and their defaults | `src/config.rs` |
| single-click-to-open for more desktops | `src/clickmode.rs` |
| the one-time permission setup (udev rule) | `src/setup.rs` (`UDEV_RULES`) |
| the virtual devices the desktop sees | `src/input.rs` |
| the icon | `packaging/make_icon.py` (then rerun it) |

If you change a default in `Params`, change the matching test too - the tests describe the behaviour
in words, e.g. `hold_then_drag_is_a_real_mouse_drag_with_no_touch_at_all`.

## Other ways to build

* **Just for your own machine** (fastest, no zig): `cargo build --release` then run
  `target/release/pen-touch`. It will only work on distros at least as new as yours.
* **Without the window** (service + command line only): `cargo build --release --no-default-features`.
* **ARM** (Raspberry Pi etc.): `ARCH=aarch64 ./build-appimage.sh --setup` on an ARM machine.
* **In the cloud, automatically**: push the folder to a GitHub repository. The file
  `.github/workflows/appimage.yml` builds the AppImage on every version tag (`git tag v1.2.0 && git push --tags`)
  and attaches it to the release. You never have to build it on your own computer. (This workflow
  is written but not run by the author - if the first run complains, the error message names the step.)

## Troubleshooting

* **`cargo: command not found` right after installing Rust** - open a new terminal, or run `. ~/.cargo/env`.
* **`python3 -m venv failed`** - `sudo apt install python3-venv` (Debian/Ubuntu).
* **`appimagetool` cannot run (FUSE)** - the script already unpacks it to avoid FUSE; if your sandbox
  forbids even that, set `APPIMAGETOOL=/path/to/an/unpacked/appimagetool/AppRun`.
* **Running the AppImage says "FUSE" / "dlopen(): error loading libfuse.so.2"** - install FUSE 2
  (`sudo apt install libfuse2`, Fedora `fuse-libs`) or run it with `APPIMAGE_EXTRACT_AND_RUN=1 ./PenTouch-x86_64.AppImage`.
* **Build is slow / out of memory on a tiny machine** - add `CARGO_BUILD_JOBS=1` in front of the command.
* **Start over cleanly** - `rm -rf target AppDir .build-tools` and run the script again.
