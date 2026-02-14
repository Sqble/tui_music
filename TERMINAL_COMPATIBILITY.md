# Windows Terminal Tray Compatibility

If TuneTUI is launched inside the new Windows Terminal, pressing `t` may only minimize the window instead of fully hiding it to the system tray.

Enable this Windows Terminal setting so tray behavior works as expected:

1. Open Windows Terminal.
2. Press `Ctrl + ,` to open Settings.
3. Go to **Appearance**.
4. Turn on **Hide terminal in the notification area when it is minimized**.
5. Save settings.

After enabling this option, pressing `t` in TuneTUI should hide the terminal window to the notification area (system tray).
