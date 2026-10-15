import OpenChessCore
import SwiftUI
import UIKit

/// What the photo gave, for "Save correction".
struct ScanResult {
    let image: UIImage
    let readPlacement: String
    let confidence: Double
    let model: String
}

@MainActor
final class BoardEditorModel: ObservableObject {
    @Published var placement: Placement
    @Published var whiteToMove = true
    @Published var flipped = false
    /// The palette choice: index into `Piece.palette` (12 = empty).
    @Published var brush = 0
    @Published var unsure: Set<Int> = []
    @Published var error: String?
    /// Bumped to ask the editor's view to run the advice again (live advice).
    @Published var adviceRequest = 0

    init(placement: Placement) {
        self.placement = placement
    }

    func tap(_ sq: Int) {
        let p = Piece.palette[brush]
        // Tapping the same piece again clears the square.
        if let p, placement[sq] == p {
            placement[sq] = nil
        } else {
            placement[sq] = p
        }
        unsure.remove(sq)
    }

    func clear() {
        placement = Placement()
        unsure = []
    }

    func reset() {
        placement = .start
        unsure = []
    }

    /// The full FEN (castling inferred), or nil with `error` set.
    func fen() -> String? {
        guard let fen = completeFen(placement: placement.fenField, whiteToMove: whiteToMove) else {
            error = "That isn't a position the board can show."
            return nil
        }
        if let problem = checkFen(fen: fen) {
            error = "Not a legal position: \(problem)"
            return nil
        }
        error = nil
        return fen
    }
}

/// The board editor: tap a square to put the chosen piece there. Then
/// Advice (here, at once, nothing saved), Analyse (the free analysis
/// board) or play from the position.
@MainActor
struct BoardEditorView: View {
    @ObservedObject var editor: BoardEditorModel
    let photo: ScanResult?

    @EnvironmentObject var model: AppModel
    @EnvironmentObject var settings: AppSettings
    @EnvironmentObject var sync: SyncEngine
    @EnvironmentObject var auth: AuthManager
    @StateObject var advice = AdviceModel()
    @State var playFen: String?
    @State var showPlay = false
    @State var showPassAndPlay = false
    @State var showLicence = false
    @State var savedNote: String?
    @State var showAdvice = false
    @State var analysis: AnalysisBoardModel?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            BoardView(
                placement: editor.placement,
                flipped: editor.flipped,
                unsure: editor.unsure,
                onTap: { editor.tap($0) }
            )
            PiecePalette(editor: editor)
            Picker("To move", selection: $editor.whiteToMove) {
                Text("White to move").tag(true)
                Text("Black to move").tag(false)
            }
            .pickerStyle(.segmented)
            HStack(spacing: 8) {
                Button("Start position") { editor.reset() }
                Button("Clear") { editor.clear() }
                Button("Flip") { editor.flipped.toggle() }
            }
            .buttonStyle(OCButtonStyle(kind: .secondary, compact: true))

            if let e = editor.error {
                Text(e).font(OCFont.text(13)).foregroundStyle(Color.ocDanger)
            }
            if let n = savedNote {
                Text(n).font(OCFont.text(13)).foregroundStyle(Color.ocMuted)
            }

            Button {
                runAdvice(delay: 0)
            } label: {
                Label("Advice", systemImage: "lightbulb")
            }
            .buttonStyle(OCButtonStyle(kind: .primary))
            .accessibilityHint("Shows the best moves, the odds and the threats for this position, without starting a game.")
            if showAdvice {
                AdvicePanel(advice: advice)
            }
            Button("Analyse") {
                if let fen = editor.fen() {
                    do {
                        analysis = try AnalysisBoardModel(fen: fen, moves: [], flipped: editor.flipped)
                    } catch {
                        editor.error = describe(error)
                    }
                }
            }
            .buttonStyle(OCButtonStyle(kind: .secondary))
            Button("Play the computer from here") {
                if let fen = editor.fen() {
                    playFen = fen
                    showPlay = true
                }
            }
            .buttonStyle(OCButtonStyle(kind: .secondary))
            Button("Pass and play from here") {
                if let fen = editor.fen() {
                    playFen = fen
                    showPassAndPlay = true
                }
            }
            .buttonStyle(OCButtonStyle(kind: .secondary))
            if photo != nil {
                if auth.signedIn {
                    Button("Save correction") { saveCorrection() }
                        .buttonStyle(OCButtonStyle(kind: .secondary))
                        .disabled(!settings.photoUploads || settings.photoConsent == "declined")
                    if !settings.photoUploads {
                        Text("Photo uploads are off in Sync and data.")
                            .font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
                    } else if settings.photoConsent == "declined" {
                        Text("You chose not to share photos. Change it under Account.")
                            .font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
                    }
                } else {
                    SignInPrompt(message: "Sign in to send this photo with its corrected position, to help train the board reader.")
                }
            }
        }
        .onChange(of: editor.placement) { _, _ in
            if showAdvice { runAdvice(delay: 0.5) }
        }
        .onChange(of: editor.whiteToMove) { _, _ in
            if showAdvice { runAdvice(delay: 0) }
        }
        .onChange(of: editor.adviceRequest) { _, _ in
            runAdvice(delay: 0)
        }
        .sheet(isPresented: $showPlay) {
            NewComputerGameView(fen: playFen, training: true)
                .ocAppearance()
        }
        .sheet(isPresented: $showPassAndPlay) {
            NewPassAndPlayView(fen: playFen)
                .ocAppearance()
        }
        .sheet(isPresented: $showLicence) {
            PhotoLicenceSheet { accepted in
                showLicence = false
                if accepted { saveCorrection() }
            }
            .ocAppearance()
        }
        .fullScreenCover(item: $analysis) { a in
            AnalysisCover(board: a) { analysis = nil }
                .ocAppearance()
        }
    }

    /// Advice for the position as set up, with the chosen side to move.
    func runAdvice(delay: Double) {
        guard let fen = editor.fen() else {
            showAdvice = false
            advice.clear()
            return
        }
        showAdvice = true
        advice.run(fen: fen, delay: delay)
    }

    /// Queue the photo with the corrected placement (outbox kind "photo").
    private func saveCorrection() {
        guard let photo else { return }
        guard settings.photoConsentGiven else {
            showLicence = true
            return
        }
        guard let jpeg = ImageTools.uploadJPEG(photo.image) else {
            editor.error = "Couldn't prepare the photo."
            return
        }
        let key = "photo-" + newClientId()
        let body: [String: Any] = [
            "clientId": key,
            "image": jpeg.base64EncodedString(),
            "placement": editor.placement.fenField,
            "confidence": photo.confidence,
            "model": photo.model,
        ]
        sync.enqueue(key: key, kind: "photo", payload: JSONAny.data(body))
        savedNote = sync.photosAllowed
            ? "Saved. It goes with the next sync."
            : "Saved. It waits for Wi-Fi and normal data mode."
    }
}

/// The photo licence, shown once before any photo is kept.
@MainActor
struct PhotoLicenceSheet: View {
    let onDone: (Bool) -> Void

    @EnvironmentObject var settings: AppSettings
    @EnvironmentObject var auth: AuthManager
    @State var makePublic = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text(PhotoLicence.text)
                        .font(OCFont.text(14))
                    Toggle("Add my photos to the public dataset (CC0)", isOn: $makePublic)
                        .padding(.top, 4)
                    Button("I accept") { answer(true) }
                        .buttonStyle(OCButtonStyle(kind: .primary))
                    Button("No thanks") { answer(false) }
                        .buttonStyle(OCButtonStyle(kind: .secondary))
                }
                .padding(16)
            }
            .ocScreen()
            .navigationTitle("The photo licence")
            .navigationBarTitleDisplayMode(.inline)
        }
        .onAppear { makePublic = settings.photoPublic }
    }

    private func answer(_ accept: Bool) {
        settings.photoConsent = accept ? "accepted" : "declined"
        settings.photoPublic = accept && makePublic
        if auth.signedIn {
            let api = auth.server
            let isPublic = accept && makePublic
            Task {
                _ = try? await api.postJSON("/api/esl/photo-terms", object: ["accept": accept, "public": isPublic])
            }
        }
        onDone(accept)
    }
}

enum PhotoLicence {
    static let version = "open-chess-photo-licence-1"
    static let text = """
    open-chess photo licence, version 1

    When the board reader isn't sure of a photo you send, the server can keep it so people can say what it shows. The board reader then learns from the answers. It keeps your photos only if you accept this licence.

    1. You may share the photo. You took it, or you have the right to send it. It shows a chess board, and nothing private: no faces you'd mind being seen, no documents or screens with personal details.

    2. You give the operator of the server you send it to, and the open-chess project, a worldwide, royalty-free, non-exclusive, perpetual and irrevocable licence to store, copy, crop, label and review the photo; to use it to train, test and improve machine-learning models of any kind and for any purpose; and to use, sell, publish and share those models and what they produce. You keep your copyright in the photo.

    3. People signed in to that server may see the photo, to say what it shows. Its operator and the open-chess project may share it with others who train models under these same terms.

    4. What you say a photo shows is covered by the same licence.

    Optional: tick "public dataset" to also dedicate the photos you send from then on to the public domain under CC0 1.0.

    You can withdraw on the Account screen at any time; that stops photos being kept from then on. Models already trained, the licence for photos already sent, and CC0 dedications can't be undone.
    """
}

/// The editor's pieces: tap one, then tap squares; the last slot clears.
@MainActor
struct PiecePalette: View {
    @ObservedObject var editor: BoardEditorModel

    var body: some View {
        let columns = Array(repeating: GridItem(.flexible(), spacing: 4), count: 7)
        return LazyVGrid(columns: columns, spacing: 4) {
            ForEach(0..<14, id: \.self) { i in
                paletteButton(i)
            }
        }
    }

    @ViewBuilder
    private func paletteButton(_ i: Int) -> some View {
        if i < Piece.palette.count {
            let piece = Piece.palette[i]
            Button {
                editor.brush = i
            } label: {
                ZStack {
                    Rectangle().fill(i % 2 == 0 ? Color.ocSqLight : Color.ocSqDark)
                    if let piece {
                        PieceGlyph(piece: piece, size: 36)
                    } else {
                        Image(systemName: "xmark").foregroundStyle(Color.ocBSeg)
                    }
                }
                .frame(height: 42)
                .overlay(
                    Rectangle().stroke(editor.brush == i ? Color.ocAction : Color.ocRule,
                                       lineWidth: editor.brush == i ? 3 : 1)
                )
            }
            .buttonStyle(.plain)
            .accessibilityLabel(piece.map { String($0.letter) } ?? "Empty")
        } else {
            // The 14th slot: clear the whole board.
            Button {
                editor.clear()
            } label: {
                Image(systemName: "trash")
                    .frame(maxWidth: .infinity, minHeight: 42)
                    .foregroundStyle(Color.ocDanger)
                    .overlay(Rectangle().stroke(Color.ocRule, lineWidth: 1))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Clear the board")
        }
    }
}
