// SM Fixture: a tiny AppKit app whose windows Stage Manager tests manage instead of the
// owner's. Opens N standard windows titled "Fixture 1…N", each a distinct flat color with
// its title drawn large, at distinct frames, plus an optional utility panel.
//
// Options (arguments or environment):
//   --windows N  | FIXTURE_WINDOWS=N   number of standard windows (default 4)
//   --panel      | FIXTURE_PANEL=1     also open "Fixture Panel" (an NSPanel, child-like)
//   --background | FIXTURE_BACKGROUND=1  do not activate; order windows to the back
//   --title T    | FIXTURE_TITLE=T     window titles "T 1…N" (default "Fixture")
//   --frames F   | FIXTURE_FRAMES=F    frames of the first windows, "x,y,w,h;x,y,w,h;..." in
//                                      points from the visible frame's top-left corner (y
//                                      down); a width <= 0 is the visible width plus it
//   --styles S   | FIXTURE_STYLES=S    window chrome per window, comma-separated, cycled:
//                                      plain (title bar only, the default), toolbar (unified
//                                      toolbar), compact (unified compact), expanded (toolbar
//                                      under the title), fullsize (content under a transparent
//                                      title bar, as Chrome and Telegram draw), titleless
//                                      (toolbar, title hidden); scripts/probe_park.py measures
//                                      what macOS keeps on screen of each when parked
//   --delay-ms N | FIXTURE_DELAY_MS=N  open the windows N ms after launch (an app reopening
//                                      its windows late after a login)
//   --stagger-ms N | FIXTURE_STAGGER_MS=N  one window every N ms instead of all at once
//   --reverse    | FIXTURE_REVERSE=1   open the windows last first (their window ids then come
//                                      in the other order; each keeps its own title and frame)
//   --retitle M  | FIXTURE_RETITLE=M   retitle windows once shown, "Old=New;Old2=New2" (whole
//                                      titles, e.g. "Fixture 1=Fixture 1 — Edited")
//   --retitle-after-ms N | FIXTURE_RETITLE_AFTER_MS=N  how long after a window shows (default 0)
//   --minimized M | FIXTURE_MINIMIZED=M  open these windows minimized, "T 1;T 2" (whole
//                                      titles: an app reopening a window that was minimized
//                                      when it quit). They open at launch, before --delay-ms
//                                      applies to the others: each is ordered in behind the
//                                      other windows and minimized a moment later (AppKit
//                                      minimizes a window that has been on screen; one never
//                                      ordered in was never listed by Accessibility in the
//                                      10-15 live run, and SIGHUP had nothing to un-minimize).
//                                      A test starts this fixture before Stage Manager, so
//                                      the moment on screen is not seen as a new window.
//   --tabbed N   | FIXTURE_TABBED=N    the last N standard windows open as tabs of window 1
//                                      (NSWindow tabbing), window 1 the selected tab: the
//                                      others are hidden tabs, listed off screen by the window
//                                      server, never through Accessibility (not with
//                                      --delay-ms, --stagger-ms or --minimized)
//   --child      | FIXTURE_CHILD=1     window 1 gets a popover-like child window (a borderless
//                                      panel, addChildWindow) sticking out 150 pt past its
//                                      right edge, 60 pt in from its top and bottom: the
//                                      capture phase's card shows it beside the window
// A bare number argument is the same as --windows N.
// SIGUSR1 opens one more standard window ("T N+1"), ordered front without activating the app.
// SIGUSR2 minimizes the first visible standard window (as ⌘M would).
// SIGHUP un-minimizes the first minimized standard window (by title), as a Dock click would.
// SIGWINCH stands in for "its Desktop is not shown": every visible standard window parked
// in the screen's bottom-right corner is ordered out (the window server still lists it, off
// screen, with its frame; Accessibility no longer does). SIGINFO orders them in again
// behind the other windows at the same frames (their Desktop shows), without activating.
// SIGIO selects the next tab of every tab group (as ⌘⇧] would), without activating: the tab
// selected before becomes a hidden tab where it was.
import AppKit

struct Options {
    var windows = 4
    var panel = false
    var background = false
    var title = "Fixture"
    var frames: [[Double]] = []
    var styles: [String] = []
    var delayMs = 0
    var staggerMs = 0
    var reverse = false
    var retitle: [String: String] = [:]
    var retitleAfterMs = 0
    var minimized: Set<String> = []
    var tabbed = 0
    var child = false

    static func parseRetitle(_ text: String) -> [String: String] {
        var map: [String: String] = [:]
        for part in text.split(separator: ";") {
            let kv = part.split(separator: "=", maxSplits: 1).map(String.init)
            if kv.count == 2, !kv[0].isEmpty { map[kv[0]] = kv[1] }
        }
        return map
    }

    static func parseFrames(_ text: String) -> [[Double]] {
        text.split(separator: ";").compactMap { part in
            let v = part.split(separator: ",").compactMap { Double($0.trimmingCharacters(in: .whitespaces)) }
            return v.count == 4 ? v : nil
        }
    }

    static func parse() -> Options {
        var o = Options()
        let env = ProcessInfo.processInfo.environment
        if let n = env["FIXTURE_WINDOWS"].flatMap(Int.init) { o.windows = n }
        if env["FIXTURE_PANEL"] == "1" { o.panel = true }
        if env["FIXTURE_BACKGROUND"] == "1" { o.background = true }
        if let t = env["FIXTURE_TITLE"], !t.isEmpty { o.title = t }
        if let f = env["FIXTURE_FRAMES"] { o.frames = parseFrames(f) }
        if let s = env["FIXTURE_STYLES"] { o.styles = s.split(separator: ",").map(String.init) }
        if let n = env["FIXTURE_DELAY_MS"].flatMap(Int.init) { o.delayMs = n }
        if let n = env["FIXTURE_STAGGER_MS"].flatMap(Int.init) { o.staggerMs = n }
        if env["FIXTURE_REVERSE"] == "1" { o.reverse = true }
        if let r = env["FIXTURE_RETITLE"] { o.retitle = parseRetitle(r) }
        if let n = env["FIXTURE_RETITLE_AFTER_MS"].flatMap(Int.init) { o.retitleAfterMs = n }
        if let m = env["FIXTURE_MINIMIZED"] { o.minimized = Set(m.split(separator: ";").map(String.init)) }
        if let n = env["FIXTURE_TABBED"].flatMap(Int.init) { o.tabbed = n }
        if env["FIXTURE_CHILD"] == "1" { o.child = true }
        var args = CommandLine.arguments.dropFirst().makeIterator()
        while let a = args.next() {
            switch a {
            case "--windows": if let n = args.next().flatMap(Int.init) { o.windows = n }
            case "--panel": o.panel = true
            case "--background": o.background = true
            case "--title": if let t = args.next() { o.title = t }
            case "--frames": if let f = args.next() { o.frames = parseFrames(f) }
            case "--styles": if let s = args.next() { o.styles = s.split(separator: ",").map(String.init) }
            case "--delay-ms": if let n = args.next().flatMap(Int.init) { o.delayMs = n }
            case "--stagger-ms": if let n = args.next().flatMap(Int.init) { o.staggerMs = n }
            case "--reverse": o.reverse = true
            case "--retitle": if let r = args.next() { o.retitle = parseRetitle(r) }
            case "--retitle-after-ms": if let n = args.next().flatMap(Int.init) { o.retitleAfterMs = n }
            case "--minimized": if let m = args.next() { o.minimized = Set(m.split(separator: ";").map(String.init)) }
            case "--tabbed": if let n = args.next().flatMap(Int.init) { o.tabbed = n }
            case "--child": o.child = true
            default: if let n = Int(a) { o.windows = n }
            }
        }
        o.windows = max(0, min(o.windows, 12))
        o.delayMs = max(0, o.delayMs)
        o.staggerMs = max(0, o.staggerMs)
        o.retitleAfterMs = max(0, o.retitleAfterMs)
        o.tabbed = max(0, min(o.tabbed, o.windows - 1))
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
        let all = options.reverse ? Array((0..<options.windows).reversed()) : Array(0..<options.windows)
        // Windows reopened minimized go to the Dock at once; the others follow the delays.
        for i in all where options.minimized.contains(title(i)) { open(i, area: area) }
        let order = all.filter { !options.minimized.contains(title($0)) }
        if options.delayMs == 0 && options.staggerMs == 0 {
            for i in order { open(i, area: area) }
            if options.tabbed > 0 && options.minimized.isEmpty { joinTabs() }
        } else {
            for (k, i) in order.enumerated() {
                let ms = options.delayMs + k * options.staggerMs
                DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(ms)) { self.open(i, area: area) }
            }
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
        if options.child, let parent = windows.first(where: { $0.title == title(0) }) {
            addPopup(to: parent)
        }
        if !options.background {
            NSApp.activate(ignoringOtherApps: true)
        }
    }

    /// `--child`: a popover-like child of `parent` sticking out past its right edge. A panel,
    /// so the signals' standard-window filters leave it alone; it moves with its parent.
    func addPopup(to parent: NSWindow) {
        let f = parent.frame
        let popup = NSPanel(
            contentRect: NSRect(x: f.maxX - 100, y: f.minY + 60, width: 250, height: max(40, f.height - 120)),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered, defer: false)
        popup.isFloatingPanel = false
        popup.hidesOnDeactivate = false
        popup.isReleasedWhenClosed = false
        popup.hasShadow = true
        popup.contentView = LabelView(frame: .zero, color: .systemTeal, label: "Popup")
        parent.addChildWindow(popup, ordered: .above)
    }

    var opened = 0

    /// The title of window number i + 1.
    func title(_ i: Int) -> String {
        "\(options.title) \(i + 1)"
    }

    /// Window number i + 1, shown (or, when `--minimized` names it, ordered in behind the
    /// others and minimized), then retitled when `--retitle` names its title.
    func open(_ i: Int, area: NSRect) {
        let window = makeWindow(i, area: area)
        if options.minimized.contains(window.title) {
            // Ordered in first: `miniaturize` on a window never on screen left it unknown to
            // Accessibility and not minimized (the 10-15 live run). A moment later, so the
            // window server has shown it, it goes to the Dock.
            window.orderBack(nil)
            DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(250)) { window.miniaturize(nil) }
        } else {
            show(window)
        }
        guard let title = options.retitle[window.title] else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(options.retitleAfterMs)) { window.title = title }
    }

    /// Window number i + 1: distinct frames, a diagonal cascade with varied sizes.
    func makeWindow(_ i: Int, area: NSRect) -> NSWindow {
        let size = NSSize(width: 520, height: 360)
        let w = size.width + CGFloat(i % 3) * 40
        let h = size.height + CGFloat(i % 2) * 30
        let dx = CGFloat(i % 6) * 110, dy = CGFloat(i % 6) * 70 + CGFloat(i / 6) * 30
        var rect = NSRect(x: area.minX + 80 + dx, y: area.maxY - 80 - h - dy, width: w, height: h)
        if i < options.frames.count {
            let f = options.frames[i]
            let fw = f[2] <= 0 ? area.width + f[2] : f[2]
            rect = NSRect(x: area.minX + f[0], y: area.maxY - f[1] - f[3], width: fw, height: f[3])
        }
        let name = title(i)
        let window = NSWindow(
            contentRect: rect,
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = name
        window.isReleasedWhenClosed = false
        window.contentView = LabelView(frame: .zero, color: palette[i % palette.count], label: name)
        if !options.styles.isEmpty {
            applyStyle(options.styles[i % options.styles.count], to: window)
        }
        windows.append(window)
        opened = max(opened, i + 1)
        return window
    }

    /// Window chrome by name (`--styles`).
    func applyStyle(_ style: String, to window: NSWindow) {
        func toolbar() {
            let t = NSToolbar(identifier: "fixture.\(style)")
            t.displayMode = .iconOnly
            window.toolbar = t
        }
        switch style {
        case "toolbar":
            toolbar()
            window.toolbarStyle = .unified
        case "compact":
            toolbar()
            window.toolbarStyle = .unifiedCompact
        case "expanded":
            toolbar()
            window.toolbarStyle = .expanded
        case "fullsize":
            window.styleMask.insert(.fullSizeContentView)
            window.titlebarAppearsTransparent = true
            window.titleVisibility = .hidden
        case "titleless":
            toolbar()
            window.toolbarStyle = .unified
            window.titleVisibility = .hidden
        default:
            break
        }
        window.title = "\(window.title) \(style)"
    }

    /// `--tabbed N`: the last N standard windows join window 1's tab group; window 1 stays the
    /// selected tab, so the others are hidden tabs.
    func joinTabs() {
        let standard = windows.filter { !($0 is NSPanel) }.sorted { $0.title < $1.title }
        guard let first = standard.first(where: { $0.title == title(0) }) else { return }
        first.tabbingMode = .preferred
        for i in (options.windows - options.tabbed)..<options.windows {
            guard let tab = standard.first(where: { $0.title == title(i) }), tab !== first else { continue }
            tab.tabbingMode = .preferred
            first.addTabbedWindow(tab, ordered: .above)
        }
        first.tabGroup?.selectedWindow = first
    }

    /// SIGIO: the next tab of every tab group is selected.
    func selectNextTabs() {
        var seen = Set<ObjectIdentifier>()
        for w in windows {
            guard let group = w.tabGroup, group.windows.count > 1, seen.insert(ObjectIdentifier(group)).inserted,
                  let selected = group.selectedWindow, let k = group.windows.firstIndex(of: selected) else { continue }
            group.selectedWindow = group.windows[(k + 1) % group.windows.count]
        }
    }

    /// SIGUSR1: one more window, ordered front without activating the app.
    func openAnother() {
        let area = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
        makeWindow(opened, area: area).orderFront(nil)
    }

    /// SIGUSR2: minimize the first visible standard window.
    func minimizeOne() {
        let wins = NSApp.windows.filter { $0.isVisible && !$0.isMiniaturized && !($0 is NSPanel) }
        wins.sorted { $0.title < $1.title }.first?.miniaturize(nil)
    }

    /// SIGHUP: un-minimize the first minimized standard window.
    func unminimizeOne() {
        let wins = NSApp.windows.filter { $0.isMiniaturized && !($0 is NSPanel) }
        wins.sorted { $0.title < $1.title }.first?.deminiaturize(nil)
    }

    var away: [(NSWindow, NSRect)] = []

    /// SIGWINCH: windows parked in the bottom-right corner go "to another Desktop".
    func sendParkedAway() {
        guard let screen = NSScreen.main?.frame else { return }
        let parked = NSApp.windows.filter { w in
            w.isVisible && !(w is NSPanel) && w.frame.minX >= screen.maxX - 64 && w.frame.maxY <= screen.minY + 64
        }
        for w in parked { w.orderOut(nil) }
        away += parked.map { ($0, $0.frame) }
    }

    /// SIGINFO: the windows sent away show again ("their Desktop shows").
    func bringAwayBack() {
        // Ordering in constrains a window onto the screen; a Desktop switch never moves it.
        for (w, frame) in away {
            w.orderBack(nil)
            w.setFrame(frame, display: false)
        }
        away = []
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
signal(SIGUSR1, SIG_IGN)
let another = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
another.setEventHandler { delegate.openAnother() }
another.resume()
signal(SIGUSR2, SIG_IGN)
let minimize = DispatchSource.makeSignalSource(signal: SIGUSR2, queue: .main)
minimize.setEventHandler { delegate.minimizeOne() }
minimize.resume()
signal(SIGHUP, SIG_IGN)
let unminimize = DispatchSource.makeSignalSource(signal: SIGHUP, queue: .main)
unminimize.setEventHandler { delegate.unminimizeOne() }
unminimize.resume()
signal(SIGWINCH, SIG_IGN)
let sendAway = DispatchSource.makeSignalSource(signal: SIGWINCH, queue: .main)
sendAway.setEventHandler { delegate.sendParkedAway() }
sendAway.resume()
signal(SIGINFO, SIG_IGN)
let comeBack = DispatchSource.makeSignalSource(signal: SIGINFO, queue: .main)
comeBack.setEventHandler { delegate.bringAwayBack() }
comeBack.resume()
signal(SIGIO, SIG_IGN)
let nextTab = DispatchSource.makeSignalSource(signal: SIGIO, queue: .main)
nextTab.setEventHandler { delegate.selectNextTabs() }
nextTab.resume()
app.run()
