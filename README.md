# JPerformance Unified

One Windows app for this PC and your Linux servers. It records CPU, memory, disk, network and the apps or services behind them every 5 seconds, keeps the history for 30 days, and explains in plain words what slowed a machine down.

It replaces JPerformance (this PC) and JServer (servers over SSH). On first start it copies their history once. The old apps and their files are only read, never changed.

## What it shows

- **Overview**: every machine in one table, with CPU for the last hour, memory, the fullest disk, network and what uses the most memory.
- **Findings**: worst first, for example "Memory was over 90% full for 45 min" and which apps were behind it.
- **This PC**: live readings, history charts you can hover and pin, the apps that cost the most, running apps you can stop, one-app recordings every second, and a weekday by hour heatmap.
- **Servers**: live readings, where the memory goes (programs, ZFS cache, file cache), processes grouped by VM, container or systemd service, disks, history and findings.
- **SSH keys**: checks your keys and SSH config.

## Servers over SSH are optional

Servers are off until you turn them on in Settings. Then JPerformance reads `~/.ssh/config`, and you pick the hosts to watch. On each server it:

- runs read-only commands only (`/proc`, `/sys`, `df`, `docker ps`, `qm list`) and installs nothing,
- never asks for a password and never accepts a new or changed host key,
- does not retry a failed login on its own, so fail2ban will not ban you. Press Retry when the server is back.

## Install

Download `JPerformanceUnified.exe` from the [latest release](https://github.com/Josephivgi/JPerformance-Unified/releases/latest) and run it. No installer and no admin rights. It updates itself from GitHub releases and checks each download against the SHA-256 that GitHub publishes.

Data is stored in `%APPDATA%\JPerformance Unified` (`this-pc.db` and `servers.db`, both SQLite). Nothing leaves your PC except the update check.

A small recorder process (`JPerformanceUnified.exe --collector`) keeps recording when the window is closed. Settings > Recording > Start with Windows keeps it going after a restart.

## Command line

```
JPerformanceUnified.exe --report [hours]   findings for this PC and every server
JPerformanceUnified.exe --probe <host>     log in to one SSH host once and print what it sees
JPerformanceUnified.exe --check-update     check GitHub for a newer version
JPerformanceUnified.exe --update           install the newer version
```

## Build

Rust stable on Windows with the WebView2 runtime (part of Windows 11).

```
cargo build --release
cargo test --release
```

The window is a Tauri 2 webview. The page is plain HTML, CSS and JavaScript in `ui/` and reads everything through Tauri commands in `src/api.rs`. `JPerformanceUnified.exe --smoke` opens every view in a hidden window and writes `smoke.json` to the data folder.

## Release

Bump the version in `Cargo.toml` and `tauri.conf.json`, add `release-notes/vX.Y.Z.md`, then push the tag `vX.Y.Z`. GitHub Actions builds, tests and publishes the release.
