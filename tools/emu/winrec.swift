// Records one window (by CGWindowID) with ScreenCaptureKit, even when other windows cover it:
// `winrec WID FPS SECONDS OUTDIR` writes OUTDIR/f_NNNN.jpg (one per 1/FPS s from the first frame) and prints
// "start" on stdout when the first frame arrives, so a driver can time its inputs from that moment.
import CoreImage
import CoreMedia
import Foundation
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers

let args = CommandLine.arguments
guard args.count == 5, let wid = UInt32(args[1]), let fps = Double(args[2]), let secs = Double(args[3]) else {
    FileHandle.standardError.write("usage: winrec WID FPS SECONDS OUTDIR\n".data(using: .utf8)!)
    exit(2)
}
let outDir = URL(fileURLWithPath: args[4])
try? FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)

final class Sink: NSObject, SCStreamOutput {
    let ctx = CIContext()
    var t0: CMTime?
    var next = 0
    let step: Double
    let total: Int
    let done = DispatchSemaphore(value: 0)
    let queue = DispatchQueue(label: "write", qos: .userInitiated, attributes: .concurrent)
    let group = DispatchGroup()

    init(fps: Double, secs: Double) {
        step = 1.0 / fps
        total = Int(secs * fps)
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, next < total, let px = sb.imageBuffer else { return }
        // skip idle / blank frames (no pixel buffer status complete)
        if let att = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
           let raw = att.first?[.status] as? Int, let st = SCFrameStatus(rawValue: raw), st != .complete {
            return
        }
        let t = sb.presentationTimeStamp
        if t0 == nil {
            t0 = t
            print("start")
            fflush(stdout)
        }
        let el = CMTimeGetSeconds(CMTimeSubtract(t, t0!))
        // a frame stands for every output slot up to its time (the window repeats it while unchanged)
        let image = CIImage(cvPixelBuffer: px)
        guard let cg = ctx.createCGImage(image, from: image.extent) else { return }
        while next < total && Double(next) * step <= el + step * 0.5 {
            let url = outDir.appendingPathComponent(String(format: "f_%04d.jpg", next))
            next += 1
            group.enter()
            queue.async {
                if let dst = CGImageDestinationCreateWithURL(url as CFURL, UTType.jpeg.identifier as CFString, 1, nil) {
                    CGImageDestinationAddImage(dst, cg, [kCGImageDestinationLossyCompressionQuality: 0.92] as CFDictionary)
                    CGImageDestinationFinalize(dst)
                }
                self.group.leave()
            }
        }
        if next >= total { done.signal() }
    }
}

let sink = Sink(fps: fps, secs: secs)
var stream: SCStream?
let sem = DispatchSemaphore(value: 0)
SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { content, err in
    guard let content, let win = content.windows.first(where: { $0.windowID == wid }) else {
        FileHandle.standardError.write("window \(wid) not found: \(String(describing: err))\n".data(using: .utf8)!)
        exit(1)
    }
    let filter = SCContentFilter(desktopIndependentWindow: win)
    let cfg = SCStreamConfiguration()
    let scale = Double(filter.pointPixelScale)
    cfg.width = Int(win.frame.width * scale)
    cfg.height = Int(win.frame.height * scale)
    cfg.minimumFrameInterval = CMTime(value: 1, timescale: 60)
    cfg.showsCursor = false
    cfg.queueDepth = 8
    let s = SCStream(filter: filter, configuration: cfg, delegate: nil)
    do {
        try s.addStreamOutput(sink, type: .screen, sampleHandlerQueue: DispatchQueue(label: "frames"))
    } catch {
        FileHandle.standardError.write("addStreamOutput: \(error)\n".data(using: .utf8)!)
        exit(1)
    }
    stream = s
    s.startCapture { e in
        if let e {
            FileHandle.standardError.write("startCapture: \(e)\n".data(using: .utf8)!)
            exit(1)
        }
        sem.signal()
    }
}
sem.wait()
// an unchanging window sends no new frames: stop after the duration even if slots are left
_ = sink.done.wait(timeout: .now() + secs + 3)
let stopped = DispatchSemaphore(value: 0)
stream?.stopCapture { _ in stopped.signal() }
_ = stopped.wait(timeout: .now() + 2)
sink.group.wait()
// slots after the last change repeat the last frame
if sink.next > 0 {
    let last = outDir.appendingPathComponent(String(format: "f_%04d.jpg", sink.next - 1))
    for i in sink.next..<sink.total {
        try? FileManager.default.copyItem(at: last, to: outDir.appendingPathComponent(String(format: "f_%04d.jpg", i)))
    }
}
print("frames \(sink.total)")
