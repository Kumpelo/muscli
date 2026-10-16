import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import Quickshell.Services.Mpris
import Quickshell.Wayland

ShellRoot {
    id: root
    component TransportButton: Button {
        implicitWidth: 58; implicitHeight: 38
        opacity: enabled ? 1 : 0.35
        background: Rectangle { radius: 10; color: parent.down ? "#43574a" : parent.hovered ? "#34443a" : "#26332c" }
        contentItem: Text { text: parent.text; color: root.foreground; horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter; font.pixelSize: 16 }
    }
    property var snapshot: ({ available: false, upcoming: [] })
    property double receivedAt: 0
    property double now: Date.now()
    readonly property var player: Mpris.players.values.find(p => p.dbusName === "org.mpris.MediaPlayer2.muscli") ?? null
    readonly property bool connected: snapshot.available === true && now - receivedAt < 3500
    readonly property var current: connected ? snapshot.current : null
    readonly property string requestedCover: current?.cover_uri ?? ""
    property string readyCover: ""
    readonly property real edgeMargin: Number(Quickshell.env("MUSCLI_WIDGET_MARGIN") || 18)
    readonly property string corner: Quickshell.env("MUSCLI_WIDGET_CORNER") || "bottom-right"
    readonly property string screenName: Quickshell.env("MUSCLI_WIDGET_SCREEN") || ""
    readonly property real duration: connected ? (snapshot.duration_ms || 0) : 0
    readonly property real position: connected ? Math.min(duration, (snapshot.position_ms || 0)
        + (snapshot.status === "playing" ? Math.min(now - receivedAt, 1500) : 0)) : 0
    readonly property color background: "#151a1c"
    readonly property color foreground: "#edf0ed"
    readonly property color muted: "#9daaa7"
    readonly property color accent: "#aecdb8"

    function acceptSnapshot(text) {
        try {
            const value = JSON.parse(text)
            if (value.schema_version !== 1) throw new Error("Unsupported status version")
            snapshot = value
            receivedAt = Date.now()
        } catch (_) { snapshot = ({ available: false, upcoming: [] }) }
    }
    function timeLabel(ms) {
        const seconds = Math.max(0, Math.floor(ms / 1000))
        return Math.floor(seconds / 60) + ":" + String(seconds % 60).padStart(2, "0")
    }

    Timer {
        interval: 1000; running: panel.visible; repeat: true; triggeredOnStart: true
        onTriggered: if (!statusProcess.running) statusProcess.running = true
    }
    Timer { interval: 250; running: panel.visible; repeat: true; onTriggered: root.now = Date.now() }
    Process {
        id: statusProcess
        command: [Quickshell.env("MUSCLI_WIDGET_BINARY") || "muscli", "remote", "status", "--json"]
        stdout: StdioCollector { onStreamFinished: root.acceptSnapshot(text) }
        stderr: StdioCollector { }
        onExited: (exitCode, exitStatus) => { if (exitCode !== 0) root.snapshot = ({ available: false, upcoming: [] }) }
    }
    // The loader is invisible. The visible image keeps its old source until ready.
    Image {
        id: coverLoader
        source: root.requestedCover; asynchronous: true; visible: false
        sourceSize: Qt.size(208, 208)
        onStatusChanged: if (status === Image.Ready) root.readyCover = source.toString()
    }

    PanelWindow {
        id: panel
        WlrLayershell.namespace: "muscli-widget"
        screen: Quickshell.screens.find(s => s.name === root.screenName) ?? Quickshell.screens[0]
        anchors.top: root.corner.startsWith("top")
        anchors.bottom: root.corner.startsWith("bottom")
        anchors.left: root.corner.endsWith("left")
        anchors.right: root.corner.endsWith("right")
        margins { top: root.edgeMargin; bottom: root.edgeMargin; left: root.edgeMargin; right: root.edgeMargin }
        implicitWidth: Math.max(280, Math.min(600, Number(Quickshell.env("MUSCLI_WIDGET_WIDTH") || 360)))
        implicitHeight: content.implicitHeight + 36
        color: "transparent"
        Rectangle {
            anchors.fill: parent; radius: 18; color: root.background
            border.width: 1; border.color: "#34413e"
        }
        ColumnLayout {
            id: content
            anchors { left: parent.left; right: parent.right; top: parent.top; margins: 18 }
            spacing: 14
            RowLayout {
                Layout.fillWidth: true
                Text { text: "muscli"; color: root.accent; font.pixelSize: 14; font.weight: Font.DemiBold }
                Item { Layout.fillWidth: true }
                Text {
                    text: !root.connected ? "OFFLINE" : root.snapshot.context?.album ? "ALBUM" : root.snapshot.mix_progress !== null ? "MIX" : "LOCAL"
                    color: root.muted; font.pixelSize: 10; font.letterSpacing: 1.5
                }
            }
            RowLayout {
                Layout.fillWidth: true; spacing: 14
                Rectangle {
                    Layout.preferredWidth: 96; Layout.preferredHeight: 96
                    color: "#242e2b"; radius: 8
                    Text { anchors.centerIn: parent; text: "♪"; color: root.muted; font.pixelSize: 34 }
                    Image {
                        anchors.fill: parent; source: root.readyCover
                        visible: root.connected && source.toString() !== ""
                        fillMode: Image.PreserveAspectCrop; sourceSize: Qt.size(208, 208)
                    }
                }
                ColumnLayout {
                    Layout.fillWidth: true; spacing: 8
                    Text {
                        Layout.fillWidth: true; text: root.current?.title || (root.connected ? "Ready to play" : "Player disconnected")
                        color: root.foreground; font.pixelSize: 18; font.weight: Font.DemiBold
                        wrapMode: Text.Wrap; maximumLineCount: 2; elide: Text.ElideRight
                    }
                    Text { Layout.fillWidth: true; text: root.current?.artist || "muscli · local music"; color: root.muted; font.pixelSize: 12; elide: Text.ElideRight }
                }
            }
            ColumnLayout {
                Layout.fillWidth: true; spacing: 7
                Rectangle {
                    Layout.fillWidth: true; height: 3; color: "#34413e"; radius: 2
                    Rectangle { width: parent.width * (root.duration > 0 ? root.position / root.duration : 0); height: parent.height; radius: 2; color: root.accent }
                }
                RowLayout {
                    Layout.fillWidth: true
                    Text { text: root.timeLabel(root.position); color: root.muted; font.pixelSize: 10 }
                    Item { Layout.fillWidth: true }
                    Text { text: root.timeLabel(root.duration); color: root.muted; font.pixelSize: 10 }
                }
            }
            RowLayout {
                Layout.alignment: Qt.AlignHCenter; spacing: 12
                TransportButton { id: previousButton; text: "‹"; enabled: root.player?.canGoPrevious ?? false; onClicked: root.player.previous(); Accessible.name: "Previous" }
                TransportButton { id: playButton; text: root.player?.isPlaying ? "Ⅱ" : "▷"; enabled: root.player?.canTogglePlaying ?? false; onClicked: root.player.togglePlaying(); Accessible.name: "Play or pause" }
                TransportButton { id: nextButton; text: "›"; enabled: root.player?.canGoNext ?? false; onClicked: root.player.next(); Accessible.name: "Next" }
            }
            RowLayout {
                Layout.fillWidth: true; spacing: 12
                Text { text: "VOL"; color: root.muted; font.pixelSize: 10 }
                Slider {
                    id: volumeSlider
                    Layout.fillWidth: true; from: 0; to: 1
                    value: root.player?.volume ?? (root.snapshot.volume || 0)
                    enabled: (root.player?.canControl ?? false) && (root.player?.volumeSupported ?? false)
                    onMoved: if (root.player) root.player.volume = value
                    background: Rectangle { x: volumeSlider.leftPadding; y: volumeSlider.topPadding + volumeSlider.availableHeight / 2 - height / 2; width: volumeSlider.availableWidth; height: 3; color: "#34413e"; radius: 2
                        Rectangle { width: volumeSlider.visualPosition * parent.width; height: parent.height; color: root.accent; radius: 2 }
                    }
                    handle: Rectangle { x: volumeSlider.leftPadding + volumeSlider.visualPosition * (volumeSlider.availableWidth - width); y: volumeSlider.topPadding + volumeSlider.availableHeight / 2 - height / 2; width: 12; height: 12; radius: 6; color: root.accent }
                    Accessible.name: "Volume"
                }
            }
            ColumnLayout {
                Layout.fillWidth: true; spacing: 8
                visible: root.connected && (root.snapshot.upcoming?.length || 0) > 0
                Text { text: "UP NEXT"; color: root.muted; font.pixelSize: 10; font.letterSpacing: 1.5 }
                Repeater {
                    model: root.connected ? (root.snapshot.upcoming || []).slice(0, 4) : []
                    delegate: Text {
                        required property var modelData
                        required property int index
                        Layout.fillWidth: true
                        text: (index + 1) + "   " + modelData.title + "  ·  " + modelData.artist
                        color: index === 0 ? root.foreground : root.muted; font.pixelSize: 12; elide: Text.ElideRight
                    }
                }
            }
        }
    }
}
