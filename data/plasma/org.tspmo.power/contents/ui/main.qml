// Power Mode: the four tspmo modes and the GPU mode, replacing the power-profile applet.
// State comes from tspmod's PropertiesChanged signal; nothing polls.
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.plasma.plasmoid
import org.kde.plasma.components as PlasmaComponents
import org.kde.plasma.plasma5support as Plasma5Support
import org.kde.plasma.workspace.dbus as DBus

PlasmoidItem {
    id: root

    readonly property var modes: [
        { id: "eco-plus", label: "Eco+", icon: "battery-profile-powersave" },
        { id: "power-saver", label: "Silent", icon: "weather-clear-night" },
        { id: "balanced", label: "Balanced", icon: "battery-profile-balanced" },
        { id: "performance", label: "Turbo", icon: "battery-profile-performance" }
    ]
    readonly property var gpuModes: [
        { id: "eco", label: "Eco" },
        { id: "hybrid", label: "Hybrid" },
        { id: "ultimate", label: "Ultimate" },
        { id: "optimized", label: "Auto" }
    ]

    function modeInfo(id) {
        for (const m of modes) {
            if (m.id === id) return m;
        }
        return modes[2];
    }

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
    toolTipSubText: daemon.online ? daemon.gpuPolicy : ""

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

    fullRepresentation: ColumnLayout {
        Layout.minimumWidth: Kirigami.Units.gridUnit * 20
        Layout.preferredWidth: Kirigami.Units.gridUnit * 20
        spacing: Kirigami.Units.smallSpacing

        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Repeater {
                model: root.modes
                PlasmaComponents.ToolButton {
                    required property var modelData
                    Layout.fillWidth: true
                    Layout.preferredWidth: 1
                    display: PlasmaComponents.AbstractButton.TextUnderIcon
                    icon.name: modelData.icon
                    text: modelData.label
                    checkable: true
                    checked: daemon.profile === modelData.id
                    enabled: daemon.online
                    onClicked: daemon.setProfile(modelData.id)
                }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            visible: daemon.gpuMode !== ""
            spacing: Kirigami.Units.smallSpacing
            Repeater {
                model: root.gpuModes
                PlasmaComponents.ToolButton {
                    required property var modelData
                    Layout.fillWidth: true
                    Layout.preferredWidth: 1
                    text: modelData.label
                    checkable: true
                    checked: daemon.gpuPolicy === modelData.id
                    enabled: daemon.online && daemon.gpuSwitching === ""
                    onClicked: daemon.setGpu(modelData.id)
                }
            }
        }

        PlasmaComponents.Label {
            Layout.fillWidth: true
            visible: text !== ""
            text: !daemon.online ? "tspmod is not running" : daemon.error
            color: Kirigami.Theme.negativeTextColor
            elide: Text.ElideRight
        }

        PlasmaComponents.Button {
            Layout.alignment: Qt.AlignRight
            icon.name: "configure"
            text: "More"
            onClicked: {
                launcher.connectSource("tspmo-gui --toggle");
                root.expanded = false;
            }
        }
    }
}
