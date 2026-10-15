import AVFoundation
import CoreGraphics
import CoreImage
import CoreVideo
import Foundation
import ImageIO
import OpenChessCore
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Camera frames to images, pixels for the board reader, and JPEGs, with
/// CoreGraphics only (safe off the main thread).
enum FrameTools {
    static let context = CIContext()

    /// A video frame as an image, its longer side at most `maxSide`.
    static func cgImage(_ buffer: CVPixelBuffer, maxSide: CGFloat) -> CGImage? {
        var image = CIImage(cvPixelBuffer: buffer)
        let w = image.extent.width
        let h = image.extent.height
        guard w > 0, h > 0 else { return nil }
        let scale = min(1, maxSide / max(w, h))
        if scale < 1 {
            image = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        }
        return context.createCGImage(image, from: image.extent)
    }

    /// The image redrawn with its longer side at most `maxSide` (no metadata).
    static func scaled(_ image: CGImage, maxSide: Int) -> CGImage? {
        let w = image.width
        let h = image.height
        let scale = min(1.0, Double(maxSide) / Double(max(1, max(w, h))))
        let tw = max(1, Int((Double(w) * scale).rounded()))
        let th = max(1, Int((Double(h) * scale).rounded()))
        guard let ctx = CGContext(
            data: nil,
            width: tw,
            height: th,
            bitsPerComponent: 8,
            bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
        ) else { return nil }
        ctx.interpolationQuality = .medium
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: tw, height: th))
        return ctx.makeImage()
    }

    /// RGBA8 pixels, row by row, scaled so the longer side is at most `maxSide`.
    static func rgba(_ image: CGImage, maxSide: Int) -> (data: Data, width: Int, height: Int)? {
        let w = image.width
        let h = image.height
        let scale = min(1.0, Double(maxSide) / Double(max(1, max(w, h))))
        let tw = max(1, Int((Double(w) * scale).rounded()))
        let th = max(1, Int((Double(h) * scale).rounded()))
        let bytesPerRow = tw * 4
        var buffer = Data(count: bytesPerRow * th)
        let ok: Bool = buffer.withUnsafeMutableBytes { raw -> Bool in
            guard let base = raw.baseAddress,
                  let ctx = CGContext(
                      data: base,
                      width: tw,
                      height: th,
                      bitsPerComponent: 8,
                      bytesPerRow: bytesPerRow,
                      space: CGColorSpaceCreateDeviceRGB(),
                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
                  ) else { return false }
            ctx.interpolationQuality = .medium
            ctx.draw(image, in: CGRect(x: 0, y: 0, width: tw, height: th))
            return true
        }
        guard ok else { return nil }
        return (data: buffer, width: tw, height: th)
    }

    /// A JPEG of the image, its longer side at most `maxSide`; re-encoding
    /// carries no metadata (EXIF, location).
    static func jpeg(_ image: CGImage, maxSide: Int, quality: Double) -> Data? {
        guard let small = scaled(image, maxSide: maxSide) else { return nil }
        let data = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(data as CFMutableData, UTType.jpeg.identifier as CFString, 1, nil) else {
            return nil
        }
        let options = [kCGImageDestinationLossyCompressionQuality as String: quality] as CFDictionary
        CGImageDestinationAddImage(dest, small, options)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return data as Data
    }

    /// The board reader's reading of an image (JSON), or nil. Slow: off
    /// the main thread.
    static func read(_ image: CGImage, reader: BoardReader) -> String? {
        let side = Int(reader.imageSize())
        guard let px = rgba(image, maxSide: max(side, 256)) else { return nil }
        return try? reader.readJson(rgba: px.data, width: UInt32(px.width), height: UInt32(px.height))
    }
}

/// Where camera frames go: a sample every so often for the board reader
/// (dropped while the last one is still being read), and single frames on
/// request (a photo). Thread-safe; frames arrive on the camera's queue.
final class FrameSink: @unchecked Sendable {
    typealias Sampler = @Sendable (CGImage, UInt64) -> Void

    private let lock = NSLock()
    private let queue = DispatchQueue(label: "org.offenedatenmodellierung.openchess.frames", qos: .userInitiated)
    private var busy = false
    private var lastSampleMs: UInt64 = 0
    private var interval: UInt64 = 0
    private var sampler: Sampler?
    private var waiters: [UUID: CheckedContinuation<CGImage?, Never>] = [:]

    /// Sample a frame every `interval` ms (0 or nil: none).
    func setSampler(interval ms: UInt64, _ sampler: Sampler?) {
        lock.lock()
        interval = ms
        self.sampler = sampler
        lock.unlock()
    }

    /// A camera frame (any queue).
    func push(_ buffer: CVPixelBuffer) {
        let now = nowMs()
        lock.lock()
        let wantSnapshot = !waiters.isEmpty
        let wantSample = sampler != nil && interval > 0 && now >= lastSampleMs + interval
        if busy || !(wantSnapshot || wantSample) {
            lock.unlock()
            return
        }
        busy = true
        if wantSample { lastSampleMs = now }
        let snaps = Array(waiters.values)
        waiters.removeAll()
        let run: Sampler? = wantSample ? sampler : nil
        lock.unlock()
        queue.async {
            let image = FrameTools.cgImage(buffer, maxSide: 1280)
            for c in snaps {
                c.resume(returning: image)
            }
            if let image, let run {
                run(image, now)
            }
            self.lock.lock()
            self.busy = false
            self.lock.unlock()
        }
    }

    /// The next frame (nil if none comes within `timeout` seconds).
    func snapshot(timeout: Double = 2) async -> CGImage? {
        let token = UUID()
        return await withCheckedContinuation { (c: CheckedContinuation<CGImage?, Never>) in
            self.lock.lock()
            self.waiters[token] = c
            self.lock.unlock()
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout) {
                self.lock.lock()
                let w = self.waiters.removeValue(forKey: token)
                self.lock.unlock()
                w?.resume(returning: nil)
            }
        }
    }
}

/// The back camera with AVFoundation, when not streaming: a preview, and
/// frames into a `FrameSink`.
final class CameraFeed: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate, @unchecked Sendable {
    let session = AVCaptureSession()
    let sink: FrameSink
    private let queue = DispatchQueue(label: "org.offenedatenmodellierung.openchess.camera")
    private let output = AVCaptureVideoDataOutput()
    private var configured = false

    init(sink: FrameSink) {
        self.sink = sink
        super.init()
    }

    static var available: Bool {
        AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back) != nil
    }

    /// Ask for camera access if not yet answered.
    static func authorize() async -> Bool {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            return true
        case .notDetermined:
            return await AVCaptureDevice.requestAccess(for: .video)
        default:
            return false
        }
    }

    /// The rotation for upright frames with the interface this way round.
    static func angle(for orientation: UIInterfaceOrientation) -> CGFloat {
        switch orientation {
        case .landscapeRight: return 0
        case .landscapeLeft: return 180
        case .portraitUpsideDown: return 270
        default: return 90
        }
    }

    /// Start the camera; an error message if it can't.
    func start() async -> String? {
        await withCheckedContinuation { (c: CheckedContinuation<String?, Never>) in
            queue.async {
                c.resume(returning: self.startOnQueue())
            }
        }
    }

    private func startOnQueue() -> String? {
        if !configured {
            guard let device = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back) else {
                return "This device has no back camera."
            }
            do {
                let input = try AVCaptureDeviceInput(device: device)
                session.beginConfiguration()
                if session.canSetSessionPreset(.hd1920x1080) {
                    session.sessionPreset = .hd1920x1080
                }
                if session.canAddInput(input) {
                    session.addInput(input)
                }
                output.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: Int(kCVPixelFormatType_32BGRA)]
                output.alwaysDiscardsLateVideoFrames = true
                output.setSampleBufferDelegate(self, queue: queue)
                if session.canAddOutput(output) {
                    session.addOutput(output)
                }
                session.commitConfiguration()
                configured = true
            } catch {
                return "The camera couldn't start: \(error.localizedDescription)"
            }
        }
        if !session.isRunning {
            session.startRunning()
        }
        return nil
    }

    func stop() {
        queue.async {
            if self.session.isRunning {
                self.session.stopRunning()
            }
        }
    }

    /// Turn frames upright for the interface orientation.
    func setRotation(_ angle: CGFloat) {
        queue.async {
            if let c = self.output.connection(with: .video), c.isVideoRotationAngleSupported(angle) {
                c.videoRotationAngle = angle
            }
        }
    }

    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        guard let buffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        sink.push(buffer)
    }
}

/// The camera's preview layer in SwiftUI.
@MainActor
struct CameraPreview: UIViewRepresentable {
    let session: AVCaptureSession

    func makeUIView(context: Context) -> CameraPreviewView {
        let v = CameraPreviewView()
        v.previewLayer.session = session
        v.previewLayer.videoGravity = .resizeAspect
        v.backgroundColor = .black
        return v
    }

    func updateUIView(_ uiView: CameraPreviewView, context: Context) {
        uiView.setNeedsLayout()
    }
}

final class CameraPreviewView: UIView {
    override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }

    var previewLayer: AVCaptureVideoPreviewLayer {
        // swiftlint:disable:next force_cast
        layer as! AVCaptureVideoPreviewLayer
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        if let orientation = window?.windowScene?.effectiveGeometry.interfaceOrientation, let c = previewLayer.connection {
            let a = CameraFeed.angle(for: orientation)
            if c.isVideoRotationAngleSupported(a) {
                c.videoRotationAngle = a
            }
        }
    }
}

/// The interface orientation of the app's window now.
@MainActor
func currentInterfaceOrientation() -> UIInterfaceOrientation {
    let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
    return scenes.first?.effectiveGeometry.interfaceOrientation ?? .portrait
}
