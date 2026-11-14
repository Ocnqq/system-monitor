function sysmon --description 'Toggle the system monitor overlay'
    set -l monitor_bin "$HOME/.local/bin/system-monitor-overlay"
    if set -q XDG_BIN_HOME
        set monitor_bin "$XDG_BIN_HOME/system-monitor-overlay"
    end
    if not test -x "$monitor_bin"
        echo "system-monitor-overlay is not installed at $monitor_bin" >&2
        return 1
    end

    "$monitor_bin" --toggle
end
