// One option of a row: a flat tile. The chosen one gets a neutral fill and a bar in the accent;
// a keyed tile (the modes) shows its own colour dimly when not chosen. Same look as tspmo-gui.
import QtQuick
import org.kde.kirigami as Kirigami

Item {
    id: tile

    property string text
    property color accent: Kirigami.Theme.highlightColor
    property bool keyed: false
    property bool selected: false
    signal activated()

    implicitHeight: Math.round(Kirigami.Units.gridUnit * 2.2)
    implicitWidth: label.implicitWidth + Kirigami.Units.largeSpacing * 2
    activeFocusOnTab: enabled
    opacity: enabled ? 1 : 0.4

    Rectangle {
        anchors.fill: parent
        radius: 3
        color: area.pressed ? Qt.darker(Kirigami.Theme.alternateBackgroundColor, 1.25)
             : area.containsMouse ? Qt.darker(Kirigami.Theme.alternateBackgroundColor, 1.1)
             : Kirigami.Theme.alternateBackgroundColor
        Behavior on color { ColorAnimation { duration: 100 } }
    }
    Rectangle {
        anchors.fill: parent
        radius: 3
        color: Kirigami.Theme.textColor
        opacity: tile.selected ? 0.12 : 0
        Behavior on opacity { NumberAnimation { duration: 140 } }
    }
    Rectangle {
        anchors.bottom: parent.bottom
        width: parent.width
        height: tile.selected ? 3 : (tile.keyed ? 2 : 0)
        color: tile.accent
        opacity: tile.selected ? 1 : 0.5
        Behavior on height { NumberAnimation { duration: 140 } }
    }
    Text {
        id: label
        anchors.centerIn: parent
        width: parent.width - Kirigami.Units.smallSpacing * 2
        horizontalAlignment: Text.AlignHCenter
        elide: Text.ElideRight
        text: tile.text
        color: Kirigami.Theme.textColor
        font.family: Kirigami.Theme.defaultFont.family
        font.pixelSize: Kirigami.Theme.defaultFont.pixelSize
        font.weight: tile.selected ? Font.Bold : Font.Normal
    }
    Rectangle {
        anchors.fill: parent
        radius: 3
        color: "transparent"
        border.width: tile.activeFocus ? 2 : 0
        border.color: Kirigami.Theme.textColor
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        enabled: tile.enabled
        onClicked: {
            tile.forceActiveFocus();
            tile.activated();
        }
    }
    Keys.onPressed: (event) => {
        if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter || event.key === Qt.Key_Space) {
            tile.activated();
            event.accepted = true;
        }
    }
}
