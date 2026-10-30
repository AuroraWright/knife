// wingrab WID OUT.png [N]: N (default 1) frames of one window via ScreenCaptureKit as 64-bit half-float RGBA in
// sRGB (no 8-bit display-space step), each rounded to 8-bit sRGB: OUT_k.png.
import CoreImage
import CoreMedia
import Foundation
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers

let args = CommandLine.arguments
guard args.count >= 3, let wid = UInt32(args[1]) else { exit(2) }
let out = args[2]
let n = args.count > 3 ? Int(args[3])! : 1
let srgb = CGColorSpace(name: CGColorSpace.sRGB)!

final class Sink: NSObject, SCStreamOutput {
    let ctx = CIContext(options: [.workingColorSpace: NSNull(), .outputColorSpace: NSNull()])
    var got = 0
    let done = DispatchSemaphore(value: 0)
    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, got < n, let px = sb.imageBuffer else { return }
        if let att = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
           let raw = att.first?[.status] as? Int, let st = SCFrameStatus(rawValue: raw), st != .complete { return }
        CVPixelBufferLockBaseAddress(px, .readOnly)
        let w = CVPixelBufferGetWidth(px), h = CVPixelBufferGetHeight(px), bpr = CVPixelBufferGetBytesPerRow(px)
        let base = CVPixelBufferGetBaseAddress(px)!
        var rgb = [UInt8](repeating: 0, count: w * h * 3)
        for y in 0..<h {
            let row = (base + y * bpr).assumingMemoryBound(to: Float16.self)
            for x in 0..<w {
                for c in 0..<3 {
                    let v = Float(row[x * 4 + c])
                    rgb[(y * w + x) * 3 + c] = UInt8(max(0, min(255, (v * 255).rounded())))
                }
            }
        }
        CVPixelBufferUnlockBaseAddress(px, .readOnly)
        let prov = CGDataProvider(data: Data(rgb) as CFData)!
        let cg = CGImage(width: w, height: h, bitsPerComponent: 8, bitsPerPixel: 24, bytesPerRow: w * 3, space: srgb,
                         bitmapInfo: CGBitmapInfo(rawValue: 0), provider: prov, decode: nil, shouldInterpolate: false,
                         intent: .defaultIntent)!
        let url = URL(fileURLWithPath: n == 1 ? out : out.replacingOccurrences(of: ".png", with: String(format: "_%03d.png", got)))
        let dst = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil)!
        CGImageDestinationAddImage(dst, cg, nil)
        CGImageDestinationFinalize(dst)
        got += 1
        if got >= n { done.signal() }
    }
}
let sink = Sink()
var stream: SCStream?
let sem = DispatchSemaphore(value: 0)
SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { content, err in
    guard let content, let win = content.windows.first(where: { $0.windowID == wid }) else { exit(1) }
    let filter = SCContentFilter(desktopIndependentWindow: win)
    let cfg = SCStreamConfiguration()
    let scale = Double(filter.pointPixelScale)
    cfg.width = Int(win.frame.width * scale)
    cfg.height = Int(win.frame.height * scale)
    cfg.pixelFormat = kCVPixelFormatType_64RGBAHalf
    cfg.colorSpaceName = CGColorSpace.sRGB
    cfg.minimumFrameInterval = CMTime(value: 1, timescale: 60)
    cfg.showsCursor = false
    let s = SCStream(filter: filter, configuration: cfg, delegate: nil)
    try! s.addStreamOutput(sink, type: .screen, sampleHandlerQueue: DispatchQueue(label: "f"))
    stream = s
    s.startCapture { e in if e != nil { exit(1) }; sem.signal() }
}
sem.wait()
_ = sink.done.wait(timeout: .now() + 5)
let st = DispatchSemaphore(value: 0)
stream?.stopCapture { _ in st.signal() }
_ = st.wait(timeout: .now() + 2)
print("frames \(sink.got)")
