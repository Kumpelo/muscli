# Changelog

All notable user-visible changes are documented here.

## Unreleased

## 0.2.0-beta.5 - 2026-10-18

- Fixed duplicate available/offline albums when scanning the same files through
  a drive root and its music subfolder. Repeated scans preserve track identities,
  and matching existing duplicate paths are reconciled without editing music.
- Preserved favorites, playlists, queues, listening history, statistics and valid
  ReplayGain/MIX analyses during reconciliation, including in-flight playback
  accounting that still references an older track ID.
- Stabilized source identities across temporary volume lookup failures and
  Windows drive-path representations. Incomplete scans and device changes no
  longer prune a disconnected source as though its files had been deleted.
- Serialized scan passes, made library snapshots consistent, and ignored access
  and timestamp-only watcher events to avoid unnecessary library reloads.

## 0.2.0-beta.4 - 2026-10-18

- Added native Ogg Opus decoding with encoder delay and padding trimming,
  header gain, seeking with preroll, and multichannel channel ordering.
  Invalid headers, excessive internal streams and malformed packets are bounded
  or rejected without interrupting playback of supported files.
- Added generic discovery of conventional music folders on mounted USB drives
  and SD cards, with hot-plug watching, stable volume identities when available,
  and preservation of favorites, playlists and listening history on reconnect.
  Dedicated music folders avoid Ventoy ISO trees, overlapping sources and
  unsafe symbolic links.
- Bounded artwork reads and image decoding, sanitized untrusted tag and playlist
  display text, and limited ReplayGain subprocess runtime and captured output.
- Improved native gapless handovers across 44.1 and 48 kHz with resampling;
  bit-perfect playback retains matching-format requirements.
- Cached failed ReplayGain analyses and added retries for failed files without
  repeating successful work; clarified progress and MIX requirements.
- Added portable terminal symbols and volume shortcuts, including Shift and
  mouse-wheel controls, and fixed theme backgrounds around covers and dialogs.
- Fixed missing-instance remote commands on Windows.

## 0.2.0-beta.3 - 2026-10-17

- Added native live MIX transitions, persistent incremental BPM and energy
  analysis, transition previews, and configurable transition effects.
- Added album sessions, conservative compilation grouping and album ReplayGain
  based on actual group membership; preserved distinct releases.
- Added portable synced lyrics, configurable pause fade, an optional Quickshell
  widget and verified updates from official releases.
- Improved Discord playback synchronization and canonical album covers, persisted
  resolved cover URLs and cleared presence while paused.
- Fixed queue prefetch after edits, seek and pause behavior, incremental
  ReplayGain scheduling, integer PCM clipping and ALSA callback buffering.

## 0.2.0-beta.2 - 2026-09-25

- Added an optional native audio backend with ReplayGain, an eight-band
  equalizer, a limiter, high-quality resampling, dither, bit-perfect mode, and
  gapless playback between tracks with matching stream formats.
- Added automatic fallback to mpv for formats the native decoder cannot read.
- Added seek controls, a decibel volume scale, audio-device selection, and
  native-backend diagnostics in `muscli doctor`.
- Added artist and genre group enqueueing, and made shuffle update the
  unplayed part of an active queue without skipping the current track.
- Discord Rich Presence now uses muscli's built-in application ID; users no
  longer need to create or configure their own Discord application.
- Added a selectable colour theme on Linux and Windows alike: `light` (the new
  default, on a white background), `dark`, `high-contrast`, `nord`, `gruvbox`,
  `solarized-light`, and `system` to follow the Omarchy palette on Linux. It is
  chosen from the settings view, `theme` in `config.toml`, or `--theme` for a
  single run. Linux installs that were following the Omarchy palette get the
  light default and restore the old behaviour with `theme = "system"`.
- Added the interface language to the settings view; changing it applies
  immediately, without restarting.
- Added the settings previously reachable only by editing `config.toml`: gapless
  playback, scan threads, and every band of the equaliser, with a row to flatten
  it again.
- Reworked cover rendering and cache versioning to avoid stale artwork and
  unnecessary terminal redraws.
- Fixed saved volume resets, playback between different sample rates, context
  menu redraws, terminal restoration after hangup, and queue edge cases.

## 0.2.0-beta.1

- Added Windows 11 x64 support with named-pipe IPC, removable-drive discovery,
  SMTC media controls, Discord Rich Presence, and an Inno Setup installer.
- Added Linux and Windows CI plus draft multi-platform GitHub releases,
  checksums, SBOM generation, and public-repository attestations.
- Added genres, history/resume, smart playlists, fuzzy search, context actions,
  saved queues, ReplayGain, settings, help, compact mode, and remote volume.
- Added dynamic Omarchy theme colors and safer idempotent desktop integration.
- Fixed stale library rows, dangling/oversized cover cache entries, album-grid
  navigation and redraws, artist navigation, and session-position restore.
- Fixed Windows CI and release packaging after the cross-platform port.
- Improved removable-drive hot-plug detection while muscli is already running.
- Keyed cached covers by artwork content so replaced or same-named album art
  cannot reuse stale cached images.
