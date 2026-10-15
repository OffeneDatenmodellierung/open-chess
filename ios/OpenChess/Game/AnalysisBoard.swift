import OpenChessCore
import SwiftUI
import UIKit

/// The analysis board: both sides move freely, and the advice (top five
/// lines, odds, the best move explained, the threat) follows each move.
/// It runs on a session of its own, so it's never scored and never
/// counts against a game.
@MainActor
final class AnalysisBoardModel: ObservableObject, Identifiable {
    nonisolated let id: String
    let title: String

    @Published private(set) var state: GameState
    @Published private(set) var placement: Placement
    @Published private(set) var selected: Int?
    @Published private(set) var targets: Set<Int> = []
    @Published var pendingPromotion: (from: Int, to: Int)?
    @Published var flipped: Bool
    @Published private(set) var shakeSquare: Int?
    @Published private(set) var shakes = 0
    @Published var note: String?

    let advice = AdviceModel()
    private var session: GameSession
    private var legal: [String] = []
    private var input = MoveInput()

    init(fen: String?, moves: [String], title: String = "Analysis", flipped: Bool = false) throws {
        id = newClientId()
        self.title = title
        self.flipped = flipped
        let s = try GameSession.resume(
            fen: fen,
            moves: moves.joined(separator: " "),
            moveTimesMs: [],
            mode: .passAndPlay,
            help: Assistance(hints: 0, takebacks: 0, analysed: false)
        )
        session = s
        let st = s.state()
        state = st
        placement = Placement(fen: st.fen)
        refresh(delay: 0)
    }

    var whiteToMove: Bool { state.turn == "white" }
    var lastMove: UCIMove? { state.last.flatMap { UCIMove($0) } }
    var checkSquare: Int? { state.check ? placement.kingSquare(white: whiteToMove) : nil }
    var startsWhite: Bool {
        state.initialFen.split(separator: " ").dropFirst().first.map(String.init) != "b"
    }

    /// The best move, for the board's arrow.
    var bestMove: UCIMove? {
        guard let r = advice.result, r.fen == state.fen,
              let uci = r.analysis?.lines.first?.moves.first?.uci else { return nil }
        return UCIMove(uci)
    }

    private func refresh(delay: Double) {
        state = session.state()
        placement = Placement(fen: state.fen)
        legal = state.status == "started" ? session.legalUci() : []
        input.clear()
        selected = nil
        targets = []
        advice.run(fen: state.fen, delay: delay)
    }

    func tap(_ sq: Int) {
        let outcome = input.tap(sq, legal: legal, placement: placement, whiteToMove: whiteToMove)
        selected = input.selected
        targets = input.targets
        switch outcome {
        case .none:
            break
        case .move(let m):
            play(m)
        case .promote(let from, let to):
            pendingPromotion = (from: from, to: to)
        case .illegal(let square):
            shakeSquare = square
            shakes += 1
            Feedback.illegal()
        }
    }

    func promote(_ piece: Character) {
        guard let p = pendingPromotion else { return }
        pendingPromotion = nil
        play(Square.name(p.from) + Square.name(p.to) + String(piece))
    }

    func cancelPromotion() {
        pendingPromotion = nil
        input.clear()
        selected = nil
        targets = []
    }

    private func play(_ uci: String) {
        do {
            _ = try session.play(mv: uci, atMs: nowMs())
            refresh(delay: 0.35)
            if state.check && state.status == "started" {
                Feedback.check()
            }
        } catch {
            note = describe(error)
        }
    }

    func takeBack() {
        guard state.ply > 0 else { return }
        session.undo(n: 1)
        refresh(delay: 0.35)
    }

    func flip() {
        flipped.toggle()
    }

    func copyFEN() {
        UIPasteboard.general.string = state.fen
        note = "FEN copied."
    }

    func copyPGN() {
        UIPasteboard.general.string = session.pgn(white: "?", black: "?", event: "open-chess analysis")
        note = "PGN copied."
    }
}

/// The analysis board's content (pushed from Train, or inside `AnalysisCover`).
@MainActor
struct AnalysisScreen: View {
    @ObservedObject var board: AnalysisBoardModel
    @ObservedObject var advice: AdviceModel
    @Environment(\.horizontalSizeClass) var hSize

    init(board: AnalysisBoardModel) {
        _board = ObservedObject(wrappedValue: board)
        _advice = ObservedObject(wrappedValue: board.advice)
    }

    var body: some View {
        GeometryReader { geo in
            let wide = hSize == .regular && geo.size.width > geo.size.height * 1.1
            Group {
                if wide {
                    HStack(alignment: .top, spacing: 16) {
                        boardColumn
                            .frame(width: min(geo.size.height - 32, geo.size.width * 0.55))
                        ScrollView { panel }
                    }
                    .padding(16)
                } else {
                    ScrollView {
                        VStack(spacing: 12) {
                            boardColumn
                            panel
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .frame(maxWidth: 720)
                        .frame(maxWidth: .infinity)
                    }
                }
            }
        }
        .ocScreen()
        .navigationTitle(board.title)
        .navigationBarTitleDisplayMode(.inline)
        .overlay {
            if board.pendingPromotion != nil {
                ZStack {
                    Color.black.opacity(0.35).ignoresSafeArea()
                    PromotionPicker(
                        white: board.whiteToMove,
                        onPick: { board.promote($0) },
                        onCancel: { board.cancelPromotion() }
                    )
                }
            }
        }
    }

    private var boardColumn: some View {
        VStack(spacing: 8) {
            HStack {
                OCBadge(text: "Not scored", color: .ocMuted)
                Spacer()
                Text(board.whiteToMove ? "White to move" : "Black to move")
                    .font(OCFont.text(14, weight: .semibold))
            }
            BoardView(
                placement: board.placement,
                flipped: board.flipped,
                selected: board.selected,
                lastMove: board.lastMove,
                hint: board.bestMove,
                targets: board.targets,
                check: board.checkSquare,
                shakeSquare: board.shakeSquare,
                shakes: board.shakes,
                onTap: { board.tap($0) }
            )
        }
    }

    private var panel: some View {
        VStack(alignment: .leading, spacing: 12) {
            AdvicePanel(advice: advice)
            MoveListView(san: board.state.san, startsWhite: board.startsWhite, words: MoveWords.words(for: board.state))
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 100), spacing: 8)], spacing: 8) {
                Button("Take back") { board.takeBack() }
                    .disabled(board.state.ply == 0)
                Button("Flip") { board.flip() }
                Button("Copy FEN") { board.copyFEN() }
                Button("Copy PGN") { board.copyPGN() }
            }
            .buttonStyle(OCButtonStyle(kind: .secondary, compact: true))
            if let n = board.note {
                Text(n).font(OCFont.text(13)).foregroundStyle(Color.ocMuted)
            }
            Text("Analysis is never scored and doesn't count against any game.")
                .font(OCFont.text(12))
                .foregroundStyle(Color.ocMuted)
        }
    }
}

/// The analysis board full screen, with Close.
@MainActor
struct AnalysisCover: View {
    let board: AnalysisBoardModel
    let onClose: () -> Void

    var body: some View {
        NavigationStack {
            AnalysisScreen(board: board)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Close", action: onClose)
                    }
                }
        }
    }
}

/// The analysis board pushed from Train or the board editor (owns its model).
@MainActor
struct AnalysisEntryView: View {
    @StateObject var board: AnalysisBoardModel

    init(fen: String?) {
        // An autoclosure: the model is made once, not on every redraw.
        _board = StateObject(wrappedValue: AnalysisEntryView.make(fen: fen))
    }

    private static func make(fen: String?) -> AnalysisBoardModel {
        if let m = try? AnalysisBoardModel(fen: fen, moves: []) {
            return m
        }
        // A FEN the core refuses: start from the opening position instead.
        return (try? AnalysisBoardModel(fen: nil, moves: []))!
    }

    var body: some View {
        AnalysisScreen(board: board)
    }
}
