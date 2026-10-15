import OpenChessCore
import SwiftUI
import UIKit

@MainActor
final class ScanViewModel: ObservableObject {
    @Published var reading = false
    @Published var note: String?
    @Published var result: ScanResult?
    let editor = BoardEditorModel(placement: .start)

    /// Read a photo with the board reader, if this build has one.
    func read(_ image: UIImage, app: AppModel) async {
        reading = true
        note = nil
        defer { reading = false }
        let (reader, problem) = await app.boardReader()
        guard let reader else {
            note = problem
            result = ScanResult(image: image, readPlacement: "", confidence: 0, model: "")
            return
        }
        let side = Int(reader.imageSize())
        let card = ModelCardInfo.decode(reader.cardJson())
        let modelName = [card?.name ?? "", card?.version ?? ""].filter { !$0.isEmpty }.joined(separator: " ")
        // Scale near the reader's size first: less to copy, same result.
        let scaled = ImageTools.fit(image, maxSide: max(side, 256))
        guard let px = ImageTools.rgba(scaled) else {
            note = "Couldn't read the photo's pixels."
            return
        }
        let outcome: (String?, String?) = await Task.detached(priority: .userInitiated) { () -> (String?, String?) in
            do {
                let json = try reader.readJson(rgba: px.data, width: UInt32(px.width), height: UInt32(px.height))
                return (json, nil)
            } catch {
                return (nil, describe(error))
            }
        }.value
        if let e = outcome.1 {
            note = "The board reader failed: \(e)"
            result = ScanResult(image: image, readPlacement: "", confidence: 0, model: modelName)
            return
        }
        guard let json = outcome.0, let r = BoardReadingResult.decode(json) else {
            note = "The board reader gave no answer."
            return
        }
        editor.placement = Placement(fen: r.placement)
        editor.unsure = Set(r.unsureSquares.compactMap { Square.index($0) })
        result = ScanResult(image: image, readPlacement: r.placement, confidence: r.confidence, model: modelName)
        let pct = Int((r.confidence * 100).rounded())
        if !r.plausible {
            note = "That doesn't look like a game position (\(pct)% sure). Fix the squares, then play or save."
        } else if !r.unsureSquares.isEmpty {
            note = "\(pct)% sure. Check the outlined squares."
        } else {
            note = "\(pct)% sure."
        }
    }
}

/// Scan a board: take a photo, read it, correct it, play from it.
@MainActor
struct ScanView: View {
    @EnvironmentObject var model: AppModel
    @StateObject var scan = ScanViewModel()
    @State var showCamera = false
    @State var source: UIImagePickerController.SourceType = .camera
    @StateObject var live = LiveAdviceModel()
    @State var liveOn = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                if CameraFeed.available {
                    Toggle("Live advice", isOn: $liveOn)
                        .font(OCFont.text(15, weight: .semibold))
                        .accessibilityHint("Keeps the camera on the board and updates the advice as the position changes.")
                        .ocCard(padding: 12)
                }
                if liveOn {
                    LiveAdviceHeader(live: live)
                    BoardEditorView(editor: scan.editor, photo: nil)
                } else if scan.result == nil {
                    intro
                } else {
                    if let img = scan.result?.image {
                        Image(uiImage: img)
                            .resizable()
                            .scaledToFit()
                            .frame(maxHeight: 180)
                            .clipShape(RoundedRectangle(cornerRadius: OCMetrics.corner))
                    }
                    if let n = scan.note {
                        Text(n).font(OCFont.text(14)).foregroundStyle(Color.ocMuted)
                    }
                    BoardEditorView(editor: scan.editor, photo: scan.result)
                    Button("Take another photo") { openPicker(.camera) }
                        .buttonStyle(OCButtonStyle(kind: .secondary))
                }
                if scan.reading {
                    HStack {
                        ProgressView()
                        Text("Reading the board…").foregroundStyle(Color.ocMuted)
                    }
                }
            }
            .padding(16)
            .frame(maxWidth: 720)
            .frame(maxWidth: .infinity)
        }
        .ocScreen()
        .navigationTitle("Scan a board")
        .navigationBarTitleDisplayMode(.inline)
        .onChange(of: liveOn) { _, on in
            if on {
                live.start(app: model, editor: scan.editor)
            } else {
                live.stop()
            }
        }
        .onDisappear {
            liveOn = false
            live.stop()
        }
        .onReceive(NotificationCenter.default.publisher(for: UIDevice.orientationDidChangeNotification)) { _ in
            live.orientationChanged()
        }
        .sheet(isPresented: $showCamera) {
            CameraPicker(source: source) { image in
                showCamera = false
                if let image {
                    Task { await scan.read(image, app: model) }
                }
            }
            .ignoresSafeArea()
        }
    }

    private var intro: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Photograph the board from above, the whole board in view. The reader runs on this device; nothing is sent unless you save a correction.")
                .font(OCFont.text(14))
                .foregroundStyle(Color.ocMuted)
                .ocCard()
            Button {
                openPicker(.camera)
            } label: {
                Label("Take a photo", systemImage: "camera")
            }
            .buttonStyle(OCButtonStyle(kind: .primary))
            Button {
                openPicker(.photoLibrary)
            } label: {
                Label("Choose a photo", systemImage: "photo")
            }
            .buttonStyle(OCButtonStyle(kind: .secondary))
            NavigationLink {
                ManualSetupView()
            } label: {
                Label("Set up by hand", systemImage: "square.grid.3x3")
            }
            .buttonStyle(OCButtonStyle(kind: .secondary))
        }
    }

    private func openPicker(_ s: UIImagePickerController.SourceType) {
        source = (s == .camera && !CameraPicker.cameraAvailable) ? .photoLibrary : s
        showCamera = true
    }
}
