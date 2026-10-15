import OpenChessCore

/// "Moves in plain English": the core's `describeMoves`, when the setting
/// is on (empty otherwise), with the last answer kept so redraws are free.
@MainActor
enum MoveWords {
    private static var lastKey = ""
    private static var lastWords: [String] = []

    static var enabled: Bool { Feedback.settings?.plainMoves ?? false }

    /// One sentence per move of a game ("Knight from b1 to c3.").
    static func words(initialFen: String?, uci: [String]) -> [String] {
        guard enabled, !uci.isEmpty else { return [] }
        return describe(initialFen: initialFen, uci: uci)
    }

    static func words(for state: GameState) -> [String] {
        words(initialFen: startFen(state), uci: state.uci)
    }

    /// The last move's sentence, whatever the setting (for speaking it).
    static func lastMove(of state: GameState) -> String? {
        guard !state.uci.isEmpty else { return nil }
        return describe(initialFen: startFen(state), uci: state.uci).last
    }

    /// One move from a position (a hint), in words.
    static func move(fen: String, uci: String) -> String? {
        guard enabled else { return nil }
        return describeMoves(fen: fen, moves: uci).first
    }

    private static func startFen(_ state: GameState) -> String? {
        state.initialFen == GameViewModel.startFen ? nil : state.initialFen
    }

    private static func describe(initialFen: String?, uci: [String]) -> [String] {
        let moves = uci.joined(separator: " ")
        let key = (initialFen ?? "") + "|" + moves
        if key == lastKey { return lastWords }
        let out = describeMoves(fen: initialFen, moves: moves)
        lastKey = key
        lastWords = out
        return out
    }
}
