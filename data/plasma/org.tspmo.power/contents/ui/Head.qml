// `Label ──────── value`: a dim caption, a hairline, a readout.
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

RowLayout {
    id: head

    property string label
    property string value

    spacing: Kirigami.Units.smallSpacing

    Text {
        text: head.label
        color: Kirigami.Theme.disabledTextColor
        font.family: Kirigami.Theme.defaultFont.family
        font.pixelSize: Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.85)
    }
    Rectangle {
        Layout.fillWidth: true
        Layout.alignment: Qt.AlignVCenter
        height: 1
        color: Kirigami.Theme.separatorColor ?? Qt.rgba(1, 1, 1, 0.15)
        opacity: 0.6
    }
    Text {
        text: head.value
        color: Kirigami.Theme.disabledTextColor
        font.family: "monospace"
        font.pixelSize: Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.85)
    }
}
