# Dave

Dave is a compact, transparent desktop overlay for CPU and GPU activity and
media playback. It is written in Rust and supports Linux and Windows, with
platform-specific telemetry and controls.

## Features

- CPU utilization on Linux and Windows.
- GPU utilization and temperature from `nvidia-smi`; Linux also supports
  compatible DRM/sysfs sensors.
- CPU temperature from supported Linux `hwmon` sensors.
- A compact two-row overlay anchored to the top-right on supported desktops.
- Media title and previous, play/pause, and next controls.
- Spotify album-art accent color on Linux.
- Steam-game detection for background startup.
- An in-game FPS counter through MangoHud on Linux; use RTSS/Afterburner on
  Windows.
- **Alt+R** toggles the overlay; **Ctrl+]** skips to the next track.
- On Linux, only the media buttons capture pointer input; other overlay areas
  pass clicks through.

## Linux

### Requirements

On CachyOS or Arch Linux, install the build and runtime packages:

```sh
sudo pacman -S --needed rust gtk4 gtk4-layer-shell playerctl curl \
  xdg-desktop-portal xdg-desktop-portal-kde
```

Optional packages:

```sh
sudo pacman -S --needed nvidia-utils mangohud
```

`nvidia-utils` provides `nvidia-smi` for NVIDIA telemetry. Compatible AMD and
Intel DRM devices may expose telemetry through sysfs without it. MangoHud is
only needed for an in-game FPS display. The GTK layer-shell integration
requires a compatible Wayland desktop; other sessions may not support precise
top-right anchoring or click-through.

### Install

From the project directory:

```sh
bash install.sh
```

The installer builds Dave and installs it under `~/.local/bin`, adds a Fish
function named `sysmon`, registers a desktop entry, and configures KDE
autostart. Open a new Fish shell, then run:

```sh
sysmon
```

Dave starts in its own systemd user scope so KDE can identify the application
when it requests portal shortcuts. The autostart instance stays hidden until a
Steam game or active media player is detected.

### FPS in games

Install MangoHud and add this to a game's Steam launch options:

```text
mangohud %command%
```

Dave does not draw its own FPS counter.

## Windows

Windows builds use the MSYS2 UCRT64 toolchain and GTK4 runtime.

1. Install [MSYS2](https://www.msys2.org/).
2. Open the **MSYS2 UCRT64** terminal and install the build tools:

   ```sh
   pacman -S --needed mingw-w64-ucrt-x86_64-gtk4 \
     mingw-w64-ucrt-x86_64-pkgconf mingw-w64-ucrt-x86_64-rust
   ```

3. In the project directory, build Dave:

   ```sh
   cargo build --locked --release
   ```

4. From PowerShell in the same project directory, install Dave:

   ```powershell
   .\install.ps1
   ```

The installer creates Start menu and sign-in shortcuts. Launch Dave from the
Start menu. Its Windows backend supports CPU utilization, NVIDIA GPU telemetry
when `nvidia-smi.exe` is on `PATH`, Steam process detection, global hotkeys,
and system-wide media keys. Windows media metadata and CPU temperature are
not currently available. For an in-game FPS display, use RTSS/Afterburner.

## Behavior and limitations

| Capability | Linux | Windows |
|---|---|---|
| CPU utilization | Yes | Yes |
| CPU temperature | Supported `hwmon` sensors | Not available |
| GPU utilization and temperature | NVIDIA `nvidia-smi` or compatible DRM/sysfs | NVIDIA `nvidia-smi.exe` |
| Track title and playback state | MPRIS through `playerctl` | Not currently available |
| Media controls | `playerctl` | System-wide media keys |
| Game detection | Steam app environment | Steam install-path process detection |
| In-game FPS | MangoHud | RTSS/Afterburner |
| Click-through outside media buttons | Supported layer-shell sessions | Not implemented |

Telemetry availability depends on the GPU driver and exposed sensor
interfaces. Missing sensors are shown as unavailable rather than guessed.
Pausing with Dave's pause button keeps the overlay visible so the resume
control remains available.

## Privacy

Dave has no analytics or account sign-in. System and media information is
queried locally. On Linux, the Spotify artwork color feature downloads the
current artwork over HTTPS from Spotify image hosts using `curl`; artwork
URLs are limited to recognized Spotify domains and redirects are not followed.
No track history or artwork is uploaded by Dave.

## Development

The project uses Rust 2024 edition and a committed `Cargo.lock`. On Linux,
install the required GTK development packages, then run:

```sh
cargo fmt --all -- --check
cargo test --locked
cargo check --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --locked --release
```

GitHub Actions runs formatting, tests, lint, and a release build on Linux, plus
tests, a release build, and installer syntax validation on Windows. A green
build confirms compilation, not runtime testing on every desktop, driver, or
Windows configuration.

## License

Dave is licensed under the [MIT License](./LICENSE).
