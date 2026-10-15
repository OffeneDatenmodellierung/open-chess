import Foundation

// Codable mirrors of the core's JSON (crates/chess-analysis: analysis.rs,
// explain.rs, patterns.rs, wdl.rs, search.rs; crates/chess-vision for the
// board reading and model card). Every field is optional or defaulted so a
// missing or added field never breaks decoding.

struct WDL: Decodable {
    var white: Double = 0
    var draw: Double = 0
    var black: Double = 0

    enum CodingKeys: String, CodingKey { case white, draw, black }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        white = try c.decodeIfPresent(Double.self, forKey: .white) ?? 0
        draw = try c.decodeIfPresent(Double.self, forKey: .draw) ?? 0
        black = try c.decodeIfPresent(Double.self, forKey: .black) ?? 0
    }
}

/// `{"kind":"cp","value":35}` or `{"kind":"mate","moves":-3}`, from White's side.
struct EngineScore: Decodable {
    var kind: String = "cp"
    var value: Int?
    var moves: Int?

    enum CodingKeys: String, CodingKey { case kind, value, moves }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? "cp"
        value = try c.decodeIfPresent(Int.self, forKey: .value)
        moves = try c.decodeIfPresent(Int.self, forKey: .moves)
    }

    var text: String {
        if kind == "mate", let m = moves {
            return m >= 0 ? "#\(m)" : "#-\(abs(m))"
        }
        let cp = Double(value ?? 0) / 100
        return String(format: "%+.2f", cp)
    }
}

struct PatternInfo: Decodable, Identifiable {
    var motif: String = ""
    var name: String = ""
    var detail: String = ""
    var definition: String = ""
    var id: String { motif + detail }

    enum CodingKeys: String, CodingKey { case motif, name, detail, definition }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        motif = try c.decodeIfPresent(String.self, forKey: .motif) ?? ""
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        detail = try c.decodeIfPresent(String.self, forKey: .detail) ?? ""
        definition = try c.decodeIfPresent(String.self, forKey: .definition) ?? ""
    }
}

/// A move inside a line (MoveExplanation flattened, plus number/side).
struct LineMoveInfo: Decodable, Identifiable {
    var number: String = ""
    var side: String = ""
    var uci: String = ""
    var san: String = ""
    var text: String = ""
    var projected = false
    var patterns: [PatternInfo] = []
    var id: String { number + uci }

    enum CodingKeys: String, CodingKey { case number, side, uci, san, text, projected, patterns }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        number = try c.decodeIfPresent(String.self, forKey: .number) ?? ""
        side = try c.decodeIfPresent(String.self, forKey: .side) ?? ""
        uci = try c.decodeIfPresent(String.self, forKey: .uci) ?? ""
        san = try c.decodeIfPresent(String.self, forKey: .san) ?? ""
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        projected = try c.decodeIfPresent(Bool.self, forKey: .projected) ?? false
        patterns = try c.decodeIfPresent([PatternInfo].self, forKey: .patterns) ?? []
    }
}

struct AnalysisLine: Decodable, Identifiable {
    var rank: Int = 0
    var score: EngineScore?
    var wdl = WDL()
    var moves: [LineMoveInfo] = []
    var id: Int { rank }

    enum CodingKeys: String, CodingKey { case rank, score, wdl, moves }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        rank = try c.decodeIfPresent(Int.self, forKey: .rank) ?? 0
        score = try c.decodeIfPresent(EngineScore.self, forKey: .score)
        wdl = try c.decodeIfPresent(WDL.self, forKey: .wdl) ?? WDL()
        moves = try c.decodeIfPresent([LineMoveInfo].self, forKey: .moves) ?? []
    }

    /// "12. Nf3 Nc6 13. d4 …" from the line's moves.
    var sanText: String {
        var parts: [String] = []
        for (i, m) in moves.enumerated() {
            if m.side == "white" || i == 0 {
                parts.append(m.number + " " + m.san)
            } else {
                parts.append(m.san)
            }
        }
        return parts.joined(separator: " ")
    }
}

struct OpeningInfo: Decodable {
    var eco: String = ""
    var name: String = ""

    enum CodingKeys: String, CodingKey { case eco, name }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        eco = try c.decodeIfPresent(String.self, forKey: .eco) ?? ""
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
    }
}

/// `Analysis` (snake_case fields, as serde writes them).
struct AnalysisResult: Decodable {
    var fen: String = ""
    var turn: String = ""
    var score: EngineScore?
    var wdl = WDL()
    var expectedWhite: Double = 0.5
    var depth: Int = 0
    var nodes: UInt64 = 0
    var lines: [AnalysisLine] = []
    var opening: OpeningInfo?
    var result: String?

    enum CodingKeys: String, CodingKey {
        case fen, turn, score, wdl, depth, nodes, lines, opening, result
        case expectedWhite = "expected_white"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        fen = try c.decodeIfPresent(String.self, forKey: .fen) ?? ""
        turn = try c.decodeIfPresent(String.self, forKey: .turn) ?? ""
        score = try c.decodeIfPresent(EngineScore.self, forKey: .score)
        wdl = try c.decodeIfPresent(WDL.self, forKey: .wdl) ?? WDL()
        expectedWhite = try c.decodeIfPresent(Double.self, forKey: .expectedWhite) ?? 0.5
        depth = try c.decodeIfPresent(Int.self, forKey: .depth) ?? 0
        nodes = try c.decodeIfPresent(UInt64.self, forKey: .nodes) ?? 0
        lines = try c.decodeIfPresent([AnalysisLine].self, forKey: .lines) ?? []
        opening = try c.decodeIfPresent(OpeningInfo.self, forKey: .opening)
        result = try c.decodeIfPresent(String.self, forKey: .result)
    }

    static func decode(_ json: String) -> AnalysisResult? {
        try? JSONDecoder().decode(AnalysisResult.self, from: Data(json.utf8))
    }
}

/// `BoardReading` from the board reader (camelCase fields).
struct BoardReadingResult: Decodable {
    var placement: String = ""
    var confidence: Double = 0
    var unsureSquares: [String] = []
    var plausible = false

    enum CodingKeys: String, CodingKey { case placement, confidence, unsureSquares, plausible }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        placement = try c.decodeIfPresent(String.self, forKey: .placement) ?? ""
        confidence = try c.decodeIfPresent(Double.self, forKey: .confidence) ?? 0
        unsureSquares = try c.decodeIfPresent([String].self, forKey: .unsureSquares) ?? []
        plausible = try c.decodeIfPresent(Bool.self, forKey: .plausible) ?? false
    }

    static func decode(_ json: String) -> BoardReadingResult? {
        try? JSONDecoder().decode(BoardReadingResult.self, from: Data(json.utf8))
    }
}

/// The board reader's model card.
struct ModelCardInfo: Decodable {
    struct Credit: Decodable, Identifiable {
        var name: String = ""
        var licence: String = ""
        var attribution: String = ""
        var id: String { name }

        enum CodingKeys: String, CodingKey { case name, licence, attribution }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
            licence = try c.decodeIfPresent(String.self, forKey: .licence) ?? ""
            attribution = try c.decodeIfPresent(String.self, forKey: .attribution) ?? ""
        }
    }

    var name: String = ""
    var version: String = ""
    var licence: String = ""
    var base: String = ""
    var datasets: [Credit] = []

    enum CodingKeys: String, CodingKey { case name, version, licence, base, datasets }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        version = try c.decodeIfPresent(String.self, forKey: .version) ?? ""
        licence = try c.decodeIfPresent(String.self, forKey: .licence) ?? ""
        base = try c.decodeIfPresent(String.self, forKey: .base) ?? ""
        datasets = try c.decodeIfPresent([Credit].self, forKey: .datasets) ?? []
    }

    static func decode(_ json: String) -> ModelCardInfo? {
        try? JSONDecoder().decode(ModelCardInfo.self, from: Data(json.utf8))
    }
}
