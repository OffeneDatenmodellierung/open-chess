import AVFoundation
import HaishinKit
import RTMPHaishinKit
import SwiftUI
import UIKit

// Streaming to Twitch, YouTube or any RTMP server with HaishinKit 2.2.5
// (BSD-3-Clause): the back camera full frame with the board cutout drawn
// over it (the mixer's offscreen screen), microphone audio, and optionally
// a copy recorded on the phone. This is the only file that uses HaishinKit.

/// The camera's own frames (track 0, before the cutout is drawn), for the
/// board reader.
final class RawFrameTap: MediaMixerOutput, @unchecked Sendable {
    let sink: FrameSink

    init(sink: FrameSink) {
        self.sink = sink
    }

    var videoTrackId: UInt8? { 0 }
    var audioTrackId: UInt8? { nil }

    func mixer(_ mixer: MediaMixer, didOutput sampleBuffer: CMSampleBuffer) {
        guard let buffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        sink.push(buffer)
    }

    func mixer(_ mixer: MediaMixer, didOutput buffer: AVAudioPCMBuffer, when: AVAudioTime) {}

    func selectTrack(_ id: UInt8?, mediaType: CMFormatDescription.MediaType) async {}
}

@MainActor
final class StreamController: ObservableObject {
    enum State: Equatable {
        case idle
        case connecting
        case live
        case failed(String)
    }

    @Published private(set) var state: State = .idle
    @Published private(set) var startedAt: Date?
    @Published private(set) var recordingNote: String?
    @Published private(set) var cameraError: String?

    let mixer = MediaMixer(captureSessionMode: .single)
    let portrait: Bool
    private let tap: RawFrameTap
    private let recorder = StreamRecorder()
    private var session: (any Session)?
    private var readyTask: Task<Void, Never>?
    private var cameraRunning = false
    private var recording = false
    @ScreenActor private var cutoutObject: ImageScreenObject?

    init(sink: FrameSink, portrait: Bool) {
        tap = RawFrameTap(sink: sink)
        self.portrait = portrait
    }

    var screenSize: CGSize {
        portrait ? CGSize(width: 720, height: 1280) : CGSize(width: 1280, height: 720)
    }

    /// The cutout's width in points (= pixels): about 30% of the picture.
    var cutoutWidth: CGFloat { portrait ? 324 : 384 }

    var isActive: Bool {
        switch state {
        case .connecting, .live: return true
        default: return false
        }
    }

    var stateText: String {
        switch state {
        case .idle: return "Not streaming"
        case .connecting: return "Connecting…"
        case .live: return "Live"
        case .failed(let m): return m
        }
    }

    // MARK: Camera

    /// Start the camera and the mixer (the preview shows the mixed picture).
    func startCamera() async {
        guard !cameraRunning else { return }
        cameraRunning = true
        let mixer = self.mixer
        var settings = await mixer.videoMixerSettings
        settings.mode = .offscreen
        await mixer.setVideoMixerSettings(settings)
        await mixer.setVideoOrientation(videoOrientation())
        let back = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back)
        do {
            try await mixer.attachVideo(back, track: 0)
        } catch {
            cameraError = "The camera couldn't start: \(error.localizedDescription)"
        }
        do {
            try await mixer.attachAudio(AVCaptureDevice.default(for: .audio))
        } catch {
            // Streams without sound rather than not at all.
        }
        await mixer.addOutput(tap)
        await mixer.startRunning()
        let size = screenSize
        Task { @ScreenActor in
            await mixer.screen.size = size
            await mixer.screen.backgroundColor = UIColor.black.cgColor
            if self.cutoutObject == nil {
                let object = ImageScreenObject()
                object.horizontalAlignment = .right
                object.verticalAlignment = .top
                object.layoutMargin = .init(top: 16, left: 0, bottom: 0, right: 16)
                self.cutoutObject = object
                try? await mixer.screen.addChild(object)
            }
        }
    }

    func stopCamera() async {
        guard cameraRunning else { return }
        cameraRunning = false
        await mixer.removeOutput(tap)
        await mixer.stopRunning()
        try? await mixer.attachAudio(nil)
        try? await mixer.attachVideo(nil, track: 0)
    }

    /// Follow the interface orientation (landscape streams turn with the phone).
    func orientationChanged() {
        let mixer = self.mixer
        let o = videoOrientation()
        Task {
            await mixer.setVideoOrientation(o)
        }
    }

    private func videoOrientation() -> AVCaptureVideoOrientation {
        if portrait { return .portrait }
        return currentInterfaceOrientation() == .landscapeLeft ? .landscapeLeft : .landscapeRight
    }

    /// The board cutout, drawn over the camera picture.
    func setCutout(_ image: CGImage?) {
        Task { @ScreenActor in
            self.cutoutObject?.cgImage = image
        }
    }

    // MARK: Streaming

    func startStream(url: URL, bitrate: Int, record: Bool) async {
        guard !isActive else { return }
        state = .connecting
        await SessionBuilderFactory.shared.register(RTMPSessionFactory())
        do {
            guard let s = try await SessionBuilderFactory.shared.make(url).setMode(.publish).build() else {
                state = .failed("Couldn't make a stream for that address.")
                return
            }
            session = s
            let stream = await s.stream
            var video = await stream.videoSettings
            video.videoSize = screenSize
            video.bitRate = bitrate
            try await stream.setVideoSettings(video)
            var audio = await stream.audioSettings
            audio.bitRate = 128_000
            try await stream.setAudioSettings(audio)
            await mixer.addOutput(stream)
            readyTask?.cancel()
            readyTask = Task { [weak self] in
                for await ready in await s.readyState {
                    self?.readyChanged(ready)
                }
            }
            let onClosed: @Sendable () -> Void = { [weak self] in
                guard let controller = self else { return }
                Task { @MainActor in
                    controller.disconnected()
                }
            }
            try await s.connect(onClosed)
            if record {
                await startRecording()
            }
        } catch {
            state = .failed("The stream didn't start: \(describe(error))")
            await tearDownSession()
        }
    }

    func stopStream() async {
        await stopRecording()
        await tearDownSession()
        state = .idle
        startedAt = nil
        UIApplication.shared.isIdleTimerDisabled = false
    }

    private func tearDownSession() async {
        readyTask?.cancel()
        readyTask = nil
        guard let s = session else { return }
        session = nil
        try? await s.close()
        let stream = await s.stream
        await mixer.removeOutput(stream)
    }

    private func readyChanged(_ ready: SessionReadyState) {
        switch ready {
        case .connecting:
            state = .connecting
        case .open:
            state = .live
            if startedAt == nil { startedAt = Date() }
            UIApplication.shared.isIdleTimerDisabled = true
        case .closing:
            break
        case .closed:
            if state == .live || state == .connecting {
                state = .idle
                startedAt = nil
            }
        }
    }

    private func disconnected() {
        if state == .live {
            state = .failed("The stream was disconnected.")
        }
        startedAt = nil
        Task { await self.stopRecording() }
    }

    // MARK: Recording a copy

    private func startRecording() async {
        guard !recording else { return }
        await mixer.addOutput(recorder)
        let url = Recordings.newURL()
        do {
            try await recorder.startRecording(url)
            recording = true
            recordingNote = "Recording a copy on this phone."
        } catch {
            await mixer.removeOutput(recorder)
            recordingNote = "The copy couldn't be recorded: \(describe(error))"
        }
    }

    private func stopRecording() async {
        guard recording else { return }
        recording = false
        do {
            let url = try await recorder.stopRecording()
            recordingNote = "Saved \(url.lastPathComponent) (Streaming settings → Recordings)."
        } catch {
            recordingNote = "The copy couldn't be finished: \(describe(error))"
        }
        await mixer.removeOutput(recorder)
    }
}

/// The mixed picture (camera and cutout) as the stream sends it.
@MainActor
struct StreamPreview: UIViewRepresentable {
    let controller: StreamController

    final class Coordinator {
        let mixer: MediaMixer

        init(mixer: MediaMixer) {
            self.mixer = mixer
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(mixer: controller.mixer)
    }

    func makeUIView(context: Context) -> MTHKView {
        let view = MTHKView(frame: .zero)
        view.videoGravity = .resizeAspect
        let mixer = context.coordinator.mixer
        Task {
            await mixer.addOutput(view)
        }
        return view
    }

    func updateUIView(_ uiView: MTHKView, context: Context) {}

    static func dismantleUIView(_ uiView: MTHKView, coordinator: Coordinator) {
        let mixer = coordinator.mixer
        Task {
            await mixer.removeOutput(uiView)
        }
    }
}
