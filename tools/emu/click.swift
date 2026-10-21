// click X Y [COUNT]: left-click (COUNT = 2: double-click) at screen point X, Y.
import CoreGraphics
import Foundation

let a = CommandLine.arguments
guard a.count >= 3, let x = Double(a[1]), let y = Double(a[2]) else {
    FileHandle.standardError.write("usage: click X Y [COUNT]\n".data(using: .utf8)!)
    exit(2)
}
let count = a.count > 3 ? Int(a[3]) ?? 1 : 1
let p = CGPoint(x: x, y: y)
CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: p, mouseButton: .left)?.post(tap: .cghidEventTap)
usleep(100_000)
for n in 1...count {
    for t in [CGEventType.leftMouseDown, .leftMouseUp] {
        let e = CGEvent(mouseEventSource: nil, mouseType: t, mouseCursorPosition: p, mouseButton: .left)!
        e.setIntegerValueField(.mouseEventClickState, value: Int64(n))
        e.post(tap: .cghidEventTap)
        usleep(30_000)
    }
}
