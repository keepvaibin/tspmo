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
        { id: "eco-plus", label: "Eco+", icon: "battery-profile-powersave", glyph: "eco-plus", dim: "#8cd44a", lit: "#4f9a14" },
        { id: "power-saver", label: "Silent", icon: "weather-clear-night", glyph: "power-saver", dim: "#35c9a6", lit: "#0e9d7e" },
        { id: "balanced", label: "Balanced", icon: "battery-profile-balanced", glyph: "balanced", dim: "#4d9cf7", lit: "#1f74dc" },
        { id: "performance", label: "Turbo", icon: "battery-profile-performance", glyph: "performance", dim: "#f2673b", lit: "#d6481c" }
    ]
    readonly property var gpuModes: [
        { id: "eco", label: "Eco", icon: "eco" },
        { id: "hybrid", label: "Standard", icon: "hybrid" },
        { id: "ultimate", label: "Ultimate", icon: "ultimate" },
        { id: "optimized", label: "Optimized", icon: "optimized" }
    ]

    function modeInfo(id) {
        for (const m of modes) {
            if (m.id === id) return m;
        }
        return modes[2];
    }
    function accentOf(m) { return dark ? m.dim : m.lit; }

    // 1dp is 1 px at the default 10 pt font and grows with it (same as tspmo-gui).
    readonly property real dp: Kirigami.Theme.defaultFont.pixelSize / 13.333

    // Eco+ forces the dGPU off: the GPU row reads Eco and the other modes are out of reach.
    readonly property bool ecoPlus: daemon.profile === "eco-plus"

    // What the dGPU is doing: removed (Eco), runtime-suspended, or powered.
    readonly property string gpuState: daemon.gpuMode === "" ? ""
        : (ecoPlus || daemon.gpuMode === "eco") ? "dGPU off"
        : daemon.dgpuAsleep ? "dGPU asleep" : "dGPU active"
    readonly property string gpuTitle: ecoPlus ? "GPU Mode: Eco (Eco+)"
        : daemon.gpuSwitching !== "" ? "GPU Mode: switching"
        : daemon.gpuMode === "eco" ? "GPU Mode: Eco (iGPU only)"
        : daemon.gpuMode === "ultimate" ? "GPU Mode: Ultimate (dGPU only)"
        : "GPU Mode: iGPU + dGPU"

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
        readonly property real pad: 14 * root.dp
        Layout.minimumWidth: 440 * root.dp
        Layout.preferredWidth: 440 * root.dp
        implicitWidth: 440 * root.dp
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
            spacing: 14 * root.dp

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 6 * root.dp
                Head {
                    Layout.fillWidth: true
                    dp: root.dp
                    icon: "gauge"
                    title: "Mode: " + (daemon.online ? root.modeInfo(daemon.profile).label : "offline")
                    info: !daemon.online ? "" : daemon.onBattery ? "on battery" : "on AC"
                }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 6 * root.dp
                    Repeater {
                        model: root.modes
                        Tile {
                            required property var modelData
                            Layout.fillWidth: true
                            Layout.preferredWidth: 1
                            dp: root.dp
                            text: modelData.label
                            icon: modelData.glyph
                            accent: root.accentOf(modelData)
                            selected: daemon.profile === modelData.id
                            enabled: daemon.online
                            onActivated: daemon.setProfile(modelData.id)
                        }
                    }
                    Tile {
                        Layout.fillWidth: true
                        Layout.preferredWidth: 1
                        dp: root.dp
                        text: "Fans + Power"
                        icon: "tune"
                        onActivated: {
                            launcher.connectSource("tspmo-gui --window fans");
                            root.expanded = false;
                        }
                    }
                }
            }

            ColumnLayout {
                Layout.fillWidth: true
                visible: daemon.gpuMode !== ""
                spacing: 6 * root.dp
                Head {
                    Layout.fillWidth: true
                    dp: root.dp
                    icon: "chip"
                    title: root.gpuTitle
                    info: root.gpuState
                }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 6 * root.dp
                    Repeater {
                        model: root.gpuModes
                        Tile {
                            required property var modelData
                            required property int index
                            Layout.fillWidth: true
                            Layout.preferredWidth: 1
                            dp: root.dp
                            text: modelData.label
                            icon: modelData.icon
                            accent: root.accentOf(root.modeInfo(daemon.profile))
                            selected: root.ecoPlus ? index === 0 : daemon.gpuPolicy === modelData.id
                            dimmed: root.ecoPlus && index !== 0
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
                dp: root.dp
                text: "All settings"
                onActivated: {
                    launcher.connectSource("tspmo-gui --toggle");
                    root.expanded = false;
                }
            }
        }
    }
}
