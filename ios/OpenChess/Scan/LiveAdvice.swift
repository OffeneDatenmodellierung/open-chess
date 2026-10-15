import OpenChessCore
import SwiftUI

/// Live advice on the scan screen: the camera stays on, the board is read
/// about once a second (frames dropped while busy), and a new plausible
/// position goes into the editor and gets fresh advice. The side to move
/// stays as chosen unless the new position is one legal move from the
/// last (then it's the other side's turn). Its own screen: never attached
/// to a game.
@MainActor
final class LiveAdviceModel: ObservableObject {
    @Published private(set) var watching = false
    @Published private(set) var lastUpdate: Date?
    @Published private(set) var note: String?
    @Published private(set) var cameraError: String?

    let sink = FrameSink()
    let camera: CameraFeed
    private weak var editor: BoardEditorModel?
    private var lastFen: String?

    init() {
        camera = CameraFeed(sink: sink)
    }

    func start(app: AppModel, editor: BoardEditorModel) {
        guard !watching else { return }
        watching = true
        self.editor = editor
        lastFen = nil
        note = nil
        Task {
            let (reader, problem) = await app.boardReader()
            guard let reader else {
                self.note = problem ?? "This build has no board reader."
                self.watching = false
                return
            }
            guard await CameraFeed.authorize() else {
                self.cameraError = "Camera access is off for open-chess in Settings."
                self.watching = false
                return
            }
            guard self.watching else { return }
            self.camera.setRotation(CameraFeed.angle(for: currentInterfaceOrientation()))
            self.cameraError = await self.camera.start()
            self.sink.setSampler(interval: 1000) { [weak self] image, _ in
                guard let json = FrameTools.read(image, reader: reader),
                      let r = BoardReadingResult.decode(json), r.plausible else { return }
                let placement = r.placement
                Task { @MainActor in
                    self?.seen(placement)
                }
            }
        }
    }

    func stop() {
        watching = false
        sink.setSampler(interval: 0, nil)
        camera.stop()
    }

    func orientationChanged() {
        camera.setRotation(CameraFeed.angle(for: currentInterfaceOrientation()))
    }

    private func seen(_ placementText: String) {
        guard watching, let editor else { return }
        let placement = Placement(fen: placementText)
        guard placement != editor.placement || lastFen == nil else { return }
        var whiteToMove = editor.whiteToMove
        if let last = lastFen, PositionTools.move(from: last, to: placement) != nil {
            // One legal move from the last position: the other side's turn now.
            let lastWhite = last.split(separator: " ").dropFirst().first.map(String.init) != "b"
            whiteToMove = !lastWhite
        }
        editor.placement = placement
        editor.unsure = []
        editor.whiteToMove = whiteToMove
        if let fen = completeFen(placement: placement.fenField, whiteToMove: whiteToMove), checkFen(fen: fen) == nil {
            lastFen = fen
            lastUpdate = Date()
            editor.adviceRequest += 1
        }
    }
}

/// The camera and the live advice status, above the editor.
@MainActor
struct LiveAdviceHeader: View {
    @ObservedObject var live: LiveAdviceModel

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            CameraPreview(session: live.camera.session)
                .aspectRatio(4.0 / 3.0, contentMode: .fit)
                .frame(maxHeight: 260)
                .clipShape(RoundedRectangle(cornerRadius: OCMetrics.corner))
            HStack(spacing: 6) {
                if live.watching {
                    ProgressView().controlSize(.small)
                    Text("Watching the board…").font(OCFont.text(13, weight: .semibold))
                }
                Spacer()
                if let t = live.lastUpdate {
                    Text("Updated \(t.formatted(date: .omitted, time: .standard))")
                        .font(OCFont.text(12))
                        .foregroundStyle(Color.ocMuted)
                }
            }
            if let n = live.note ?? live.cameraError {
                Text(n).font(OCFont.text(13)).foregroundStyle(Color.ocAmber)
            }
        }
    }
}
