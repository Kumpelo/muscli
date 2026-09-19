# muscli

> [Documentación en español](docs/README.es.md)

muscli is a fast, local-first music player with a Spotify-like terminal UI. It
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
muscli library write-gain --yes
muscli doctor
muscli playlist export NAME playlist.m3u8
muscli playlist import playlist.m3u8
muscli summary --days 30
```

`muscli summary` reports what you have been listening to from the local index.
Like everything else here, it sends nothing anywhere.

An eight-band equaliser is configured with `equalizer` in `config.toml`, as
gains in decibels from low to high; all zero means the filter is not installed
at all.

FLAC, MP3, M4A/AAC/ALAC, Ogg, Opus, WAV, AIFF, WavPack and Monkey's Audio are
indexed; narrow the list with `audio_extensions` in `config.toml`. Tags and
embedded or external artwork are read without modifying the media. The SQLite index, configuration, and 64 MiB size-bounded
cover cache use the platform-standard application directories. A warm start
loads the index immediately while scans and ReplayGain analysis continue in
the background.

Features include genres, artist releases, resume/history, editable smart
playlists, fuzzy search, a context menu, saved/reorderable queues, local
ReplayGain analysis, settings, a keybinding reference, and compact mode.

## Discord Rich Presence

```text
muscli setup discord --large-image peter
```

MusCLI ships with its official Discord application ID, so users do not need to
create or configure a Discord application. No bot token or OAuth is needed. The
local Discord/Vesktop IPC displays title, artist, album and progress. Assets named `peter_metal`, `peter_dj`, and
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

`muscli library write-gain` is the only command that modifies your audio files.
It writes the cached loudness analysis into their ReplayGain tags so other
players can use it, and requires `--yes`. Everything else muscli does reads
your files and never writes to them.

## Lyrics

Lyrics are never picked up automatically. An `.lrc` file sitting next to a
track is left alone; lyrics live in their own directory and only get there when
you put them there:

```bash
muscli lyrics import song.lrc --artist ARTIST --title TITLE
muscli lyrics import song.lrc --track TRACK_ID
muscli lyrics where
```

Naming by artist and title survives the audio file being moved; naming by track
id does not, because the id is derived from the path. Timed lines follow
playback; a file without timestamps is shown as plain text.

## Key bindings

`muscli keys` lists the bindings in effect. To change them, write
`keybindings.toml` in the configuration directory, naming actions as that
command prints them:

```toml
quit = ["ctrl+q"]
play_pause = ["space", "p"]
```

Listing an action replaces its default keys rather than adding to them. An
entry muscli cannot read is reported and skipped; it never stops muscli
starting.

## Themes

The interface ships with a light palette, and that is the default on Linux and
on Windows alike. Change it from the settings view (`,`), with the left and
right arrows on the **Theme** row, or set `theme` in `config.toml`:

| `theme` | |
|---|---|
| `light` | white background, the default |
| `dark` | the terminal's own palette |
| `high-contrast` | black on white, for bright rooms and projectors |
| `nord`, `gruvbox`, `solarized-light` | fixed palettes, identical everywhere |
| `system` | follow the desktop: the current Omarchy theme on Linux, light elsewhere |

With `system` on Linux the palette follows Omarchy live: switching theme there
repaints muscli without restarting it. `muscli --theme dark` tries a palette
for one run without changing the configured one.

## Language

The interface follows the system locale when it recognises it, and is English
otherwise. Change it from the settings view, set `language = "en"` or
`language = "es"` in `config.toml`, or pass `muscli --lang es` for one run.
Changing it in the settings view takes effect immediately, without restarting.

## Settings

Press `,` for the settings view. Left and right adjust a value, Space and Enter
toggle one or run the row. It covers the theme and language, compact mode,
covers, volume step, gapless playback, ReplayGain and its mode and target,
every equaliser band, resume and history, removable-drive detection, scan
threads, the cover-cache budget and Discord Rich Presence, and it can start a
rescan or a ReplayGain analysis. Every change is written to `config.toml` as
it is made; that file remains editable by hand.

muscli is MIT licensed. Contributions are welcome; see
[CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
