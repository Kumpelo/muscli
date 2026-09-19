# muscli

> [Documentación en español](docs/README.es.md)

muscli is a fast, local-first FLAC player with a Spotify-like terminal UI. It
indexes removable drives and local folders, keeps playlists usable while a
drive is offline, and delegates gapless audio playback to mpv, which opens the
next track before the current one ends. It has no
account, streaming service, telemetry, or resident daemon.

## Platforms

- Linux: MPRIS, removable media discovery, Discord Rich Presence, and an
  optional Omarchy integration.
- Windows 11 x64: the same TUI, removable-drive discovery, Windows media
  controls (SMTC), media keys, and Discord Rich Presence.

The Windows installer bundles verified mpv and FFmpeg executables. See
[third-party notices](THIRD_PARTY_NOTICES.md) for exact builds, checksums,
licenses, and corresponding source. Early installers are unsigned, so Windows
SmartScreen may ask for confirmation.

## Install

### Linux / Omarchy

```bash
omarchy pkg add mpv ffmpeg
cargo install --root "$HOME/.local" --path .
muscli setup omarchy
muscli
```

Omarchy setup only edits user-owned configuration, creates timestamped
backups, validates Hyprland, enables the official media widget, and installs:

- `SUPER+SHIFT+ALT+M`: open the large floating muscli window.
- `Shift+Volume Up/Down`: change only muscli's volume.

Undo intact managed blocks with `muscli setup omarchy --undo`.

### Windows 11

Download `muscli-vX.Y.Z-windows-x86_64-setup.exe` from Releases and run it.
The Start menu contains muscli and `muscli Doctor`; the installer also
registers `muscli.exe` in Windows App Paths. Windows Terminal is recommended
for the best image support.

## Library and playback

```text
muscli library add PATH
muscli library remove PATH
muscli library list
muscli library rescan
muscli library prune
muscli library analyze-gain
muscli doctor
```

Only `.flac` files are indexed. Tags and embedded/external artwork are read
without modifying the media. The SQLite index, configuration, and 64 MiB size-bounded
cover cache use the platform-standard application directories. A warm start
loads the index immediately while scans and ReplayGain analysis continue in
the background.

Features include genres, artist releases, resume/history, editable smart
playlists, fuzzy search, a context menu, saved/reorderable queues, local
ReplayGain analysis, settings, a keybinding reference, and compact mode.

## Discord Rich Presence

```text
muscli setup discord APPLICATION_ID --large-image peter
```

No bot token or OAuth is needed. The local Discord/Vesktop IPC displays title,
artist, album and progress. Assets named `peter_metal`, `peter_dj`, and
`daft_punk` are selected for their matching genres/artist; otherwise the
configured fallback is used. Local cover art is never uploaded.

## Main controls

| Key | Action |
|---|---|
| Arrows or `hjkl` | Navigate lists and album grids |
| Enter / Esc | Open or play / go back |
| Space, `n`, `p` | Pause, next, previous |
| `/`, `x`, `f`, `a` | Search, context menu, favorite, enqueue |
| `s`, `r`, `+`, `-` | Shuffle, repeat, volume |
| `m`, `,`, `?` | Compact mode, settings, help |
| `Shift+J/K`, `d` | Reorder or remove queue item |
| `C`, `S`, `L` | Clear, save, or load a queue |
| `q` | Save state and quit |

Run `muscli remote volume up|down|set PERCENT` or
`muscli remote mute-toggle` from another process. If muscli is closed these
commands exit silently and never change the system volume.

## Development

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

CI runs these checks on Linux and Windows. Tags matching `v*` create draft
GitHub releases with Linux and Windows artifacts, SHA-256 checksums, and a
CycloneDX SBOM. The intended first prerelease is `v0.2.0-beta.1`.

## Language

The interface is English by default and follows the system locale when it
recognises it. Override with `language = "en"` or `language = "es"` in
`config.toml`, or `muscli --lang es` for one run.

muscli is MIT licensed. Contributions are welcome; see
[CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
