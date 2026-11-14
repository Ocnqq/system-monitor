#!/usr/bin/env bash
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
bin_dir="${XDG_BIN_HOME:-$HOME/.local/bin}"
app_path="$bin_dir/system-monitor-overlay"
launch_path="$bin_dir/system-monitor-overlay-launch"
desktop_path="$data_home/applications/io.github.systemmonitor.overlay.desktop"
launcher_path="$config_home/autostart/system-monitor-overlay.desktop"

cargo build --release --manifest-path "$project_dir/Cargo.toml"
install -Dm755 "$project_dir/target/release/system-monitor-overlay" "$app_path"
install -Dm755 "$project_dir/launch.sh" "$launch_path"
install -Dm644 "$project_dir/sysmon.fish" "$config_home/fish/functions/sysmon.fish"
install -Dm644 "$project_dir/io.github.systemmonitor.overlay.desktop" "$desktop_path"
sed -i "s|^Exec=.*|Exec=\"$launch_path\"|" "$desktop_path"

mkdir -p "$config_home/autostart"
cat > "$launcher_path" <<EOF
[Desktop Entry]
Type=Application
Name=System Monitor Overlay
Comment=Show system stats while a Steam game is running
Exec="$launch_path" --background
Terminal=false
X-KDE-autostart-after=panel
EOF

printf 'Installed %s\n' "$app_path"
printf 'Fish command: sysmon (toggle the overlay)\n'
printf 'Run sysmon to toggle the overlay or log out and back in for autostart.\n'
