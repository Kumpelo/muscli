# Compact desktop widget: IPC contract

Status: validated for implementation. The optional MVP lives in `desktop/quickshell`.

The control socket accepts existing commands and a read-only `status v1` request. MPRIS
exposes current metadata and transport controls but not muscli's complete queue.
A separate Quickshell process should display the running player's state without
opening the database, loading audio libraries, or launching another player.

## Proposed boundary

The per-user control socket now supports a versioned, read-only snapshot
request. Retain the Unix runtime directory's 0700 and socket's 0600 permissions.
Use one bounded request per connection, explicit framing, maximum request and
response sizes, timeouts and a bounded client count. A stalled client must not
block audio, the TUI, other clients, or application shutdown. Do not accept paths,
shell commands, or arbitrary database queries from clients.

Requests are capped at 256 bytes and responses at 64 KiB. At most eight
nonblocking clients are serviced, each with a one-second server deadline. The
CLI has bounded connection and response deadlines.

The main loop publishes an immutable snapshot; the socket worker serves the latest
copy without querying SQLite or awaiting the main loop. Snapshot fields:

- schema version, queue revision and playback availability;
- logical current queue item and currently presented/audible incoming item;
- title, individual artist, local cover URI, duration, position and volume;
- playback status, active MIX state and playback context;
- up to ten upcoming entries, with occurrence indices unique within a queue revision, including duplicates.

The `muscli remote status --json` client exits promptly when no player is
running and never starts playback. The widget queries once per second while
visible; it interpolates progress locally and stops polling when hidden.
Transport controls use MPRIS and target muscli specifically; volume uses MPRIS's
existing semantics. Queue display uses the snapshot, never MPRIS TrackList guesses.

## Optional frontend

Ship Quickshell QML and a launcher under an optional desktop integration directory,
with no Cargo dependency or installation requirement. Configuration chooses screen,
corner, margins and width. Prefer a small Wayland layer surface, with an ordinary
window fallback if the chosen Quickshell version requires it. Cover, metadata,
progress, transport, volume and a short upcoming list share a neutral theme.

Keep the previous cover until its replacement loads. Show a disconnected state
when the player exits. Missing Quickshell produces an actionable launcher message,
without changing the player's behavior. Do not alter Hyprland config or install
packages as a side effect of starting the player.

## Validation contract

Verify snapshot serialization, duplicate queue entries, Play Next priority,
concurrent/slow clients, oversized requests, disconnects, player shutdown and socket
permissions. Validate the actual QML with an installed Quickshell, then exercise
controls, second-monitor placement and reconnect behavior in Hyprland. Inspect the
supported Quickshell API version before implementation. A design review or a QML
file alone does not establish that the widget works.

## Frontend version

The MVP targets Quickshell 0.3.2. Its panel uses the documented
[PanelWindow anchors and screen selection](https://quickshell.org/docs/v0.3.2/types/Quickshell/PanelWindow/)
and [MprisPlayer capabilities and methods](https://quickshell.org/docs/v0.3.2/types/Quickshell.Services.Mpris/MprisPlayer/).
The optional `tests/widget.rs` smoke test exercises these against a private D-Bus
session and a synthetic control server, without constructing an audio backend.
