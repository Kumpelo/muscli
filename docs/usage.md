# User guide

[Back to README](../README.md)

## Installation

Linux requires ALSA, mpv and FFmpeg. To build on Debian or Ubuntu, also install
`libasound2-dev`; on Arch the development headers are included in `alsa-lib`.
Rust 1.90 or newer is required to build from source.

For a downloaded Linux archive, replace `X.Y.Z` below with its version:

```bash
tar -xzf muscli-vX.Y.Z-linux-x86_64.tar.gz
install -Dm755 muscli-vX.Y.Z-linux-x86_64/muscli "$HOME/.local/bin/muscli"
muscli --version
```

Ensure `~/.local/bin` is in `PATH`. On Windows, use the installer from
[Releases](https://github.com/Kumpelo/muscli/releases); it includes mpv and FFmpeg.
The [third-party notices](../THIRD_PARTY_NOTICES.md) list their builds and licenses.

For Omarchy:

```bash
omarchy pkg add alsa-lib mpv ffmpeg
muscli setup omarchy
```

Setup creates backups and enables Omarchy's media widget.
`Super+Shift+Alt+M` opens muscli; `Shift+Volume Up/Down` changes muscli's volume.
Use `muscli setup omarchy --undo` to remove intact managed configuration blocks.
The separate [Quickshell widget](playback-and-widget.es.md#widget-para-omarchy)
requires its own launcher.

## Updates

```bash
muscli update --check  # check without installing
muscli update          # download, verify and install
```

Settings → Updates offers **Off**, **Notify (development)** and **Automatic**.
Published release builds default to Automatic; local Cargo builds default to
Notify. The mode can also be set with `update_mode = "off"`, `"notify"` or
`"automatic"` in `config.toml`. Mode changes take effect at the next startup;
changing the mode cancels an update still being prepared.

The player checks GitHub once per day in the background. Automatic mode installs
Linux updates for the next launch; playback continues in the current process.
On Windows, it prepares the bundled installer and runs it after you close the
player. **Check and install update** in Settings installs an update on request,
even when automatic checks are off.

Downloads must match the official release's SHA-256 checksum. Stable versions
stay on the stable channel; beta versions can receive newer betas and stable
releases. Older versions and unpublished drafts are ignored. Linux keeps the
previous executable as `muscli.previous` beside `muscli`; you can restore it
with the player closed. Music, lyrics, playlists and configuration are retained.

The updater supports x86_64 user installations in `~/.local/bin`, Cargo's `bin`
directory, and Windows' default per-user installation folder. Package-managed
installations must use their package manager. Executables inside a Cargo
`target` directory must be rebuilt with Cargo.

## Larger TUI window

On Linux with Kitty installed, open the full player with a size-12 font:

```bash
muscli --large
```

This opens a maximized terminal window; Omarchy's muscli window rule controls
its size when the integration is installed. Other terminal windows keep their
usual font size. You can also pass `--lang es` or `--theme nord`.

## Library and playlists

```bash
muscli library add /path/to/Music
muscli library list
muscli library rescan
muscli playlist export NAME playlist.m3u8
muscli playlist import playlist.m3u8
muscli summary --days 30
```

The local index retains playlists when a drive is disconnected. Scans read tags
and artwork without changing the audio. Supported formats include FLAC, MP3,
M4A/AAC/ALAC, Ogg, Opus, WAV, AIFF, WavPack and Monkey's Audio.

Use `muscli library --help` for source removal, pruning and resume-position
commands. `muscli summary` shows listening history stored on this computer.

## Audio

mpv is the default backend. Select the native backend in Settings or set
`audio_backend = "native"` in `config.toml`. `muscli devices` lists native output
devices; `audio_device` selects one by name.

The native path provides ReplayGain, an eight-band equalizer, limiting and
optional bit-perfect output. Opus, WavPack and Monkey's Audio fall back to mpv.
Gapless playback requires compatible stream formats; a sample-rate change may
open a new output stream. Disconnecting a device may require restarting playback.

Bit-perfect output bypasses muscli's volume, ReplayGain and equalizer. A shared
system audio service may still mix or resample the output afterward.

ReplayGain analysis is cached locally and reused for unchanged files:

```bash
muscli library analyze-gain
muscli library write-gain --yes
```

`write-gain --yes` writes the cached gain into the audio's ReplayGain tags.
Ordinary scans and playback leave the audio unchanged.

Pause fade lowers the volume before pausing. Settings allows 0–2000 ms, with
300 ms as the default. Resume and track changes cancel a pending fade;
bit-perfect playback pauses immediately.

## Queues and Live MIX

Starting an album preserves disc/track order and suspends MIX and shuffle.
Manually adding, removing or moving tracks changes it into a song queue and
restores those preferences. Shuffle affects upcoming tracks; Play Next keeps
its requested position.

Live MIX requires the native backend, compatible track/output formats, gapless
prefetch and bit-perfect output disabled. Settings offers Off, Classic, Smart
and AutoDJ. Smart uses cached tempo/energy estimates; AutoDJ can append a
compatible library track after the selected queue runs out.

```bash
muscli library analyze-mix
muscli library mix-status
```

Use `analyze-mix --force` to replace cached results. Automatic analysis can be
disabled with `mix_analysis_enabled = false`. The estimates do not provide
beat alignment or harmonic matching.

The separate `muscli mix demo` command creates an offline transition preview.
Run `muscli mix demo --help` for input, output and effect options. See the
[Spanish playback guide](playback-and-widget.es.md) for more about Live MIX.

## Lyrics

In a song's menu (`x`), choose **Download lyrics (LRCLIB)**. This explicit request
sends the track's title, artist, album and duration to LRCLIB. Synchronized
lyrics are not available for every recording.

```bash
muscli lyrics fetch --track TRACK_ID
muscli lyrics fetch --track TRACK_ID --beside
muscli lyrics export --track TRACK_ID
muscli lyrics import song.lrc --artist ARTIST --title TITLE
muscli lyrics where
```

Downloads stay in the local cache and work offline. **Save lyrics beside audio**
exports `song.lrc` beside the audio without changing it or replacing an existing
file. Copy both files to another computer, USB, SD card or a DAP that supports LRC.
The destination player determines how synchronized lyrics are displayed.

muscli reads a sidecar LRC before its cache. Timed lines follow playback in the
Lyrics view; plain text is shown without synchronization. An import named by
artist/title survives an audio path change; one named by track ID is tied to
that path.

## Settings and controls

Press `,` for Settings. Left/right adjusts values; Space or Enter toggles a
setting or runs its action. Changes are saved to `config.toml`. Application
files use the platform's standard configuration, data and cache directories.

The default `system` theme follows Omarchy on Linux and uses Steel Blue when
no desktop palette is available. You can also select `light`, `dark`,
`high-contrast`, `nord`, `gruvbox` or `solarized-light`:

```bash
muscli --theme nord
muscli --lang es
muscli keys
```

`--theme` and `--lang` apply to one run. The language setting accepts `auto`,
`en` or `es`. Put key overrides in `keybindings.toml` in the configuration
directory, using the action names printed by `muscli keys`:

```toml
quit = ["ctrl+q"]
play_pause = ["space"]
```

Each entry replaces that action's default keys. Queue controls include
`Shift+J/K` to move an item, `d` to remove it, and `C`, `S`, `L` to clear, save
and load queues. `m` toggles compact mode.

External volume commands affect muscli rather than the system mixer:

```bash
muscli remote volume up
muscli remote volume down
muscli remote volume set 60
muscli remote mute-toggle
```

## Discord

Enable Rich Presence and cover images in Settings. Discord shows the literal
song title and artist, the album in the image tooltip and playback progress.
Pausing or stopping clears the activity.

muscli uses its bundled Discord application ID and local Discord/Vesktop IPC;
no bot token is required. Cover lookup uses MusicBrainz and public Cover Art
Archive URLs. Resolved URLs are cached; local artwork is not uploaded.

## Troubleshooting

Run `muscli doctor`. When reporting a problem, include the muscli version,
platform, reproduction steps and relevant diagnostic output. Development checks
are listed in [Contributing](../CONTRIBUTING.md).
