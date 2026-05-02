# Tray Compatibility

## Windows Terminal

If TuneTUI is launched inside the new Windows Terminal, pressing `t` may only minimize the window instead of fully hiding it to the system tray.

Enable this Windows Terminal setting so tray behavior works as expected:

1. Open Windows Terminal.
2. Press `Ctrl + ,` to open Settings.
3. Go to **Appearance**.
4. Turn on **Hide terminal in the notification area when it is minimized**.
5. Save settings.

After enabling this option, pressing `t` in TuneTUI should hide the terminal window to the notification area (system tray).

## Linux Tray Support

Linux tray support uses the StatusNotifierItem/AppIndicator protocol. Pressing `t` creates a tray icon and keeps playback running while TuneTUI is collapsed.

Supported environments depend on a tray host being available:

- KDE Plasma: supported through Plasma's built-in tray.
- Hyprland/Omarchy: supported when Waybar or another bar exposes a tray/StatusNotifier host. TuneTUI also moves the active terminal to a Hyprland special workspace named `tunetui` and restores it from the tray icon.
- GNOME: install and enable an AppIndicator/KStatusNotifierItem extension.
- Minimal window managers: install and run a tray host, otherwise TuneTUI will report that no Linux tray host was found.

On non-Hyprland Linux desktops, TuneTUI asks the terminal to minimize using standard terminal window-control escape sequences. Terminal and compositor support varies, but the tray icon remains available for restore when the tray host accepts it.

# SSH Audio Support (PulseAudio over SSH)

If you run TuneTUI on a remote Linux VPS over SSH, audio will not play by default because most servers have no physical sound device.
You can forward audio from the VPS to your local Windows machine using PulseAudio.

One-time setup (local Windows machine)

Make sure PulseAudio is installed and running on your Windows system.

Connect to VPS with audio forwarding

Use reverse port forwarding when connecting:

ssh -R 4713:localhost:4713 user@your-server-ip

This exposes your local PulseAudio server to the remote VPS.

Configure PulseAudio on the VPS (per session)

After SSH login, run:

export PULSE_SERVER=127.0.0.1

Test audio

Run:

speaker-test

If configured correctly, sound will play through your local computer speakers even though TuneTUI is running on the remote VPS.
