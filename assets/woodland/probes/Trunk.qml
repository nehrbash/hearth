import QtQuick
import qs.modules.bar
Item {
    clip: true
    QtObject { id: geometry; property real trunkWidth: 358.4; property real trunkX: 30 - 358.4 * 0.265; property bool visible: true }
    SidebarTrunk { bar: geometry; height: parent.height }
    Rectangle {
        x: -2; width: 68; height: parent.height
        gradient: Gradient {
            orientation: Gradient.Horizontal
            GradientStop { position: 0; color: "transparent" }
            GradientStop { position: 0.28; color: Qt.rgba(0.035,0.027,0.021,0.3) }
            GradientStop { position: 0.5; color: Qt.rgba(0.035,0.027,0.021,0.44) }
            GradientStop { position: 0.72; color: Qt.rgba(0.035,0.027,0.021,0.3) }
            GradientStop { position: 1; color: "transparent" }
        }
    }
}
