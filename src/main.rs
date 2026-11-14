#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::{
    cell::{Cell, RefCell},
    io,
    process::{Command, Output},
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

#[cfg(target_os = "linux")]
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::FileTypeExt,
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
};

#[cfg(target_os = "linux")]
use ashpd::WindowIdentifier;
#[cfg(target_os = "linux")]
use ashpd::desktop::{
    CreateSessionOptions,
    global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut},
};
#[cfg(target_os = "linux")]
use futures_util::StreamExt;
use gtk::{gdk, glib, pango, prelude::*};
#[cfg(target_os = "linux")]
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
#[cfg(target_os = "windows")]
use sysinfo::{ProcessesToUpdate, System};
#[cfg(target_os = "windows")]
use windows::{
    Win32::{
        Foundation::{FILETIME, HWND, RECT},
        System::Threading::GetSystemTimes,
        UI::{
            Input::KeyboardAndMouse::{
                INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
                MOD_ALT, MOD_CONTROL, RegisterHotKey, SendInput, VIRTUAL_KEY, VK_MEDIA_NEXT_TRACK,
                VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_MENU, VK_OEM_6, VK_R,
            },
            WindowsAndMessaging::{
                FindWindowW, GetMessageW, GetSystemMetrics, GetWindowRect, HWND_TOPMOST, MSG,
                SM_CXSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos, WM_HOTKEY,
            },
        },
    },
    core::w,
};

const REFRESH_INTERVAL: Duration = Duration::from_millis(500);
const GPU_CHECK_INTERVAL: Duration = Duration::from_millis(500);
const GAME_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const MEDIA_CHECK_INTERVAL: Duration = Duration::from_millis(500);
#[cfg(target_os = "linux")]
const MAX_TOGGLE_COMMAND_BYTES: u64 = 16;

#[derive(Clone)]
struct MediaInfo {
    player: String,
    title: String,
    playing: bool,
    art_url: Option<String>,
}

enum ShortcutAction {
    ToggleOverlay,
    SkipTrack,
    Error(String),
}

#[derive(Default)]
struct CpuSampler {
    previous: Option<(u64, u64)>,
}

impl CpuSampler {
    #[cfg(target_os = "linux")]
    fn sample(&mut self) -> io::Result<Option<u8>> {
        let contents = fs::read_to_string("/proc/stat")?;
        let values = contents
            .lines()
            .next()
            .filter(|line| line.starts_with("cpu "))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing CPU counters"))?
            .split_whitespace()
            .skip(1)
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

        if values.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete CPU counters",
            ));
        }

        let idle = values[3] + values.get(4).copied().unwrap_or_default();
        let total: u64 = values.iter().sum();
        let current = (total, idle);
        let result = self.previous.map(|(old_total, old_idle)| {
            let total_delta = total.saturating_sub(old_total);
            let idle_delta = idle.saturating_sub(old_idle);
            total_delta
                .saturating_sub(idle_delta)
                .saturating_mul(100)
                .checked_div(total_delta)
                .unwrap_or_default()
                .min(100) as u8
        });
        self.previous = Some(current);
        Ok(result)
    }

    #[cfg(target_os = "windows")]
    fn sample(&mut self) -> io::Result<Option<u8>> {
        let mut idle = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe {
            GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user))
                .map_err(|error| io::Error::other(error.to_string()))?;
        }

        let to_u64 =
            |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        let idle = to_u64(idle);
        let total = to_u64(kernel) + to_u64(user);
        let result = self.previous.map(|(old_total, old_idle)| {
            let total_delta = total.saturating_sub(old_total);
            let idle_delta = idle.saturating_sub(old_idle);
            total_delta
                .saturating_sub(idle_delta)
                .saturating_mul(100)
                .checked_div(total_delta)
                .unwrap_or_default()
                .min(100) as u8
        });
        self.previous = Some((total, idle));
        Ok(result)
    }
}

#[cfg(target_os = "linux")]
fn cpu_temperature() -> Option<i32> {
    let hwmon = fs::read_dir("/sys/class/hwmon").ok()?;
    for device in hwmon.flatten() {
        let Ok(name) = fs::read_to_string(device.path().join("name")) else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        if !["coretemp", "k10temp", "zenpower", "cpu"]
            .iter()
            .any(|needle| name.contains(needle))
        {
            continue;
        }

        let Ok(sensors) = fs::read_dir(device.path()) else {
            continue;
        };
        for sensor in sensors.flatten() {
            let filename = sensor.file_name();
            let filename = filename.to_string_lossy();
            if filename.starts_with("temp")
                && filename.ends_with("_input")
                && let Some(value) = fs::read_to_string(sensor.path())
                    .ok()
                    .and_then(|value| value.trim().parse::<i32>().ok())
            {
                return Some(value / 1000);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn cpu_temperature() -> Option<i32> {
    None
}

#[derive(Default)]
struct GpuTelemetry {
    utilization: Option<u8>,
    temperature: Option<i32>,
}

fn parse_gpu_telemetry(output: &str) -> Option<GpuTelemetry> {
    let mut fields = output.lines().next()?.split(',');
    let utilization = fields
        .next()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
        .map(|value| value.round() as u8);
    let temperature = fields
        .next()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && (0.0..=200.0).contains(value))
        .map(|value| value.round() as i32);

    if utilization.is_none() && temperature.is_none() {
        None
    } else {
        Some(GpuTelemetry {
            utilization,
            temperature,
        })
    }
}

fn nvidia_telemetry() -> Option<GpuTelemetry> {
    #[cfg(target_os = "linux")]
    let executable = "nvidia-smi";
    #[cfg(target_os = "windows")]
    let executable = "nvidia-smi.exe";

    let output = Command::new(executable)
        .args([
            "--query-gpu=utilization.gpu,temperature.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    parse_gpu_telemetry(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "linux")]
fn sysfs_gpu_telemetry() -> Option<GpuTelemetry> {
    let cards = fs::read_dir("/sys/class/drm").ok()?;
    for card in cards.filter_map(Result::ok) {
        let name = card.file_name();
        let name = name.to_string_lossy();
        if !name
            .strip_prefix("card")
            .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
        {
            continue;
        }

        let device = card.path().join("device");
        let utilization = fs::read_to_string(device.join("gpu_busy_percent"))
            .ok()
            .and_then(|value| value.trim().parse::<u8>().ok())
            .filter(|value| *value <= 100);
        let temperature = fs::read_dir(device.join("hwmon"))
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .flat_map(|directory| {
                fs::read_dir(directory.path())
                    .ok()
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
            })
            .filter(|sensor| {
                let name = sensor.file_name();
                let name = name.to_string_lossy();
                name.starts_with("temp") && name.ends_with("_input")
            })
            .find_map(|sensor| {
                fs::read_to_string(sensor.path())
                    .ok()
                    .and_then(|value| value.trim().parse::<i32>().ok())
                    .map(|value| value / 1000)
                    .filter(|value| (0..=200).contains(value))
            });

        if utilization.is_some() || temperature.is_some() {
            return Some(GpuTelemetry {
                utilization,
                temperature,
            });
        }
    }
    None
}

fn gpu_telemetry() -> Option<GpuTelemetry> {
    #[cfg(target_os = "linux")]
    {
        nvidia_telemetry().or_else(sysfs_gpu_telemetry)
    }
    #[cfg(target_os = "windows")]
    {
        nvidia_telemetry()
    }
}

#[cfg(target_os = "linux")]
fn artwork_accent(bytes: &[u8]) -> Option<[u8; 3]> {
    let loader = gtk::gdk_pixbuf::PixbufLoader::new();
    loader.set_size(64, 64);
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    let pixbuf = loader.pixbuf()?;
    let pixels = pixbuf.read_pixel_bytes();
    let pixels = pixels.as_ref();
    let width = pixbuf.width() as usize;
    let height = pixbuf.height() as usize;
    let channels = pixbuf.n_channels() as usize;
    let rowstride = pixbuf.rowstride() as usize;
    if width == 0 || height == 0 || channels < 3 {
        return None;
    }

    let total = width.checked_mul(height)?;
    let sample_step = total.div_ceil(2500).max(1);
    let mut vivid_sum = [0u64; 3];
    let mut vivid_count = 0u64;
    let mut all_sum = [0u64; 3];
    let mut all_count = 0u64;

    for pixel_index in (0..total).step_by(sample_step) {
        let x = pixel_index % width;
        let y = pixel_index / width;
        let offset = y * rowstride + x * channels;
        let Some(pixel) = pixels.get(offset..offset + channels) else {
            continue;
        };
        if channels >= 4 && pixel[3] < 128 {
            continue;
        }
        let color = [pixel[0], pixel[1], pixel[2]];
        let maximum = *color.iter().max()?;
        let minimum = *color.iter().min()?;
        if maximum < 48 {
            continue;
        }

        for (sum, component) in all_sum.iter_mut().zip(color) {
            *sum += u64::from(component);
        }
        all_count += 1;
        if maximum - minimum >= 24 {
            for (sum, component) in vivid_sum.iter_mut().zip(color) {
                *sum += u64::from(component);
            }
            vivid_count += 1;
        }
    }

    let (sum, count) = if vivid_count > 0 {
        (vivid_sum, vivid_count)
    } else {
        (all_sum, all_count)
    };
    (count > 0).then(|| sum.map(|component| (component / count) as u8))
}

#[cfg(target_os = "linux")]
fn is_spotify_artwork_url(url: &str) -> bool {
    let Some(authority) = url
        .strip_prefix("https://")
        .and_then(|value| value.split('/').next())
    else {
        return false;
    };
    if authority.is_empty() || authority.contains(['@', ':', '%']) {
        return false;
    }

    let host = authority.to_ascii_lowercase();
    let valid_dns_name = host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    valid_dns_name
        && (host == "open.spotify.com"
            || host == "i.scdn.co"
            || host.ends_with(".scdn.co")
            || host.ends_with(".spotifycdn.com"))
}

#[cfg(target_os = "linux")]
fn fetch_spotify_artwork(url: &str) -> Result<Vec<u8>, String> {
    if !is_spotify_artwork_url(url) {
        return Err("Spotify artwork URL uses an unsupported host".to_owned());
    }

    let output = Command::new("curl")
        .args([
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "4",
            "--max-filesize",
            "2097152",
            "--proto",
            "=https",
            "--",
            url,
        ])
        .output()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                "Install curl to color Dave from Spotify artwork".to_owned()
            } else {
                format!("Could not fetch Spotify artwork: {error}")
            }
        })?;
    if !output.status.success() {
        return Err(format!(
            "Could not fetch Spotify artwork: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if output.stdout.len() > 2 * 1024 * 1024 {
        return Err("Spotify artwork exceeds the 2 MiB limit".to_owned());
    }
    Ok(output.stdout)
}

#[cfg(target_os = "linux")]
fn current_media() -> Result<Option<MediaInfo>, String> {
    let output = Command::new("playerctl")
        .args([
            "--all-players",
            "metadata",
            "--format",
            "{{playerName}}|{{artist}} - {{title}}|{{mpris:artUrl}}",
        ])
        .output()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                "Install playerctl to show media".to_owned()
            } else {
                format!("Could not query media players: {error}")
            }
        })?;

    if !output.status.success() {
        return Ok(None);
    }

    let metadata = String::from_utf8_lossy(&output.stdout);
    Ok(select_media(&metadata, player_status))
}

fn select_media(
    metadata: &str,
    mut player_status: impl FnMut(&str) -> Option<bool>,
) -> Option<MediaInfo> {
    let mut paused_media = None;
    for line in metadata.lines() {
        let mut fields = line.splitn(3, '|');
        let (Some(player), Some(track)) = (fields.next(), fields.next()) else {
            continue;
        };
        let player = player.trim();
        let Some(playing) = player_status(player) else {
            continue;
        };

        let track = track.trim();
        let media = MediaInfo {
            player: player.to_owned(),
            playing,
            art_url: fields
                .next()
                .map(str::trim)
                .filter(|url| !url.is_empty() && *url != "-")
                .map(str::to_owned),
            title: if track.is_empty() || track == "-" {
                format!("Playing on {player}")
            } else {
                track.to_owned()
            },
        };
        if playing {
            return Some(media);
        }
        paused_media.get_or_insert(media);
    }
    paused_media
}

#[cfg(target_os = "linux")]
fn player_status(player: &str) -> Option<bool> {
    let output = Command::new("playerctl")
        .args(["--player", player, "status"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&output.stdout).trim() {
        "Playing" => Some(true),
        "Paused" => Some(false),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn current_media() -> Result<Option<MediaInfo>, String> {
    Ok(None)
}

#[cfg(target_os = "linux")]
fn skip_track() -> Result<(), String> {
    let output = Command::new("playerctl")
        .arg("next")
        .output()
        .map_err(|error| format!("Could not run playerctl: {error}"))?;
    ensure_success(output, "playerctl next")
}

#[cfg(target_os = "linux")]
fn control_player(player: &str, action: &str) -> Result<(), String> {
    let output = Command::new("playerctl")
        .args(["--player", player, action])
        .output()
        .map_err(|error| format!("Could not run playerctl {action}: {error}"))?;
    ensure_success(output, &format!("playerctl {action}"))
}

#[cfg(target_os = "windows")]
fn control_player(_player: &str, action: &str) -> Result<(), String> {
    let key = match action {
        "previous" => VK_MEDIA_PREV_TRACK,
        "play-pause" => VK_MEDIA_PLAY_PAUSE,
        "next" => VK_MEDIA_NEXT_TRACK,
        _ => return Err(format!("Unsupported media action on Windows: {action}")),
    };
    send_media_key(key)
}

#[cfg(target_os = "windows")]
fn skip_track() -> Result<(), String> {
    send_media_key(VK_MEDIA_NEXT_TRACK)
}

#[cfg(target_os = "windows")]
fn send_media_key(key: VIRTUAL_KEY) -> Result<(), String> {
    let keyboard_input = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let events = [
        keyboard_input(KEYEVENTF_EXTENDEDKEY),
        keyboard_input(KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
    ];
    let sent = unsafe { SendInput(&events, std::mem::size_of::<INPUT>() as i32) };
    if sent == events.len() as u32 {
        Ok(())
    } else {
        Err("Windows did not accept the media-next key event".to_owned())
    }
}

#[cfg(target_os = "linux")]
fn steam_game_running() -> bool {
    let Ok(processes) = fs::read_dir("/proc") else {
        return false;
    };

    processes.flatten().any(|process| {
        if !process
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            return false;
        }

        let Ok(environment) = fs::read(process.path().join("environ")) else {
            return false;
        };
        environment.split(|byte| *byte == 0).any(|entry| {
            let Some(value) = entry.strip_prefix(b"SteamAppId=") else {
                return false;
            };
            value.iter().all(u8::is_ascii_digit) && value.iter().any(|byte| *byte != b'0')
        })
    })
}

#[cfg(target_os = "windows")]
fn steam_game_running() -> bool {
    use std::sync::{Mutex, OnceLock};

    static SYSTEM: OnceLock<Mutex<System>> = OnceLock::new();
    let system = SYSTEM.get_or_init(|| Mutex::new(System::new()));
    let Ok(mut system) = system.lock() else {
        return false;
    };

    system.refresh_processes(ProcessesToUpdate::All, true);
    system.processes().values().any(|process| {
        process.exe().is_some_and(|path| {
            path.to_string_lossy()
                .to_ascii_lowercase()
                .contains("\\steamapps\\common\\")
        })
    })
}

#[cfg(target_os = "linux")]
fn socket_path() -> Result<PathBuf, String> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set; cannot create the toggle socket".to_owned())?;
    Ok(PathBuf::from(runtime_dir).join("system-monitor-overlay.sock"))
}

#[cfg(target_os = "linux")]
fn send_toggle() -> Result<(), String> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(path).map_err(|error| error.to_string())?;
    stream
        .write_all(b"toggle")
        .map_err(|error| format!("Could not send toggle command: {error}"))
}

#[cfg(target_os = "linux")]
fn launch_from_desktop_entry() -> Result<(), String> {
    Command::new("gtk-launch")
        .arg("io.github.systemmonitor.overlay")
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not launch the desktop app entry: {error}"))
}

#[cfg(target_os = "windows")]
fn send_toggle() -> Result<(), String> {
    let window = unsafe { FindWindowW(None, w!("Dave")) };
    if window.is_err() {
        return Err("The monitor is not running".to_owned());
    }

    let keyboard_input = |key, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let events = [
        keyboard_input(VK_MENU, Default::default()),
        keyboard_input(VK_R, Default::default()),
        keyboard_input(VK_R, KEYEVENTF_KEYUP),
        keyboard_input(VK_MENU, KEYEVENTF_KEYUP),
    ];
    let sent = unsafe { SendInput(&events, std::mem::size_of::<INPUT>() as i32) };
    if sent == events.len() as u32 {
        Ok(())
    } else {
        Err("Windows did not accept the overlay toggle shortcut".to_owned())
    }
}

#[cfg(target_os = "linux")]
fn start_ipc_server(sender: Sender<ShortcutAction>) {
    thread::spawn(move || {
        let path = match socket_path() {
            Ok(path) => path,
            Err(error) => {
                let _ = sender.send(ShortcutAction::Error(error));
                return;
            }
        };
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(bind_error) => {
                match UnixStream::connect(&path) {
                    Ok(_) => {
                        let _ = sender.send(ShortcutAction::Error(
                            "Another system monitor instance is already running".to_owned(),
                        ));
                        return;
                    }
                    Err(_) => {
                        match fs::symlink_metadata(&path) {
                            Ok(metadata) if metadata.file_type().is_socket() => {}
                            Ok(_) => {
                                let _ = sender.send(ShortcutAction::Error(format!(
                                    "Refusing to remove non-socket toggle path {}",
                                    path.display()
                                )));
                                return;
                            }
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => {
                                let _ = sender.send(ShortcutAction::Error(format!(
                                    "Could not inspect stale toggle socket {}: {error}",
                                    path.display()
                                )));
                                return;
                            }
                        }
                        if let Err(remove_error) = fs::remove_file(&path)
                            && remove_error.kind() != io::ErrorKind::NotFound
                        {
                            let _ = sender.send(ShortcutAction::Error(format!(
                                "Could not remove stale toggle socket {}: {remove_error}",
                                path.display()
                            )));
                            return;
                        }
                    }
                }
                match UnixListener::bind(&path) {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = sender.send(ShortcutAction::Error(format!(
                            "Could not create toggle socket after {bind_error}: {error}"
                        )));
                        return;
                    }
                }
            }
        };

        if let Err(error) = listener.set_nonblocking(true) {
            let _ = sender.send(ShortcutAction::Error(format!(
                "Could not configure toggle socket: {error}"
            )));
            let _ = fs::remove_file(path);
            return;
        }

        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    if let Err(error) = stream.set_read_timeout(Some(Duration::from_secs(1))) {
                        eprintln!("Could not set toggle socket read timeout: {error}");
                        continue;
                    }
                    let mut command = String::new();
                    match stream
                        .take(MAX_TOGGLE_COMMAND_BYTES)
                        .read_to_string(&mut command)
                    {
                        Ok(_) if command.trim() == "toggle" => {
                            if sender.send(ShortcutAction::ToggleOverlay).is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(error) => eprintln!("Could not read toggle command: {error}"),
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    let _ = sender.send(ShortcutAction::Error(format!(
                        "Toggle socket failed: {error}"
                    )));
                    break;
                }
            }
        }
        let _ = fs::remove_file(path);
    });
}

fn ensure_success(output: Output, command: &str) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        let details = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if details.is_empty() {
            format!("{command} exited with {}", output.status)
        } else {
            format!("{command} failed: {details}")
        })
    }
}

#[cfg(target_os = "linux")]
fn start_global_shortcuts(sender: Sender<ShortcutAction>) -> Sender<String> {
    let (parent_sender, parent_receiver) = mpsc::channel();
    start_global_shortcuts_with_parent(sender, parent_receiver);
    parent_sender
}

#[cfg(target_os = "linux")]
fn start_global_shortcuts_with_parent(
    sender: Sender<ShortcutAction>,
    parent_receiver: Receiver<String>,
) {
    thread::spawn(move || {
        let parent = match parent_receiver.recv() {
            Ok(parent) => parent,
            Err(error) => {
                let _ = sender.send(ShortcutAction::Error(format!(
                    "Could not get the overlay window identifier: {error}"
                )));
                return;
            }
        };
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = sender.send(ShortcutAction::Error(format!(
                    "Could not start shortcut service: {error}"
                )));
                return;
            }
        };

        if let Err(error) = runtime.block_on(register_global_shortcuts(sender.clone(), parent)) {
            let _ = sender.send(ShortcutAction::Error(error));
        }
    });
}

#[cfg(target_os = "linux")]
async fn register_global_shortcuts(
    sender: Sender<ShortcutAction>,
    parent: String,
) -> Result<(), String> {
    let portal = GlobalShortcuts::new()
        .await
        .map_err(|error| format!("Global shortcuts unavailable: {error}"))?;
    let session = portal
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|error| format!("Could not create shortcut session: {error}"))?;
    let handle = parent
        .strip_prefix("wayland:")
        .ok_or_else(|| format!("Unsupported overlay window identifier: {parent}"))?;
    let parent = WindowIdentifier::from_xdg_foreign_exported(handle.to_owned());
    let shortcuts = [
        NewShortcut::new("toggle-overlay", "Toggle system monitor")
            .preferred_trigger(Some("ALT+R")),
        NewShortcut::new("skip-track", "Skip current track")
            .preferred_trigger(Some("CTRL+BRACKETRIGHT")),
    ];
    portal
        .bind_shortcuts(
            &session,
            &shortcuts,
            Some(&parent),
            BindShortcutsOptions::default(),
        )
        .await
        .and_then(|request| request.response())
        .map_err(|error| format!("Could not register global shortcuts: {error}"))?;

    let mut activated = portal
        .receive_activated()
        .await
        .map_err(|error| format!("Could not listen for shortcuts: {error}"))?;
    while let Some(signal) = activated.next().await {
        let action = match signal.shortcut_id() {
            "toggle-overlay" => ShortcutAction::ToggleOverlay,
            "skip-track" => ShortcutAction::SkipTrack,
            _ => continue,
        };
        if sender.send(action).is_err() {
            break;
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn start_global_shortcuts(sender: Sender<ShortcutAction>) {
    thread::spawn(move || unsafe {
        let toggle_registered = RegisterHotKey(None, 1, MOD_ALT, u32::from(VK_R.0)).is_ok();
        let skip_registered = RegisterHotKey(None, 2, MOD_CONTROL, u32::from(VK_OEM_6.0)).is_ok();

        if !toggle_registered {
            let _ = sender.send(ShortcutAction::Error(
                "Could not register Alt+R; another app may be using it".to_owned(),
            ));
        }
        if !skip_registered {
            let _ = sender.send(ShortcutAction::Error(
                "Could not register Ctrl+]; another app may be using it".to_owned(),
            ));
        }

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            if message.message == WM_HOTKEY {
                let action = match message.wParam.0 {
                    1 if toggle_registered => Some(ShortcutAction::ToggleOverlay),
                    2 if skip_registered => Some(ShortcutAction::SkipTrack),
                    _ => None,
                };
                if let Some(action) = action {
                    if sender.send(action).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "window { background: transparent; }
         .panel {
             background-color: transparent;
             border: none;
         }
         .content {
             padding: 2px 6px;
         }
         .metrics-row {
             min-height: 20px;
         }
         .metric {
             min-width: 104px;
         }
         .metrics {
             color: #f5f7fc;
             font: 600 10px monospace;
         }
         .separator {
             background-color: rgba(255, 255, 255, 0.12);
             min-width: 1px;
         }
         .track {
             color: #f5f7fc;
             font: 10px sans-serif;
         }
         .music-icon {
             color: #f5f7fc;
             font: 12px sans-serif;
         }
         .media-button,
         .media-button:hover,
         .media-button:active,
         .media-button:focus {
             min-width: 22px;
             min-height: 22px;
             padding: 0;
             border-radius: 999px;
             color: #f5f7fc;
             border: none;
             box-shadow: none;
             outline-style: none;
             background-color: transparent;
         }",
    );
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

#[cfg(target_os = "linux")]
fn install_spotify_theme() -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    provider.load_from_data("");
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
    }
    provider
}

#[cfg(target_os = "linux")]
fn set_spotify_theme(provider: &gtk::CssProvider, panel: &gtk::Box, color: Option<[u8; 3]>) {
    let Some([red, green, blue]) = color else {
        panel.remove_css_class("spotify-theme");
        provider.load_from_data("");
        return;
    };
    let color = format!("#{red:02x}{green:02x}{blue:02x}");
    provider.load_from_data(&format!(
        ".spotify-theme .media-button:hover {{ background-color: {color}; }}"
    ));
    panel.add_css_class("spotify-theme");
}

fn build_metric(name: &str) -> gtk::Label {
    let metric = gtk::Label::new(Some(&format!("{name} --% --°C")));
    metric.add_css_class("metric");
    metric.add_css_class("metrics");
    metric.set_xalign(0.5);
    metric.set_hexpand(true);
    metric
}

fn media_button(icon: &str, description: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("media-button");
    button.set_tooltip_text(Some(description));
    button.set_focusable(false);
    button
}

fn connect_media_button(
    button: &gtk::Button,
    active_player: &Rc<RefCell<Option<String>>>,
    action: &'static str,
    on_success: impl Fn() + 'static,
) {
    let active_player = active_player.clone();
    button.connect_clicked(move |button| {
        let Some(player) = active_player.borrow().clone() else {
            return;
        };
        if let Err(error) = control_player(&player, action) {
            eprintln!("{error}");
            button.set_tooltip_text(Some(&error));
        } else {
            on_success();
        }
    });
}

fn update_label(label: &gtk::Label, text: &str) {
    if label.text().as_str() != text {
        label.set_text(text);
    }
}

fn automatic_visibility(game_running: bool, media_active: bool, paused_media_hold: bool) -> bool {
    game_running || media_active || paused_media_hold
}

#[cfg(target_os = "linux")]
fn update_click_through_region(
    window: &gtk::ApplicationWindow,
    controls_visible: bool,
    buttons: [&gtk::Button; 3],
) {
    let Some(surface) = window.surface() else {
        return;
    };
    let region = gtk::cairo::Region::create();
    if controls_visible {
        for button in buttons {
            let Some(bounds) = button.compute_bounds(window) else {
                continue;
            };
            let rect = gtk::cairo::RectangleInt::new(
                bounds.x().floor() as i32,
                bounds.y().floor() as i32,
                bounds.width().ceil() as i32,
                bounds.height().ceil() as i32,
            );
            if let Err(error) = region.union_rectangle(&rect) {
                eprintln!("Could not update overlay click region: {error}");
                return;
            }
        }
    }
    surface.set_input_region(Some(&region));
}

fn build_ui(application: &gtk::Application, background_mode: bool) {
    install_css();
    #[cfg(target_os = "linux")]
    let spotify_theme = install_spotify_theme();

    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("Dave")
        .default_width(270)
        .default_height(54)
        .width_request(250)
        .height_request(50)
        .decorated(false)
        .resizable(true)
        .build();

    #[cfg(target_os = "linux")]
    if gtk4_layer_shell::is_supported() {
        window.init_layer_shell();
        window.set_namespace(Some("system-monitor-overlay"));
        window.set_layer(Layer::Overlay);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Right, true);
        window.set_margin(Edge::Top, 0);
        window.set_margin(Edge::Right, 0);
        window.set_exclusive_zone(0);
        window.set_keyboard_mode(KeyboardMode::None);
    } else {
        eprintln!(
            "Wayland layer-shell is unavailable; the overlay cannot be anchored to the top-right."
        );
    }

    let panel = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    panel.add_css_class("panel");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 1);
    content.add_css_class("content");

    let metrics_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    metrics_row.add_css_class("metrics-row");

    let cpu_metric = build_metric("CPU");
    let gpu_metric = build_metric("GPU");
    let separator = gtk::Box::new(gtk::Orientation::Vertical, 0);
    separator.add_css_class("separator");
    separator.set_size_request(1, -1);

    metrics_row.append(&cpu_metric);
    metrics_row.append(&separator);
    metrics_row.append(&gpu_metric);
    content.append(&metrics_row);

    let track_row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let music_icon = gtk::Label::new(Some("🎧"));
    music_icon.add_css_class("music-icon");
    let initial_track = if cfg!(target_os = "windows") {
        "Media controls ready"
    } else {
        "No media playing"
    };
    let track = gtk::Label::new(Some(initial_track));
    track.set_xalign(0.0);
    track.set_ellipsize(pango::EllipsizeMode::End);
    track.set_width_chars(12);
    track.set_max_width_chars(24);
    track.set_hexpand(true);
    track.add_css_class("track");
    track_row.append(&music_icon);
    track_row.append(&track);

    let controls_revealer = gtk::Revealer::new();
    controls_revealer.set_transition_type(gtk::RevealerTransitionType::SlideRight);
    controls_revealer.set_transition_duration(120);
    controls_revealer.set_reveal_child(false);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    let previous_button = media_button("media-skip-backward-symbolic", "Previous track");
    let pause_button = media_button("media-playback-pause-symbolic", "Pause");
    let next_button = media_button("media-skip-forward-symbolic", "Next track");
    controls.append(&previous_button);
    controls.append(&pause_button);
    controls.append(&next_button);
    controls_revealer.set_child(Some(&controls));
    track_row.append(&controls_revealer);
    content.append(&track_row);
    panel.append(&content);
    window.set_child(Some(&panel));

    let active_player = Rc::new(RefCell::new(None::<String>));
    let paused_media_hold = Rc::new(Cell::new(false));
    #[cfg(target_os = "windows")]
    {
        *active_player.borrow_mut() = Some("windows-media-keys".to_owned());
    }
    connect_media_button(&previous_button, &active_player, "previous", || {});
    let paused_media_hold_for_button = paused_media_hold.clone();
    connect_media_button(&pause_button, &active_player, "play-pause", move || {
        paused_media_hold_for_button.set(!paused_media_hold_for_button.get());
    });
    connect_media_button(&next_button, &active_player, "next", || {});

    let (sender, receiver) = mpsc::channel();
    #[cfg(target_os = "linux")]
    let parent_sender = start_global_shortcuts(sender.clone());
    #[cfg(target_os = "windows")]
    start_global_shortcuts(sender.clone());
    #[cfg(target_os = "linux")]
    start_ipc_server(sender.clone());

    #[cfg(target_os = "linux")]
    let window_identifier = Rc::new(RefCell::new(None::<WindowIdentifier>));
    #[cfg(target_os = "linux")]
    {
        let window_identifier_ref = window_identifier.clone();
        let identifier_requested = Rc::new(Cell::new(false));
        let identifier_requested_ref = identifier_requested.clone();
        let action_sender = sender.clone();
        let window_for_hide = window.clone();
        window.connect_map(move |window| {
            if identifier_requested_ref.replace(true) {
                return;
            }
            let window_identifier = window_identifier_ref.clone();
            let parent_sender = parent_sender.clone();
            let action_sender = action_sender.clone();
            let window_for_hide = window_for_hide.clone();
            let window_native = window.clone();
            glib::MainContext::default().spawn_local(async move {
                match WindowIdentifier::from_native(&window_native).await {
                    Some(identifier) => {
                        let handle = identifier.to_string();
                        *window_identifier.borrow_mut() = Some(identifier);
                        if let Err(error) = parent_sender.send(handle) {
                            let _ = action_sender.send(ShortcutAction::Error(format!(
                                "Could not pass the overlay window to the shortcut portal: {error}"
                            )));
                        }
                        if background_mode {
                            window_for_hide.set_visible(false);
                        }
                    }
                    None => {
                        let _ = action_sender.send(ShortcutAction::Error(
                            "Could not identify the overlay window for the shortcut portal"
                                .to_owned(),
                        ));
                    }
                }
            });
        });
    }

    let window_ref = window.clone();
    #[cfg(target_os = "linux")]
    let panel_ref = panel.clone();
    #[cfg(target_os = "linux")]
    let spotify_theme_ref = spotify_theme.clone();
    #[cfg(target_os = "linux")]
    let (artwork_sender, artwork_receiver) = mpsc::channel::<(String, Result<Vec<u8>, String>)>();
    #[cfg(target_os = "linux")]
    let mut current_art_url = None::<String>;
    #[cfg(target_os = "linux")]
    let mut requested_art_url = None::<String>;
    let cpu_metric_ref = cpu_metric.clone();
    let gpu_metric_ref = gpu_metric.clone();
    let track_ref = track.clone();
    let controls_revealer_ref = controls_revealer.clone();
    let pause_button_ref = pause_button.clone();
    #[cfg(target_os = "linux")]
    let previous_button_ref = previous_button.clone();
    #[cfg(target_os = "linux")]
    let next_button_ref = next_button.clone();
    let active_player_ref = active_player.clone();
    let paused_media_hold_ref = paused_media_hold.clone();
    let cpu = Rc::new(RefCell::new(CpuSampler::default()));
    let shortcut_error = Rc::new(RefCell::new(None::<String>));
    let shortcut_error_ref = shortcut_error.clone();
    let mut manual_visibility = if background_mode { None } else { Some(true) };
    let mut game_running = false;
    let mut media_active = false;
    let mut media_playing = false;
    #[cfg(target_os = "linux")]
    let mut retained_media_info = None::<MediaInfo>;
    let mut game_elapsed = GAME_CHECK_INTERVAL;
    let mut media_elapsed = MEDIA_CHECK_INTERVAL;
    let mut gpu_elapsed = GPU_CHECK_INTERVAL;
    let mut gpu = GpuTelemetry::default();
    let mut previous_auto_visibility = false;
    let mut last_media_error = None::<String>;
    #[cfg(target_os = "linux")]
    let window_identifier_ref = window_identifier.clone();

    glib::timeout_add_local(REFRESH_INTERVAL, move || {
        #[cfg(target_os = "linux")]
        let _keep_window_identifier = &window_identifier_ref;
        game_elapsed += REFRESH_INTERVAL;
        if game_elapsed >= GAME_CHECK_INTERVAL {
            game_elapsed = Duration::ZERO;
            game_running = steam_game_running();
        }
        media_elapsed += REFRESH_INTERVAL;
        let mut media_info = None;
        if media_elapsed >= MEDIA_CHECK_INTERVAL {
            media_elapsed = Duration::ZERO;
            match current_media() {
                Ok(media) => {
                    if let Some(media) = media {
                        #[cfg(target_os = "linux")]
                        {
                            let artwork_url = media
                                .player
                                .to_ascii_lowercase()
                                .contains("spotify")
                                .then(|| media.art_url.clone())
                                .flatten();
                            if artwork_url != current_art_url {
                                current_art_url = artwork_url.clone();
                                requested_art_url = None;
                                set_spotify_theme(&spotify_theme_ref, &panel_ref, None);
                            }
                            if let Some(url) =
                                artwork_url.filter(|url| requested_art_url.as_ref() != Some(url))
                            {
                                requested_art_url = Some(url.clone());
                                let sender = artwork_sender.clone();
                                thread::spawn(move || {
                                    let result = fetch_spotify_artwork(&url);
                                    let _ = sender.send((url, result));
                                });
                            }
                        }
                        media_active = true;
                        media_playing = media.playing;
                        #[cfg(target_os = "linux")]
                        {
                            retained_media_info = Some(media.clone());
                        }
                        *active_player_ref.borrow_mut() = Some(media.player.clone());
                        media_info = Some(media);
                    } else if cfg!(target_os = "linux") {
                        #[cfg(target_os = "linux")]
                        if paused_media_hold_ref.get() {
                            media_active = true;
                            media_playing = false;
                            media_info = retained_media_info.clone();
                        } else {
                            let active_player = active_player_ref.borrow().clone();
                            if let Some(player) = active_player {
                                if let Some(playing) = player_status(&player) {
                                    media_active = true;
                                    media_playing = playing;
                                    media_info = retained_media_info.clone();
                                } else {
                                    current_art_url = None;
                                    requested_art_url = None;
                                    retained_media_info = None;
                                    set_spotify_theme(&spotify_theme_ref, &panel_ref, None);
                                    media_active = false;
                                    media_playing = false;
                                    *active_player_ref.borrow_mut() = None;
                                }
                            } else {
                                current_art_url = None;
                                requested_art_url = None;
                                retained_media_info = None;
                                set_spotify_theme(&spotify_theme_ref, &panel_ref, None);
                                media_active = false;
                                media_playing = false;
                            }
                        }
                    }
                    last_media_error = None;
                }
                Err(error) => {
                    #[cfg(target_os = "linux")]
                    {
                        if active_player_ref.borrow().is_none() {
                            current_art_url = None;
                            requested_art_url = None;
                            retained_media_info = None;
                            set_spotify_theme(&spotify_theme_ref, &panel_ref, None);
                        }
                    }
                    if active_player_ref.borrow().is_none() {
                        media_active = false;
                    }
                    media_playing = false;
                    if last_media_error.as_ref() != Some(&error) {
                        eprintln!("{error}");
                    }
                    last_media_error = Some(error.clone());
                    update_label(&track_ref, &error);
                }
            }
        }
        #[cfg(target_os = "linux")]
        while let Ok((url, result)) = artwork_receiver.try_recv() {
            if current_art_url.as_ref() != Some(&url) {
                continue;
            }
            match result {
                Ok(bytes) => match artwork_accent(&bytes) {
                    Some(color) => {
                        set_spotify_theme(&spotify_theme_ref, &panel_ref, Some(color));
                    }
                    None => eprintln!("Could not extract a color from Spotify artwork"),
                },
                Err(error) => eprintln!("{error}"),
            }
        }
        gpu_elapsed += REFRESH_INTERVAL;
        if gpu_elapsed >= GPU_CHECK_INTERVAL {
            gpu_elapsed = Duration::ZERO;
            gpu = gpu_telemetry().unwrap_or_default();
        }
        process_shortcut_actions(
            &receiver,
            &window_ref,
            &track_ref,
            &shortcut_error_ref,
            &mut manual_visibility,
        );
        let automatic_visibility =
            automatic_visibility(game_running, media_active, paused_media_hold_ref.get());
        if automatic_visibility != previous_auto_visibility {
            manual_visibility = None;
            previous_auto_visibility = automatic_visibility;
        }
        let visible = manual_visibility.unwrap_or(automatic_visibility);
        if visible && !window_ref.is_visible() {
            window_ref.set_visible(true);
        } else {
            window_ref.set_visible(visible);
        }
        #[cfg(target_os = "windows")]
        if visible {
            position_windows_overlay();
        }

        let utilization = cpu.borrow_mut().sample().ok().flatten();
        let cpu_load = utilization
            .map(|value| format!("{value:>2}%"))
            .unwrap_or_else(|| "--%".to_owned());
        let cpu_temp = cpu_temperature()
            .map(|value| format!("{value}°C"))
            .unwrap_or_else(|| "--°C".to_owned());
        let gpu_load = gpu
            .utilization
            .map(|value| format!("{value:>2}%"))
            .unwrap_or_else(|| "--%".to_owned());
        let gpu_temp = gpu
            .temperature
            .map(|value| format!("{value}°C"))
            .unwrap_or_else(|| "--°C".to_owned());
        update_label(&cpu_metric_ref, &format!("CPU {cpu_load} {cpu_temp}"));
        update_label(&gpu_metric_ref, &format!("GPU {gpu_load} {gpu_temp}"));

        if shortcut_error.borrow().is_some() {
            update_label(&track_ref, "Shortcuts unavailable");
        } else if let Some(media) = media_info {
            update_label(&track_ref, &media.title);
        } else if active_player_ref.borrow().is_none() && last_media_error.is_none() {
            update_label(&track_ref, "No media playing");
        }
        let show_controls =
            active_player_ref.borrow().is_some() && (visible || cfg!(target_os = "windows"));
        controls_revealer_ref.set_reveal_child(show_controls);
        #[cfg(target_os = "linux")]
        update_click_through_region(
            &window_ref,
            show_controls,
            [&previous_button_ref, &pause_button_ref, &next_button_ref],
        );
        if pause_button_ref.icon_name().as_deref()
            != Some(if media_playing {
                "media-playback-pause-symbolic"
            } else {
                "media-playback-start-symbolic"
            })
        {
            pause_button_ref.set_icon_name(if media_playing {
                "media-playback-pause-symbolic"
            } else {
                "media-playback-start-symbolic"
            });
            pause_button_ref.set_tooltip_text(Some(if media_playing { "Pause" } else { "Resume" }));
        }

        glib::ControlFlow::Continue
    });

    #[cfg(target_os = "windows")]
    glib::timeout_add_local_once(Duration::from_millis(300), position_windows_overlay);

    window.set_visible(true);
}

fn process_shortcut_actions(
    receiver: &Receiver<ShortcutAction>,
    window: &gtk::ApplicationWindow,
    track: &gtk::Label,
    shortcut_error: &Rc<RefCell<Option<String>>>,
    manual_visibility: &mut Option<bool>,
) {
    while let Ok(action) = receiver.try_recv() {
        match action {
            ShortcutAction::ToggleOverlay => {
                *manual_visibility = Some(!window.is_visible());
                window.set_visible(!window.is_visible());
            }
            ShortcutAction::SkipTrack => {
                if let Err(error) = skip_track() {
                    eprintln!("{error}");
                    track.set_text(&error);
                    *shortcut_error.borrow_mut() = Some(error);
                } else {
                    *shortcut_error.borrow_mut() = None;
                }
            }
            ShortcutAction::Error(error) => {
                eprintln!("{error}");
                track.set_text("Shortcuts unavailable");
                track.set_tooltip_text(Some(&error));
                *shortcut_error.borrow_mut() = Some(error);
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn position_windows_overlay() {
    unsafe {
        let Ok(window) = FindWindowW(None, w!("Dave")) else {
            return;
        };
        if window.is_invalid() {
            return;
        }

        let screen_width = GetSystemMetrics(SM_CXSCREEN);
        let mut rect = RECT::default();
        let _ = GetWindowRect(window, &mut rect);
        let width = (rect.right - rect.left).max(220);
        let height = (rect.bottom - rect.top).max(50);
        let _ = SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            screen_width - width,
            0,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

fn main() {
    let background_mode = std::env::args().any(|argument| argument == "--background");
    if std::env::args().any(|argument| argument == "--toggle") {
        match send_toggle() {
            Ok(()) => return,
            Err(error) => {
                #[cfg(target_os = "linux")]
                if launch_from_desktop_entry().is_ok() {
                    return;
                }
                eprintln!("{error}");
                #[cfg(target_os = "linux")]
                eprintln!("Launch the app from the desktop entry or install it with install.sh.");
            }
        }
    }

    let application = gtk::Application::builder()
        .application_id("io.github.systemmonitor.overlay")
        .build();
    application.connect_activate(move |application| build_ui(application, background_mode));
    application.run_with_args(&["system-monitor-overlay"]);
}

#[cfg(test)]
mod tests {
    use super::{automatic_visibility, parse_gpu_telemetry, select_media};

    #[cfg(target_os = "linux")]
    #[test]
    fn restricts_spotify_artwork_to_https_spotify_hosts() {
        assert!(super::is_spotify_artwork_url(
            "https://i.scdn.co/image/cover"
        ));
        assert!(super::is_spotify_artwork_url(
            "https://cdn.spotifycdn.com/image/cover"
        ));
        assert!(!super::is_spotify_artwork_url(
            "http://i.scdn.co/image/cover"
        ));
        assert!(!super::is_spotify_artwork_url(
            "https://i.scdn.co.evil.example/image/cover"
        ));
        assert!(!super::is_spotify_artwork_url(
            "https://attacker@i.scdn.co/image/cover"
        ));
        assert!(!super::is_spotify_artwork_url(
            "https://i.scdn.co:443/image/cover"
        ));
        assert!(!super::is_spotify_artwork_url(
            "https://.scdn.co/image/cover"
        ));
    }

    #[test]
    fn keeps_overlay_visible_while_pause_hold_is_set() {
        assert!(automatic_visibility(false, false, true));
        assert!(!automatic_visibility(false, false, false));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn extracts_accent_color_from_artwork() {
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x02, 0x00, 0x00,
            0x00, 0xfd, 0xd4, 0x9a, 0x73, 0x00, 0x00, 0x00, 0x12, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0xb8, 0xe0, 0xd0, 0x70, 0xc1, 0xa1, 0x81, 0x01, 0x42, 0x01, 0x00, 0x2d,
            0x0e, 0x06, 0x41, 0x78, 0xc7, 0x32, 0x2d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
            0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        assert_eq!(super::artwork_accent(&png), Some([0xd0, 0x40, 0x80]));
    }

    #[test]
    fn parses_decimal_nvidia_temperature() {
        let telemetry = parse_gpu_telemetry("7, 49.16\n").expect("valid telemetry");
        assert_eq!(telemetry.utilization, Some(7));
        assert_eq!(telemetry.temperature, Some(49));
    }

    #[test]
    fn preserves_available_values_when_other_metric_is_unavailable() {
        let telemetry = parse_gpu_telemetry("N/A, 52.4\n").expect("temperature available");
        assert_eq!(telemetry.utilization, None);
        assert_eq!(telemetry.temperature, Some(52));
    }

    #[test]
    fn rejects_unavailable_or_out_of_range_telemetry() {
        assert!(parse_gpu_telemetry("N/A, N/A\n").is_none());
        let telemetry = parse_gpu_telemetry("101, 48\n").expect("temperature available");
        assert_eq!(telemetry.utilization, None);
        assert_eq!(telemetry.temperature, Some(48));
    }

    #[test]
    fn selects_playing_media_and_skips_paused_players() {
        let metadata = "spotify|Artist - Paused track\nfirefox|Artist - Current track\n";
        let media = select_media(metadata, |player| match player {
            "spotify" => Some(false),
            "firefox" => Some(true),
            _ => None,
        });
        assert_eq!(
            media.map(|media| media.title).as_deref(),
            Some("Artist - Current track")
        );
    }

    #[test]
    fn retains_paused_media_for_resume_controls() {
        let metadata = "spotify|Artist - Paused track\n";
        let media = select_media(metadata, |_| Some(false)).expect("paused media");
        assert!(!media.playing);
        assert_eq!(media.title, "Artist - Paused track");
    }

    #[test]
    fn reads_spotify_artwork_url_from_metadata() {
        let metadata = "spotify|Artist - Current track|https://i.scdn.co/image/cover\n";
        let media = select_media(metadata, |_| Some(true)).expect("playing media");
        assert_eq!(
            media.art_url.as_deref(),
            Some("https://i.scdn.co/image/cover")
        );
    }
}
