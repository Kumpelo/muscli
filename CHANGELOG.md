# Changelog

All notable user-visible changes are documented here.

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
