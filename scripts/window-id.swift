// Prints the window id of the first normal window that process <pid> owns, for
// `screencapture -l`. Exits 1 when it has none yet. Used by scripts/shot.sh. A background test
// launch's window sits at the lowest window level (finch#jxb8), so that level counts too (finch#0vj9).
import CoreGraphics
import Foundation

guard CommandLine.arguments.count == 2, let pid = Int32(CommandLine.arguments[1]) else {
    FileHandle.standardError.write("usage: window-id <pid>\n".data(using: .utf8)!)
    exit(2)
}
let windows = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
let levels = [0, Int(CGWindowLevelForKey(.minimumWindow))]
for window in windows {
    guard (window[kCGWindowOwnerPID as String] as? Int32) == pid,
          levels.contains(window[kCGWindowLayer as String] as? Int ?? 1),
          let bounds = window[kCGWindowBounds as String] as? [String: Any],
          (bounds["Width"] as? Double ?? 0) > 100,
          let number = window[kCGWindowNumber as String] as? Int
    else { continue }
    print(number)
    exit(0)
}
exit(1)
