// Lists on-screen windows of an app: `winlist OWNER` prints "id<TAB>x<TAB>y<TAB>w<TAB>h<TAB>title" per window
// (points, top-left origin), frontmost first.
import CoreGraphics
import Foundation

let owner = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "Astris"
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
for w in list {
    guard let o = w[kCGWindowOwnerName as String] as? String, o == owner,
          (w[kCGWindowLayer as String] as? Int) == 0,
          let id = w[kCGWindowNumber as String] as? Int,
          let b = w[kCGWindowBounds as String] as? [String: CGFloat] else { continue }
    let title = (w[kCGWindowName as String] as? String) ?? ""
    print("\(id)\t\(Int(b["X"] ?? 0))\t\(Int(b["Y"] ?? 0))\t\(Int(b["Width"] ?? 0))\t\(Int(b["Height"] ?? 0))\t\(title)")
}
