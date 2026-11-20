import QtQuick
import qs.components
Item {
    Canvas {
        objectName: "monitorFocusSigil"
        visible: true
        anchors.centerIn: parent
        y: 0
        width: 30
        height: 19

        onPaint: {
            const ctx = getContext("2d");
            ctx.reset();
            ctx.translate(width / 2, height / 2);
            ctx.fillStyle = Woodland.barkEdge;
            ctx.strokeStyle = Woodland.parchmentEdge;
            ctx.lineWidth = 1;
            ctx.beginPath();
            ctx.moveTo(-12, 0);
            ctx.quadraticCurveTo(0, -10, 12, 0);
            ctx.quadraticCurveTo(0, 10, -12, 0);
            ctx.fill();
            ctx.stroke();
            ctx.beginPath();
            ctx.arc(0, 0, 3.5, 0, Math.PI * 2);
            ctx.stroke();
            ctx.fillStyle = Woodland.parchmentEdge;
            ctx.beginPath();
            ctx.moveTo(0, -2.5);
            ctx.lineTo(1.5, 0);
            ctx.lineTo(0, 2.5);
            ctx.lineTo(-1.5, 0);
            ctx.closePath();
            ctx.fill();
            ctx.beginPath();
            for (const side of [-1, 1]) {
                ctx.moveTo(0, side * 6);
                ctx.lineTo(0, side * 9);
                ctx.moveTo(-7, side * 5);
                ctx.lineTo(-9, side * 7);
                ctx.moveTo(7, side * 5);
                ctx.lineTo(9, side * 7);
            }
            ctx.stroke();
        }
    }

}
