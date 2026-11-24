// One option of a row: a flat tile, a small icon over the label. The chosen one is outlined in
// its colour (a mode's own, otherwise the accent); unavailable ones are dimmed. Same look as
// tspmo-gui.
import QtQuick
import org.kde.kirigami as Kirigami

Item {
    id: tile

    property string text
    property string icon
    property real dp: 1
    property color accent: Kirigami.Theme.highlightColor
    property bool selected: false
    property bool dimmed: false
    signal activated()

    implicitHeight: Math.round((icon !== "" ? 58 : 26) * dp)
    implicitWidth: label.implicitWidth + 12 * dp
    activeFocusOnTab: enabled
    opacity: enabled ? (dimmed ? 0.38 : 1) : 0.4
    Behavior on opacity { NumberAnimation { duration: 140 } }

    Rectangle {
        anchors.fill: parent
        radius: 4 * tile.dp
        color: area.pressed ? Qt.darker(Kirigami.Theme.alternateBackgroundColor, 1.25)
             : area.containsMouse && !tile.dimmed ? Qt.darker(Kirigami.Theme.alternateBackgroundColor, 1.1)
             : Kirigami.Theme.alternateBackgroundColor
        Behavior on color { ColorAnimation { duration: 100 } }
    }
    Glyph {
        visible: tile.icon !== ""
        name: tile.icon
        size: 18 * tile.dp
        color: tile.selected ? tile.accent : Kirigami.Theme.textColor
        x: Math.round((parent.width - width) / 2)
        y: Math.round(10 * tile.dp)
    }
    Text {
        id: label
        x: 2 * tile.dp
        width: parent.width - 4 * tile.dp
        y: tile.icon !== "" ? parent.height - height - 9 * tile.dp : Math.round((parent.height - height) / 2)
        horizontalAlignment: Text.AlignHCenter
        elide: Text.ElideRight
        text: tile.text
        color: Kirigami.Theme.textColor
        font.family: Kirigami.Theme.defaultFont.family
        font.pixelSize: Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.9)
        font.weight: tile.selected ? Font.Bold : Font.Normal
    }
    Rectangle {
        anchors.fill: parent
        radius: 4 * tile.dp
        color: "transparent"
        border.width: tile.selected ? 2 : 0
        border.color: tile.accent
        Behavior on border.width { NumberAnimation { duration: 140 } }
    }
    Rectangle {
        x: 3 * tile.dp
        y: 3 * tile.dp
        width: parent.width - 6 * tile.dp
        height: parent.height - 6 * tile.dp
        radius: 2 * tile.dp
        color: "transparent"
        border.width: tile.activeFocus ? 1.5 : 0
        border.color: Kirigami.Theme.textColor
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        enabled: tile.enabled && !tile.dimmed
        onClicked: {
            tile.forceActiveFocus();
            tile.activated();
        }
    }
    Keys.onPressed: (event) => {
        if (!tile.dimmed && (event.key === Qt.Key_Return || event.key === Qt.Key_Enter || event.key === Qt.Key_Space)) {
            tile.activated();
            event.accepted = true;
        }
    }
}
