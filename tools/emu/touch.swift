// touch X0 Y0 [X1 Y1] [MS]: press the left button at screen point (X0, Y0), drag it to (X1, Y1) over MS ms
// (default 150; without X1 Y1 it holds still, sending no drag events), then release. Astris turns the mouse into the touch screen; it
// registers a touch a few frames after the press, so a drag first holds still at (X0, Y0) for 100 ms (or a fast
// drag would start where the finger already is).
import CoreGraphics
import Foundation

// touch move X Y: only move the pointer (no button), e.g. to park it after a touch
let src = CGEventSource(stateID: .hidSystemState)
if CommandLine.arguments.count == 4 && CommandLine.arguments[1] == "move",
   let x = Double(CommandLine.arguments[2]), let y = Double(CommandLine.arguments[3])
{
    CGEvent(mouseEventSource: src, mouseType: .mouseMoved, mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left)?
        .post(tap: .cghidEventTap)
    exit(0)
}

let a = CommandLine.arguments.dropFirst().compactMap { Double($0) }
guard a.count == 2 || a.count == 3 || a.count == 4 || a.count == 5 else {
    FileHandle.standardError.write("usage: touch X0 Y0 [X1 Y1] [MS]\n".data(using: .utf8)!)
    exit(2)
}
let p0 = CGPoint(x: a[0], y: a[1])
let p1 = a.count >= 4 ? CGPoint(x: a[2], y: a[3]) : p0
let ms = a.count == 3 ? a[2] : (a.count == 5 ? a[4] : 150)

func post(_ t: CGEventType, _ p: CGPoint) {
    CGEvent(mouseEventSource: src, mouseType: t, mouseCursorPosition: p, mouseButton: .left)?.post(tap: .cghidEventTap)
}

post(.mouseMoved, p0)
usleep(50_000)
post(.leftMouseDown, p0)
if p1 != p0 {
    usleep(100_000)
}
// a tap only presses and releases: Astris takes drag events at the press point as fresh presses (a held
// config - / + then stepped several times per tap instead of waiting out its 20-frame repeat delay)
if p1 == p0 {
    usleep(useconds_t(ms * 1000))
    post(.leftMouseUp, p0)
    exit(0)
}
let steps = max(1, Int(ms / 16))
for i in 1...steps {
    usleep(useconds_t(ms * 1000 / Double(steps)))
    let t = Double(i) / Double(steps)
    post(.leftMouseDragged, CGPoint(x: p0.x + (p1.x - p0.x) * t, y: p0.y + (p1.y - p0.y) * t))
}
post(.leftMouseUp, p1)
