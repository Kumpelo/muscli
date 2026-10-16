//! Optional actual Quickshell/MPRIS smoke test on a private D-Bus session.
//! No audio backend is created and no user state is read or changed.
#![cfg(unix)]
mod common;
use muscli::{
    control::ControlServer,
    model::{PlaybackContext, PlaybackState, PlaybackStatus, PlayerAction, RepeatMode, Track},
    mpris::MprisBridge,
    widget::{WidgetSnapshot, WidgetTrack},
};
use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct WidgetProcess(Child);
impl Drop for WidgetProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "requires Quickshell 0.3.2, Wayland and a private dbus-run-session; no audio hardware"]
fn actual_widget_controls_and_reconnect_use_the_running_session_only() {
    assert_eq!(
        std::env::var("MUSCLI_WIDGET_TEST_BUS").as_deref(),
        Ok("1"),
        "run inside a private dbus-run-session"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let socket = root.join("muscli/control.sock");
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let cover = root.join("cover.png");
        fs::write(&cover, common::photo_bytes(256, 256, 90)).unwrap();
        let track = Track {
            id: "synthetic".into(), source_id: "test".into(), relative_path: "never-play.flac".into(), path: root.join("never-play.flac"),
            title: "Continuous light".into(), artist: "Synthetic ensemble".into(), album_artist: "Synthetic ensemble".into(), album: "Test release".into(),
            release_id: String::new(), compilation: false, genre: String::new(), year: Some(2026), disc_number: 1, track_number: 1,
            duration_ms: 180_000, cover_path: Some(cover), available: false, favorite: false,
        };
        let state = PlaybackState { status: PlaybackStatus::Playing, position_ms: 42_000, duration_ms: track.duration_ms, volume: 0.6 };
        let mut snapshot = WidgetSnapshot::new(&state, PlaybackContext::Songs);
        snapshot.current = Some(WidgetTrack::from_track(&track, 0));
        snapshot.logical_current = snapshot.current.clone();
        snapshot.upcoming = (1..=4).map(|n| { let mut row = WidgetTrack::from_track(&track, n); row.title = format!("Upcoming piece {n}"); row }).collect();
        let (server, _commands) = ControlServer::start(&socket).unwrap();
        server.publish(&snapshot).unwrap();
        let (sender, mut actions) = tokio::sync::mpsc::unbounded_channel();
        let mpris = MprisBridge::new(sender).await.unwrap();
        mpris.sync(Some(&track), &state, false, RepeatMode::Off, true, true).await.unwrap();
        mpris.sync_position(&state).await.unwrap();
        let original = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("desktop/quickshell/shell.qml")).unwrap();
        let smoke = r#"
    property int smokeStage: 0
    Timer {
        interval: 200; running: true; repeat: true
        onTriggered: {
            if (root.smokeStage === 0 && root.player && root.current?.title === "Continuous light" && root.readyCover !== "") {
                if (!previousButton.enabled || !playButton.enabled || !nextButton.enabled || !volumeSlider.enabled) throw new Error("Disabled controls")
                previousButton.clicked(); playButton.clicked(); nextButton.clicked()
                volumeSlider.value = 0.42; volumeSlider.moved()
                console.log("WIDGET_READY", root.current.title, root.snapshot.upcoming.length)
                root.smokeStage = 1
            } else if (root.smokeStage === 1 && !root.connected) {
                console.log("WIDGET_DISCONNECTED"); root.smokeStage = 2
            } else if (root.smokeStage === 2 && root.current?.title === "Reconnected") {
                console.log("WIDGET_RECONNECTED"); root.smokeStage = 3
            }
        }
    }
"#;
        let qml = root.join("shell.qml");
        fs::write(&qml, original.replacen("    id: root", &format!("    id: root\n{smoke}"), 1)).unwrap();
        let log = root.join("widget.log");
        let wayland = std::env::var_os("WAYLAND_DISPLAY").map(PathBuf::from).map(|path| {
            if path.is_absolute() { path } else { PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap()).join(path) }
        }).unwrap();
        let child = Command::new("qs").args(["--path"]).arg(&qml)
            .env("WAYLAND_DISPLAY", wayland)
            .env("XDG_RUNTIME_DIR", root).env("MUSCLI_WIDGET_BINARY", env!("CARGO_BIN_EXE_muscli"))
            .env("MUSCLI_WIDGET_CORNER", std::env::var("MUSCLI_WIDGET_TEST_CORNER").unwrap_or_else(|_| "bottom-right".into())).env("MUSCLI_WIDGET_SCREEN", std::env::var("MUSCLI_WIDGET_TEST_SCREEN").unwrap_or_default())
            .stdout(Stdio::from(fs::File::create(&log).unwrap())).stderr(Stdio::from(fs::OpenOptions::new().append(true).open(&log).unwrap()))
            .spawn().unwrap();
        let mut child = WidgetProcess(child);
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut received = Vec::new();
        while received.len() < 4 && Instant::now() < deadline {
            if let Ok(Some(action)) = tokio::time::timeout(Duration::from_millis(100), actions.recv()).await { received.push(action); }
            assert!(child.0.try_wait().unwrap().is_none(), "{}", fs::read_to_string(&log).unwrap());
        }
        let output = fs::read_to_string(&log).unwrap();
        assert_eq!(received.len(), 4, "{output}");
        for expected in [PlayerAction::Previous, PlayerAction::Pause, PlayerAction::Next, PlayerAction::SetVolume(0.42)] {
            assert!(received.contains(&expected), "missing {expected:?}: {received:?}; {output}");
        }
        assert!(output.contains("WIDGET_READY"), "{output}");
        if let Ok(ms) = std::env::var("MUSCLI_WIDGET_TEST_HOLD_MS") {
            println!("Widget screenshot window is ready; private fixture: {}", root.display());
            tokio::time::sleep(Duration::from_millis(ms.parse().unwrap())).await;
        }
        drop(server);
        tokio::time::sleep(Duration::from_millis(2200)).await;
        let (server, _) = ControlServer::start(&socket).unwrap();
        snapshot.current.as_mut().unwrap().title = "Reconnected".into();
        server.publish(&snapshot).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = fs::read_to_string(&log).unwrap();
            if output.contains("WIDGET_RECONNECTED") { assert!(output.contains("WIDGET_DISCONNECTED")); break; }
            assert!(Instant::now() < deadline, "{output}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let output = fs::read_to_string(&log).unwrap();
        assert!(!output.contains("ReferenceError") && !output.contains("TypeError") && !output.contains("Failed to load configuration"), "{output}");
    });
}

#[test]
fn a_status_query_without_a_player_does_not_initialize_user_state() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let output = Command::new(env!("CARGO_BIN_EXE_muscli"))
        .args(["remote", "status", "--json"])
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read_dir(root).unwrap().count(),
        0,
        "a read-only query must not create state"
    );
}
