# Dave

Dave is a compact, transparent desktop overlay for CPU and GPU activity and
media playback. It is written in Rust and supports Linux and Windows, with
platform-specific telemetry and controls.

## Features

- CPU utilization on Linux and Windows.
- GPU utilization on Windows and Linux; GPU temperature is available through
  NVIDIA `nvidia-smi` or compatible Linux DRM/sysfs sensors.
- CPU temperature from supported Linux `hwmon` sensors or Windows firmware/ACPI
  sensors when exposed.
- A compact two-row overlay anchored to the top-right on supported desktops.
- Media title and previous, play/pause, and next controls.
- Spotify album-art accent color on Linux.
- Steam-game detection for background startup.
- An in-game FPS counter through MangoHud on Linux; use RTSS/Afterburner on
  Windows.
- **Alt+R** toggles the overlay; **Ctrl+]** skips to the next track.
- On Linux, only the media buttons capture pointer input; other overlay areas
  pass clicks through.
- The Windows build uses a native Win32 window and does not require GTK or
  MSYS2 at runtime.

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

Dave targets 64-bit Windows 10 version 1809 or later and Windows 11. Windows
10 22H2 or later is recommended. The media-session API used for track details
is available starting in Windows 10 version 1809.

To build from source, install Rust with the 64-bit MSVC toolchain and the
Visual Studio **Desktop development with C++** workload. Download and extract
the project, then open PowerShell in the extracted project folder (the folder
containing `Cargo.toml` and `install.ps1`). Do not run these commands from
`C:\Windows\System32`.

Build first and wait for it to finish successfully:

```powershell
cargo build --locked --release
```

Then install the app:

```powershell
.\install.ps1
```

If PowerShell says that running scripts is disabled, allow scripts only in the
current PowerShell window and run the installer again:

```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
.\install.ps1
```

This process-scoped setting resets when the PowerShell window is closed. The
installer needs the release executable produced by the build, so running it
before the build succeeds will fail.

The installer copies the standalone executable and creates Start menu and
sign-in shortcuts. No GTK, MSYS2, Python, PyWinRT, or third-party media service
is needed at runtime. Dave uses a native Win32 transparent overlay, native
Windows media-session APIs for track metadata, global media keys for controls,
`GetSystemTimes` for CPU utilization, and Windows GPU performance counters for
GPU utilization.

For NVIDIA, Dave prefers `nvidia-smi.exe` for GPU usage and temperature. GPU
usage falls back to Windows GPU-engine performance counters, which require a
WDDM 2.0-or-later driver; the fallback reports the busiest engine rather than
summing engines. CPU temperature is read from the firmware's ACPI thermal
zones when exposed. Windows does not provide a reliable vendor-independent GPU
temperature API, so GPU temperature is available through `nvidia-smi.exe` for
supported NVIDIA drivers and otherwise shows as unavailable. Steam game
detection, Alt+R, Ctrl+], and click-through outside the
three media buttons are supported. For an in-game FPS display, use
RTSS/Afterburner.

The Windows app is a desktop overlay, not a game-injected overlay. Games using
exclusive full-screen mode may cover it; borderless-windowed mode is the
recommended setup.

## Behavior and limitations

| Capability | Linux | Windows |
|---|---|---|
| CPU utilization | Yes | Yes |
| CPU temperature | Supported `hwmon` sensors | Firmware/ACPI WMI sensor, when exposed |
| GPU utilization | NVIDIA `nvidia-smi` or compatible DRM/sysfs | NVIDIA `nvidia-smi.exe` or Windows GPU-engine counters |
| GPU temperature | NVIDIA `nvidia-smi` or compatible DRM/sysfs | NVIDIA `nvidia-smi.exe`; unavailable for generic drivers |
| Track title and playback state | MPRIS through `playerctl` | Windows media-session API (Windows 10 1809+) |
| Media controls | `playerctl` | Global Windows media keys |
| Game detection | Steam app environment | Steam install-path process detection |
| In-game FPS | MangoHud | RTSS/Afterburner |
| Click-through outside media buttons | Supported layer-shell sessions | Supported |

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

GitHub Actions runs formatting, tests, lint, and release builds on Linux and
Windows Server 2022/2025. Windows CI compiles and runs the Windows-specific
unit tests without GTK/MSYS2. A green build does not prove runtime behavior on
every Windows 10/11 build, firmware sensor, GPU driver, or media player.

## License

Dave is licensed under the [MIT License](./LICENSE).
## Dave's programming featured AI help.
