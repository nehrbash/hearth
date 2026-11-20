import QtQuick
import QtQuick.Shapes
import qs.components
import qs.components.widgets
import qs.services
Item { id: ws; property bool isActive: false; readonly property int size: 40;
        BarkSocket {
            anchors.horizontalCenter: parent.horizontalCenter
            y: (ws.size - height) / 2
            width: ws.size * 0.72
            height: width
        }

        Shape {
            id: seal
            anchors.horizontalCenter: parent.horizontalCenter
            y: (ws.size - height) / 2
            width: ws.size - 1
            height: width
            preferredRendererType: Shape.CurveRenderer

            ShapePath {
                fillColor: "transparent"
                strokeColor: ws.isActive ? Colours.palette.m3tertiary : Qt.alpha(Woodland.parchmentEdge, 0.58)
                strokeWidth: ws.isActive ? 1.6 : 1
                capStyle: ShapePath.RoundCap

                PathAngleArc {
                    centerX: seal.width / 2
                    centerY: seal.height / 2
                    radiusX: seal.width * 0.45
                    radiusY: radiusX
                    startAngle: 135
                    sweepAngle: 270
                }
            }

            ShapePath {
                fillColor: "transparent"
                strokeColor: Qt.alpha(ws.isActive ? Colours.palette.m3tertiary : Woodland.parchmentEdge, ws.isActive ? 0.72 : 0.34)
                strokeWidth: 1
                capStyle: ShapePath.RoundCap

                PathAngleArc {
                    centerX: seal.width / 2
                    centerY: seal.height / 2
                    radiusX: seal.width * 0.30
                    radiusY: radiusX
                    startAngle: 20
                    sweepAngle: 48
                }
            }
        }

        Repeater {
            model: 7
            delegate: Rectangle {
                required property int index
                readonly property real angle: (150 + index * 40) * Math.PI / 180
                x: ws.width / 2 + Math.cos(angle) * ws.size * 0.4 - width / 2
                y: ws.size / 2 + Math.sin(angle) * ws.size * 0.4 - height / 2
                width: 1
                height: ws.size * 0.08
                rotation: 150 + index * 40 + 90
                color: ws.isActive ? Colours.palette.m3tertiary : Qt.alpha(Woodland.parchmentEdge, 0.58)
            }
        }


}
