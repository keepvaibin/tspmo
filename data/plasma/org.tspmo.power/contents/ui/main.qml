// Power Mode: the four tspmo modes and the GPU mode, replacing the power-profile applet.
// State comes from tspmod's PropertiesChanged signal; nothing polls. Looks like tspmo-gui.
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.plasma.plasmoid
import org.kde.plasma.components as PlasmaComponents
import org.kde.plasma.plasma5support as Plasma5Support
import org.kde.plasma.workspace.dbus as DBus

PlasmoidItem {
    id: root

    readonly property bool dark: Kirigami.ColorUtils.brightnessForColor(Kirigami.Theme.backgroundColor) === Kirigami.ColorUtils.Dark

    // One accent per mode, tuned for the theme (same tokens as tspmo-gui).
    readonly property var modes: [
        { id: "eco-plus", label: "Eco+", icon: "battery-profile-powersave", dim: "#8cd44a", lit: "#4f9a14" },
        { id: "power-saver", label: "Silent", icon: "weather-clear-night", dim: "#35c9a6", lit: "#0e9d7e" },
        { id: "balanced", label: "Balanced", icon: "battery-profile-balanced", dim: "#4d9cf7", lit: "#1f74dc" },
        { id: "performance", label: "Turbo", icon: "battery-profile-performance", dim: "#f2673b", lit: "#d6481c" }
    ]
    readonly property var gpuModes: [
        { id: "eco", label: "Eco" },
        { id: "hybrid", label: "Standard" },
        { id: "ultimate", label: "Ultimate" },
        { id: "optimized", label: "Optimized" }
    ]

    function modeInfo(id) {
        for (const m of modes) {
            if (m.id === id) return m;
        }
        return modes[2];
    }
    function accentOf(m) { return dark ? m.dim : m.lit; }

    // What the dGPU is doing: removed (Eco), runtime-suspended, or powered.
    readonly property string gpuState: daemon.gpuMode === "" ? ""
        : daemon.gpuMode === "eco" ? "dGPU off"
        : daemon.dgpuAsleep ? "dGPU asleep" : "dGPU active"

    Daemon {
        id: daemon
        busType: DBus.BusType.System
    }

    Plasma5Support.DataSource {
        id: launcher
        engine: "executable"
        onNewData: (source) => disconnectSource(source)
    }

    Plasmoid.icon: modeInfo(daemon.profile).icon
    toolTipMainText: daemon.online ? modeInfo(daemon.profile).label : "tspmo"
    toolTipSubText: daemon.online ? gpuState : ""

    compactRepresentation: MouseArea {
        acceptedButtons: Qt.LeftButton | Qt.MiddleButton
        onClicked: (mouse) => {
            if (mouse.button === Qt.MiddleButton) {
                launcher.connectSource("tspmo-gui --toggle");
            } else {
                root.expanded = !root.expanded;
            }
        }
        Kirigami.Icon {
            anchors.fill: parent
            source: Plasmoid.icon
            active: parent.containsMouse
        }
    }

    fullRepresentation: Item {
        id: full
        readonly property int pad: Kirigami.Units.largeSpacing
        Layout.minimumWidth: Kirigami.Units.gridUnit * 22
        Layout.preferredWidth: Kirigami.Units.gridUnit * 22
        implicitWidth: Kirigami.Units.gridUnit * 22
        implicitHeight: col.implicitHeight + pad * 2
        focus: true

        Keys.onPressed: (event) => {
            const i = event.key - Qt.Key_1;
            if (i >= 0 && i < root.modes.length && daemon.online) {
                daemon.setProfile(root.modes[i].id);
                event.accepted = true;
            }
        }

        ColumnLayout {
            id: col
            anchors.fill: parent
            anchors.margins: full.pad
            spacing: Kirigami.Units.largeSpacing

        ColumnLayout {
            Layout.fillWidth: true
            spacing: Math.round(Kirigami.Units.smallSpacing * 1.5)
            Head {
                Layout.fillWidth: true
                label: "Mode"
                value: !daemon.online ? "" : daemon.onBattery ? "on battery" : "on AC"
            }
            RowLayout {
                Layout.fillWidth: true
                spacing: Math.round(Kirigami.Units.smallSpacing * 0.75)
                Repeater {
                    model: root.modes
                    Tile {
                        required property var modelData
                        required property int index
                        Layout.fillWidth: true
                        Layout.preferredWidth: 1
                        implicitHeight: Math.round(Kirigami.Units.gridUnit * 2.6)
                        text: modelData.label
                        hint: String(index + 1)
                        accent: root.accentOf(modelData)
                        keyed: true
                        selected: daemon.profile === modelData.id
                        enabled: daemon.online
                        onActivated: daemon.setProfile(modelData.id)
                    }
                }
            }
        }

        ColumnLayout {
            Layout.fillWidth: true
            visible: daemon.gpuMode !== ""
            spacing: Math.round(Kirigami.Units.smallSpacing * 1.5)
            Head {
                Layout.fillWidth: true
                label: "GPU"
                value: daemon.gpuSwitching !== "" ? "switching" : root.gpuState
            }
            RowLayout {
                Layout.fillWidth: true
                spacing: Math.round(Kirigami.Units.smallSpacing * 0.75)
                Repeater {
                    model: root.gpuModes
                    Tile {
                        required property var modelData
                        Layout.fillWidth: true
                        Layout.preferredWidth: 1
                        text: modelData.label
                        accent: root.accentOf(root.modeInfo(daemon.profile))
                        selected: daemon.gpuPolicy === modelData.id
                        enabled: daemon.online && daemon.gpuSwitching === ""
                        onActivated: daemon.setGpu(modelData.id)
                    }
                }
            }
        }

        Text {
            Layout.fillWidth: true
            visible: text !== ""
            text: !daemon.online ? "tspmod is not running" : daemon.error
            color: Kirigami.Theme.negativeTextColor
            font.pixelSize: Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.85)
            elide: Text.ElideRight
        }

        Tile {
            Layout.fillWidth: true
            text: "All settings"
            hint: "ROG"
            accent: root.accentOf(root.modeInfo(daemon.profile))
            onActivated: {
                launcher.connectSource("tspmo-gui --toggle");
                root.expanded = false;
            }
        }
        }
    }
}
