import OpenChessCore
import SwiftUI

/// Advice on one position: who is to move and whether they're in check
/// or mated, the odds and a verdict, the top five moves explained, what
/// the other side threatens, and the opening. Never scored: it runs on a
/// session of its own.
struct AdviceResult {
    var fen: String
    var whiteToMove: Bool
    /// "started", "mate", "stalemate" or "draw".
    var status: String = "started"
    var winner: String?
    var check = false
    var analysis: AnalysisResult?
    /// The other side's best move if it were their turn ("Threat: …").
    var threat: LineMoveInfo?
    var opening: String?
    var error: String?

    var sideText: String {
        let side = whiteToMove ? "White" : "Black"
        switch status {
        case "started":
            return check ? "\(side) to move · in check" : "\(side) to move"
        case "mate":
            return "Checkmate · \(whiteToMove ? "Black" : "White") wins"
        case "stalemate":
            return "Stalemate · \(side) has no move"
        case "draw":
            return "Draw · " + GameText.drawReason(fen: fen)
        default:
            return statusText(status, winner: winner)
        }
    }

    var verdict: String? {
        guard status == "started", let a = analysis else { return nil }
        return GameText.verdict(a.score ?? a.lines.first?.score)
    }
}

enum AdviceEngine {
    /// The advice for a FEN. Slow (about a second): call it off the main thread.
    static func advise(fen: String, threats: Bool = true) -> AdviceResult {
        let white = fen.split(separator: " ").dropFirst().first.map(String.init) != "b"
        var out = AdviceResult(fen: fen, whiteToMove: white)
        let session: GameSession
        do {
            session = try GameSession(fen: fen, mode: .passAndPlay)
        } catch {
            // No move to play: mate if the side to move is in check (with
            // the turn passed, its king could be taken: not a legal
            // position), else stalemate. Said as the result, not an error.
            if describe(error).contains("already over") {
                let inCheck = GameText.otherSideToMove(fen: fen).map { checkFen(fen: $0) != nil } ?? false
                out.status = inCheck ? "mate" : "stalemate"
                out.check = inCheck
                out.winner = inCheck ? (white ? "black" : "white") : nil
            } else {
                out.error = describe(error)
            }
            return out
        }
        let st = session.state()
        out.status = st.status
        out.winner = st.winner
        out.check = st.check
        out.opening = st.opening
        guard st.status == "started" else { return out }
        do {
            let json = try session.analyseJson(depth: 10, nodes: 400_000, lines: 5, plies: 8, timeMs: 900)
            out.analysis = AnalysisResult.decode(json)
            if let o = out.analysis?.opening, !o.name.isEmpty {
                out.opening = o.eco.isEmpty ? o.name : "\(o.eco) · \(o.name)"
            }
        } catch {
            out.error = describe(error)
        }
        if threats, !st.check, let other = GameText.otherSideToMove(fen: fen), checkFen(fen: other) == nil,
           let s2 = try? GameSession(fen: other, mode: .passAndPlay), s2.state().status == "started",
           let json = try? s2.analyseJson(depth: 8, nodes: 150_000, lines: 1, plies: 2, timeMs: 350),
           let first = AnalysisResult.decode(json)?.lines.first?.moves.first {
            out.threat = first
        }
        return out
    }
}

/// Runs advice off the main thread, newest request wins (with an optional
/// debounce, for a board being moved on).
@MainActor
final class AdviceModel: ObservableObject {
    @Published private(set) var result: AdviceResult?
    @Published private(set) var running = false
    @Published private(set) var updatedAt: Date?

    private var generation = 0
    private var task: Task<Void, Never>?

    func run(fen: String, delay: Double = 0) {
        generation += 1
        let gen = generation
        task?.cancel()
        running = true
        task = Task {
            if delay > 0 {
                try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            }
            if Task.isCancelled || gen != self.generation { return }
            let r: AdviceResult = await Task.detached(priority: .userInitiated) { () -> AdviceResult in
                AdviceEngine.advise(fen: fen)
            }.value
            guard gen == self.generation else { return }
            self.result = r
            self.running = false
            self.updatedAt = Date()
        }
    }

    func clear() {
        generation += 1
        task?.cancel()
        running = false
        result = nil
    }
}

/// The advice panel, shared by the scan screen and the analysis board.
@MainActor
struct AdvicePanel: View {
    @ObservedObject var advice: AdviceModel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                SectionTitle(text: "Advice")
                Spacer()
                if advice.running {
                    ProgressView().controlSize(.small)
                }
            }
            if let r = advice.result {
                content(r)
            } else if advice.running {
                Text("Thinking…").font(OCFont.text(14)).foregroundStyle(Color.ocMuted)
            }
        }
        .ocCard(padding: 12)
    }

    @ViewBuilder
    private func content(_ r: AdviceResult) -> some View {
        Text(r.sideText)
            .font(OCFont.text(16, weight: .bold))
            .foregroundStyle(r.check || r.status != "started" ? Color.ocDanger : Color.ocInk)
        if let e = r.error {
            Text(e).font(OCFont.text(13)).foregroundStyle(Color.ocDanger)
        }
        if let a = r.analysis {
            WDLBar(white: a.wdl.white, draw: a.wdl.draw, black: a.wdl.black)
            if let v = r.verdict {
                HStack {
                    Text(v).font(OCFont.text(15, weight: .semibold))
                    Spacer()
                    if let s = a.score {
                        Text(s.text).font(OCFont.mono(14, weight: .semibold)).foregroundStyle(Color.ocMuted)
                    }
                }
            }
            ForEach(a.lines.prefix(5)) { line in
                lineView(line)
            }
        }
        if let t = r.threat {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text("Threat:").font(OCFont.text(14, weight: .semibold)).foregroundStyle(Color.ocAmber)
                    Text(t.san).font(OCFont.mono(15, weight: .bold)).foregroundStyle(Color.ocAmber)
                }
                if !t.text.isEmpty {
                    Text(t.text).font(OCFont.text(13)).foregroundStyle(Color.ocMuted)
                }
            }
        }
        if let o = r.opening, !o.isEmpty {
            Text(o).font(OCFont.text(13)).foregroundStyle(Color.ocMuted)
        }
    }

    private func lineView(_ line: AnalysisLine) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(line.score?.text ?? "")
                    .font(OCFont.mono(13, weight: .semibold))
                    .frame(width: 58, alignment: .leading)
                Text(line.moves.first?.san ?? "")
                    .font(OCFont.mono(15, weight: .bold))
                    .foregroundStyle(line.rank <= 1 ? Color.ocAction : Color.ocInk)
                Text(line.moves.dropFirst().map { $0.san }.joined(separator: " "))
                    .font(OCFont.mono(12))
                    .foregroundStyle(Color.ocMuted)
                    .lineLimit(1)
            }
            if let first = line.moves.first {
                if !first.text.isEmpty {
                    Text(first.text).font(OCFont.text(13)).foregroundStyle(Color.ocMuted)
                }
                if !first.patterns.isEmpty {
                    Text(first.patterns.map { $0.detail.isEmpty ? $0.name : "\($0.name): \($0.detail)" }.joined(separator: " · "))
                        .font(OCFont.text(12))
                        .foregroundStyle(Color.ocAmber)
                }
            }
        }
        .padding(.vertical, 2)
    }
}

enum PositionTools {
    /// The legal move from `fen` that gives `target` (placement only), if
    /// there's exactly one way there.
    static func move(from fen: String, to target: Placement) -> String? {
        guard let start = try? GameSession(fen: fen, mode: .passAndPlay) else { return nil }
        for m in start.legalUci() {
            guard let s = try? GameSession(fen: fen, mode: .passAndPlay) else { continue }
            guard (try? s.play(mv: m, atMs: 0)) != nil else { continue }
            if Placement(fen: s.state().fen) == target {
                return m
            }
        }
        return nil
    }
}
