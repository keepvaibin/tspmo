// Section head: a small icon, a bold title that carries the state, a reading on the right.
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

RowLayout {
    id: head

    property string icon
    property string title
    property string info
    property real dp: 1

    spacing: 6 * dp

    Glyph {
        visible: head.icon !== ""
        name: head.icon
        size: 15 * head.dp
        color: Kirigami.Theme.textColor
        Layout.alignment: Qt.AlignVCenter
    }
    Text {
        Layout.fillWidth: true
        text: head.title
        elide: Text.ElideRight
        color: Kirigami.Theme.textColor
        font.family: Kirigami.Theme.defaultFont.family
        font.pixelSize: Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.98)
        font.weight: Font.Bold
    }
    Text {
        visible: text !== ""
        text: head.info
        color: Kirigami.Theme.textColor
        opacity: 0.8
        font.family: "monospace"
        font.pixelSize: Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.85)
    }
}
