// A virtual screen for live tests: a screen that comes and goes on demand, so a test can take
// a screen away (as unplugging one does) without anyone at the Mac.
//
//   virtual-display [--size 1920x1080|main] [--main] [--pid-file F]   adds the screen, keeps
//                                       it while running; SIGTERM, SIGINT or `stop` removes it.
//                                       `--size main`: the main screen's size in points. Made
//                                       main, it takes the main screen's Desktops (macOS moves
//                                       them, windows and all, and gives the old main screen a
//                                       new Desktop), so at the same size no window is resized
//   virtual-display stop [--pid-file F] stops the running one (SIGTERM) and waits for it
//
// The screen is a CoreGraphics virtual display (CGVirtualDisplay, private: see
// CGVirtualDisplay.h for where the interface comes from). Once it is online, it is placed with
// the public display configuration calls (CGBeginDisplayConfiguration, CGConfigureDisplayOrigin,
// for this session only): with --main at 0,0 (the main screen, with the menu bar) and every
// other screen to its right, in their order; without it, right of the screens there were.
// One JSON line on stdout says it is ready: {"ready":true,"id":N,"bounds":[x,y,w,h],"main":B}.
//
// On exit the screen goes first, as an unplugged one does (a main one goes while main), then
// every screen still online is put back where it was before the run.
//
// Build: swiftc -O -import-objc-header CGVirtualDisplay.h main.swift -o virtual-display
// (scripts/virtual-display/build.sh puts it in target/).
import CoreGraphics
import Foundation

struct Options {
    var width: UInt32 = 1920
    var height: UInt32 = 1080
    var main = false
    var stop = false
    var pidFile = NSTemporaryDirectory() + "sm-virtual-display.pid"
}

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(("virtual-display: " + msg + "\n").data(using: .utf8)!)
    exit(1)
}

/// The arguments, one at a time, with a look at the next.
struct Peeking {
    var rest: [String]
    init(_ a: [String]) { rest = a }
    var peek: String? { rest.first }
    mutating func next() -> String? { rest.isEmpty ? nil : rest.removeFirst() }
}

func parse() -> Options {
    var o = Options()
    var args = Peeking(Array(CommandLine.arguments.dropFirst()))
    while let a = args.next() {
        switch a {
        case "stop": o.stop = true
        case "--main": o.main = true
        case "--pid-file": o.pidFile = args.next() ?? o.pidFile
        case "--size" where args.peek == "main":
            _ = args.next()
            let b = CGDisplayBounds(CGMainDisplayID())
            (o.width, o.height) = (UInt32(b.width), UInt32(b.height))
        case "--size":
            let parts = (args.next() ?? "").split(separator: "x").compactMap { UInt32($0) }
            guard parts.count == 2, parts[0] >= 640, parts[1] >= 480 else { fail("--size wants WxH, at least 640x480") }
            (o.width, o.height) = (parts[0], parts[1])
        default: fail("unknown argument \(a)")
        }
    }
    return o
}

/// Every online screen's id and origin (global points, the main screen's top-left at 0,0).
func onlineScreens() -> [(CGDirectDisplayID, CGPoint)] {
    var n: UInt32 = 0
    CGGetOnlineDisplayList(0, nil, &n)
    var ids = [CGDirectDisplayID](repeating: 0, count: Int(n))
    CGGetOnlineDisplayList(n, &ids, &n)
    return ids.prefix(Int(n)).map { ($0, CGDisplayBounds($0).origin) }
}

/// Places screens at the given origins, for this login session only (never saved).
func place(_ origins: [(CGDirectDisplayID, CGPoint)]) -> Bool {
    var config: CGDisplayConfigRef?
    guard CGBeginDisplayConfiguration(&config) == .success, let config else { return false }
    for (id, p) in origins {
        CGConfigureDisplayOrigin(config, id, Int32(p.x.rounded()), Int32(p.y.rounded()))
    }
    return CGCompleteDisplayConfiguration(config, .forSession) == .success
}

func waitUntil(_ seconds: Double, _ cond: () -> Bool) -> Bool {
    let end = Date().addingTimeInterval(seconds)
    while Date() < end {
        if cond() { return true }
        RunLoop.current.run(until: Date().addingTimeInterval(0.05))
    }
    return cond()
}

func stopRunning(_ o: Options) -> Never {
    guard let text = try? String(contentsOfFile: o.pidFile, encoding: .utf8),
          let pid = pid_t(text.trimmingCharacters(in: .whitespacesAndNewlines)), kill(pid, 0) == 0
    else {
        print("{\"stopped\":false,\"why\":\"none running\"}")
        exit(0)
    }
    kill(pid, SIGTERM)
    let gone = waitUntil(15) { kill(pid, 0) != 0 }
    print("{\"stopped\":\(gone),\"pid\":\(pid)}")
    exit(gone ? 0 : 1)
}

let options = parse()
if options.stop { stopRunning(options) }

let before = onlineScreens()
let mainBefore = CGMainDisplayID()

let descriptor = CGVirtualDisplayDescriptor()
// Its own queue: the exit runs on the main queue and waits for the screen to go.
descriptor.queue = DispatchQueue(label: "virtual-display")
descriptor.name = "SM Test Screen"
descriptor.maxPixelsWide = options.width
descriptor.maxPixelsHigh = options.height
// About 24 inches at 1920 wide (0.28 mm a pixel): the size only shapes the scale macOS offers.
descriptor.sizeInMillimeters = CGSize(width: Double(options.width) * 0.28, height: Double(options.height) * 0.28)
descriptor.vendorID = 0x5354  // made up: "ST"
descriptor.productID = 0x4D47
descriptor.serialNum = 0x534D_0001
descriptor.terminationHandler = { _, _ in
    FileHandle.standardError.write("virtual-display: the system ended the virtual screen\n".data(using: .utf8)!)
}

// Only `screen` holds the display: setting it to nil removes the screen.
var screen: CGVirtualDisplay? = CGVirtualDisplay(descriptor: descriptor)
// (a top-level `guard let` would be a global holding it too)
if screen == nil { fail("CGVirtualDisplay could not be made on this macOS") }
let settings = CGVirtualDisplaySettings()
settings.hiDPI = 0
settings.modes = [CGVirtualDisplayMode(width: options.width, height: options.height, refreshRate: 60)]
guard screen?.apply(settings) == true else { fail("the virtual screen refused its settings") }
let vid = screen?.displayID ?? 0
guard vid != 0, waitUntil(10, { onlineScreens().contains { $0.0 == vid } }) else {
    fail("the virtual screen (id \(vid)) never came online")
}

/// Removes the virtual screen (first, as an unplug), then puts every screen still online back.
func finish(_ code: Int32) -> Never {
    autoreleasepool { screen = nil }
    let gone = waitUntil(25) { !onlineScreens().contains { $0.0 == vid } }
    let left = Set(onlineScreens().map { $0.0 })
    let restored = place(before.filter { left.contains($0.0) })
    _ = waitUntil(1) { false }
    let mainOk = CGMainDisplayID() == mainBefore
    print("{\"removed\":\(gone),\"restored\":\(restored),\"main_as_before\":\(mainOk)}")
    try? FileManager.default.removeItem(atPath: options.pidFile)
    exit(code)
}

// Placement: --main puts it at 0,0 and the others to its right, in their order; otherwise it
// goes right of them, top-aligned with the main screen.
let others = before.sorted { $0.1.x < $1.1.x }
var origins: [(CGDirectDisplayID, CGPoint)] = []
if options.main {
    origins.append((vid, .zero))
    var x = CGFloat(options.width)
    for (id, _) in others {
        origins.append((id, CGPoint(x: x, y: 0)))
        x += CGDisplayBounds(id).width
    }
} else {
    let right = others.map { CGDisplayBounds($0.0).maxX }.max() ?? 0
    origins.append((vid, CGPoint(x: right, y: 0)))
}
if !place(origins) { finish(1) }
let placed = waitUntil(5) { options.main ? CGMainDisplayID() == vid : CGDisplayBounds(vid).origin.x > 0 }
let b = CGDisplayBounds(vid)
try? "\(getpid())\n".write(toFile: options.pidFile, atomically: true, encoding: .utf8)
print("{\"ready\":\(placed),\"id\":\(vid),\"bounds\":[\(Int(b.minX)),\(Int(b.minY)),\(Int(b.width)),\(Int(b.height))],\"main\":\(CGMainDisplayID() == vid)}")
fflush(stdout)

var signalSources: [DispatchSourceSignal] = []
for sig in [SIGTERM, SIGINT, SIGHUP] {
    signal(sig, SIG_IGN)
    let source = DispatchSource.makeSignalSource(signal: sig, queue: .main)
    source.setEventHandler { finish(0) }
    source.resume()
    signalSources.append(source)
}
dispatchMain()
