// Frames a full-bleed icon render (Icon Composer's export, the rounded square filling the
// canvas) on the classic macOS icon grid: the shape at 824/1024 of the canvas, centred, with
// the standard soft drop shadow. Writes an `.iconset` folder for `iconutil`.
//
//     icon_frame <render-1024.png> <out.iconset>
//
// Used by scripts/icon.sh for the fallback AppIcon.icns (macOS before 26 reads it).
import AppKit

let args = CommandLine.arguments
guard args.count == 3, let source = NSImage(contentsOfFile: args[1]) else {
    FileHandle.standardError.write("usage: icon_frame <render-1024.png> <out.iconset>\n".data(using: .utf8)!)
    exit(2)
}
let out = URL(fileURLWithPath: args[2])
try? FileManager.default.removeItem(at: out)
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

/// Draws the framed icon into a square bitmap `px` pixels wide.
func framed(_ px: Int) -> NSBitmapImageRep {
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8, samplesPerPixel: 4,
        hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    let n = CGFloat(px)
    rep.size = NSSize(width: n, height: n)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    NSGraphicsContext.current?.imageInterpolation = .high
    let side = n * 824 / 1024
    let rect = NSRect(x: (n - side) / 2, y: (n - side) / 2 + n * 4 / 1024, width: side, height: side)
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.3)
    shadow.shadowBlurRadius = n * 20 / 1024
    shadow.shadowOffset = NSSize(width: 0, height: -n * 10 / 1024)
    shadow.set()
    source.draw(in: rect, from: .zero, operation: .sourceOver, fraction: 1)
    NSGraphicsContext.restoreGraphicsState()
    return rep
}

for (points, scales) in [(16, [1, 2]), (32, [1, 2]), (128, [1, 2]), (256, [1, 2]), (512, [1, 2])] {
    for scale in scales {
        let name = scale == 1 ? "icon_\(points)x\(points).png" : "icon_\(points)x\(points)@2x.png"
        let data = framed(points * scale).representation(using: .png, properties: [:])!
        try data.write(to: out.appendingPathComponent(name))
    }
}
