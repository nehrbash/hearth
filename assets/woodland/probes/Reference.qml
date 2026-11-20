import QtQuick
import Quickshell
import Burl.Config
import qs.components
import qs.modules.bar
import qs.modules.bar.components.workspaces
Item {
    id: root
    ScreenState { id: state; modelData: Quickshell.screens[0] }
    BarWrapper {
        id: bar
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        width: implicitWidth
        screen: Quickshell.screens[0]
        screenState: state
        popouts: null
        fullscreen: false
        Component.onCompleted: console.log("REFERENCE", contentWidth, padding, Tokens.sizes.bar.innerWidth, Tokens.spacing.medium, Tokens.font.family, Config.bar.workspaces.shown, Config.bar.workspaces.showWindows, Config.bar.workspaces.occupiedBg)
    }
    Timer { interval: 1800; running: true; onTriggered: {
        function walk(item) {
            if (item.isWorkspace) {
                const at = item.mapToItem(root, 0, 0);
                console.log("WORKSPACE", item.ws, at.x, at.y, item.width, item.height, item.cardHeight, item.size, item.isOccupied, item.isActive);
            }
            if (item.children) for (const child of item.children) walk(child);
        }
        walk(bar);
    } }
}
