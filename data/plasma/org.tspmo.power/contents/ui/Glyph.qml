// A monochrome line icon on a 24 x 24 grid (the same paths as tspmo-gui's `Ic`).
import QtQuick
import QtQuick.Shapes

Item {
    id: glyph

    property string name
    property color color: "white"
    property real size: 16

    width: size
    height: size

    readonly property var paths: ({
        "eco-plus": "M4.5 19.5C4.5 10.5 9.5 5 20 4C19.5 14.5 14.5 19.5 4.5 19.5ZM4.5 19.5L13 11",
        "silent": "M20 14.5A8.2 8.2 0 1 1 9.5 4A6.6 6.6 0 0 0 20 14.5Z",
        "balanced": "M12 4V20M7 20H17M5 7.5H19M5 7.5L2.5 14H7.5ZM19 7.5L16.5 14H21.5Z",
        "performance": "M13.5 2.5L5 13.5H11.5L10.5 21.5L19 10.5H12.5Z",
        "tune": "M4 7H20M4 12H20M4 17H20M9 5V9M15 10V14M8 15V19",
        "eco": "M4.5 19.5C4.5 10.5 9.5 5 20 4C19.5 14.5 14.5 19.5 4.5 19.5ZM4.5 19.5L13 11",
        "hybrid": "M9 6A6 6 0 1 0 9 18A6 6 0 1 0 9 6ZM15 6A6 6 0 1 0 15 18A6 6 0 1 0 15 6Z",
        "ultimate": "M7 8.5H17A5 5 0 0 1 22 13.5V15A3 3 0 0 1 16.2 16L15 14.2H9L7.8 16A3 3 0 0 1 2 15V13.5A5 5 0 0 1 7 8.5ZM7.5 11V15M5.5 13H9.5M16 12.2H16.1M18.4 14.2H18.5",
        "optimized": "M12 3L14 10L21 12L14 14L12 21L10 14L3 12L10 10Z",
        "gauge": "M3.5 17.5A9.5 9.5 0 1 1 20.5 17.5M12 15L16.5 8.5",
        "chip": "M7 7H17V17H7ZM10 3.5V7M14 3.5V7M10 17V20.5M14 17V20.5M3.5 10H7M3.5 14H7M17 10H20.5M17 14H20.5",
        "system": "M12 8.5A3.5 3.5 0 1 0 12 15.5A3.5 3.5 0 1 0 12 8.5ZM12 2.5V5M12 19V21.5M2.5 12H5M19 12H21.5M5.3 5.3L7 7M17 17L18.7 18.7M5.3 18.7L7 17M17 7L18.7 5.3"
    })

    Shape {
        width: 24
        height: 24
        scale: glyph.size / 24
        transformOrigin: Item.TopLeft
        preferredRendererType: Shape.CurveRenderer
        ShapePath {
            strokeColor: glyph.color
            strokeWidth: Math.max(24 * 0.085, 24 / glyph.size)
            fillColor: "transparent"
            capStyle: ShapePath.RoundCap
            joinStyle: ShapePath.RoundJoin
            PathSvg { path: glyph.paths[glyph.name] ?? glyph.paths["gauge"] }
        }
    }
}
