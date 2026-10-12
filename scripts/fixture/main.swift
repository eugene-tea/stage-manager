// SM Fixture: a tiny AppKit app whose windows Stage Manager tests manage instead of the
// owner's. Opens N standard windows titled "Fixture 1…N", each a distinct flat color with
// its title drawn large, at distinct frames, plus an optional utility panel.
//
// Options (arguments or environment):
//   --windows N  | FIXTURE_WINDOWS=N   number of standard windows (default 4)
//   --panel      | FIXTURE_PANEL=1     also open "Fixture Panel" (an NSPanel, child-like)
//   --background | FIXTURE_BACKGROUND=1  do not activate; order windows to the back
// A bare number argument is the same as --windows N.
import AppKit

struct Options {
    var windows = 4
    var panel = false
    var background = false

    static func parse() -> Options {
        var o = Options()
        let env = ProcessInfo.processInfo.environment
        if let n = env["FIXTURE_WINDOWS"].flatMap(Int.init) { o.windows = n }
        if env["FIXTURE_PANEL"] == "1" { o.panel = true }
        if env["FIXTURE_BACKGROUND"] == "1" { o.background = true }
        var args = CommandLine.arguments.dropFirst().makeIterator()
        while let a = args.next() {
            switch a {
            case "--windows": if let n = args.next().flatMap(Int.init) { o.windows = n }
            case "--panel": o.panel = true
            case "--background": o.background = true
            default: if let n = Int(a) { o.windows = n }
            }
        }
        o.windows = max(0, min(o.windows, 12))
        return o
    }
}

let palette: [NSColor] = [
    NSColor(srgbRed: 0.85, green: 0.25, blue: 0.25, alpha: 1),
    NSColor(srgbRed: 0.20, green: 0.55, blue: 0.85, alpha: 1),
    NSColor(srgbRed: 0.25, green: 0.70, blue: 0.35, alpha: 1),
    NSColor(srgbRed: 0.90, green: 0.65, blue: 0.15, alpha: 1),
    NSColor(srgbRed: 0.55, green: 0.35, blue: 0.80, alpha: 1),
    NSColor(srgbRed: 0.15, green: 0.70, blue: 0.70, alpha: 1),
    NSColor(srgbRed: 0.85, green: 0.40, blue: 0.65, alpha: 1),
    NSColor(srgbRed: 0.45, green: 0.45, blue: 0.45, alpha: 1),
    NSColor(srgbRed: 0.60, green: 0.45, blue: 0.25, alpha: 1),
    NSColor(srgbRed: 0.35, green: 0.30, blue: 0.75, alpha: 1),
    NSColor(srgbRed: 0.55, green: 0.70, blue: 0.20, alpha: 1),
    NSColor(srgbRed: 0.80, green: 0.30, blue: 0.10, alpha: 1),
]

/// A flat color with a label drawn large in the middle.
final class LabelView: NSView {
    let color: NSColor
    let label: String

    init(frame: NSRect, color: NSColor, label: String) {
        self.color = color
        self.label = label
        super.init(frame: frame)
    }

    required init?(coder: NSCoder) { nil }

    override func draw(_ dirtyRect: NSRect) {
        color.setFill()
        bounds.fill()
        let size = max(18, min(bounds.width / 7, bounds.height / 3))
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: size, weight: .bold),
            .foregroundColor: NSColor.white,
        ]
        let text = NSAttributedString(string: label, attributes: attrs)
        let t = text.size()
        text.draw(at: NSPoint(x: bounds.midX - t.width / 2, y: bounds.midY - t.height / 2))
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    let options = Options.parse()
    var windows: [NSWindow] = []

    func applicationDidFinishLaunching(_ notification: Notification) {
        let area = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
        let size = NSSize(width: 520, height: 360)
        for i in 0..<options.windows {
            // Distinct frames: a diagonal cascade with varied sizes, wrapping inside the screen.
            let w = size.width + CGFloat(i % 3) * 40
            let h = size.height + CGFloat(i % 2) * 30
            let dx = CGFloat(i % 6) * 110, dy = CGFloat(i % 6) * 70 + CGFloat(i / 6) * 30
            let origin = NSPoint(x: area.minX + 80 + dx, y: area.maxY - 80 - h - dy)
            let title = "Fixture \(i + 1)"
            let window = NSWindow(
                contentRect: NSRect(origin: origin, size: NSSize(width: w, height: h)),
                styleMask: [.titled, .closable, .miniaturizable, .resizable],
                backing: .buffered, defer: false)
            window.title = title
            window.isReleasedWhenClosed = false
            window.contentView = LabelView(frame: .zero, color: palette[i % palette.count], label: title)
            show(window)
            windows.append(window)
        }
        if options.panel {
            let panel = NSPanel(
                contentRect: NSRect(x: area.maxX - 340, y: area.maxY - 300, width: 260, height: 180),
                styleMask: [.titled, .closable, .utilityWindow],
                backing: .buffered, defer: false)
            panel.title = "Fixture Panel"
            // Not floating: it stays on layer 0 so it is a manageable child window.
            panel.isFloatingPanel = false
            panel.hidesOnDeactivate = false
            panel.isReleasedWhenClosed = false
            panel.contentView = LabelView(frame: .zero, color: .darkGray, label: "Panel")
            show(panel)
            windows.append(panel)
        }
        if !options.background {
            NSApp.activate(ignoringOtherApps: true)
        }
    }

    func show(_ window: NSWindow) {
        if options.background { window.orderBack(nil) } else { window.makeKeyAndOrderFront(nil) }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let mainMenu = NSMenu()
let appItem = NSMenuItem()
mainMenu.addItem(appItem)
let appMenu = NSMenu()
appMenu.addItem(withTitle: "Quit SM Fixture", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
appItem.submenu = appMenu
app.mainMenu = mainMenu
let delegate = AppDelegate()
app.delegate = delegate
app.run()
