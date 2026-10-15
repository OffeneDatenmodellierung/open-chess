import Foundation

// Pure-Swift parts of live board mode (no core, no UI), so the logic tests
// cover them: the setup, the server queue's operations, the stream address
// and the moves shown in the stream's cutout.

/// Where a real board's moves come from.
enum LiveSource: String, CaseIterable, Codable {
    case chessnut, photos, manual

    var label: String {
        switch self {
        case .chessnut: return "Chessnut board"
        case .photos: return "Photos after each move"
        case .manual: return "Enter moves by hand"
        }
    }
}

/// A real-board game as set up.
struct LiveSetup: Codable, Equatable {
    var white: String
    var black: String
    /// The signed-in player (one of `white` and `black`), who records it.
    var me: String
    /// "blitz", "rapid" or "classical".
    var speed: String
    var rated: Bool
    var source: LiveSource
    var helpTrain: Bool
    var stream: Bool
    var overlay: Bool

    var recorderIsWhite: Bool { white.lowercased() == me.lowercased() }
    var opponent: String { recorderIsWhite ? black : white }

    /// The body of `POST /api/otb`.
    var startBody: [String: Any] {
        [
            "white": white,
            "black": black,
            "speed": speed,
            "rated": rated,
            "board": source.rawValue,
            "broadcast": overlay,
        ]
    }
}

/// One call to the server for a real-board game, sent in order.
struct OTBOp: Codable, Equatable, Identifiable {
    var id: String
    var gameId: String
    /// "move", "undo" or "end".
    var kind: String
    var uci: String?
    var plies: Int?
    var status: String?
    var winner: String?

    static func move(gameId: String, uci: String) -> OTBOp {
        OTBOp(id: UUID().uuidString, gameId: gameId, kind: "move", uci: uci)
    }

    static func undo(gameId: String, plies: Int) -> OTBOp {
        OTBOp(id: UUID().uuidString, gameId: gameId, kind: "undo", plies: plies)
    }

    static func end(gameId: String, status: String, winner: String?) -> OTBOp {
        OTBOp(id: UUID().uuidString, gameId: gameId, kind: "end", status: status, winner: winner)
    }

    /// `POST /api/otb/{id}/{kind}`'s JSON body.
    var body: [String: Any] {
        switch kind {
        case "move":
            return ["uci": uci ?? ""]
        case "undo":
            return ["plies": plies ?? 1]
        default:
            var b: [String: Any] = ["status": status ?? "aborted"]
            if let winner { b["winner"] = winner }
            return b
        }
    }

    var summary: String {
        switch kind {
        case "move": return "the move \(uci ?? "")"
        case "undo": return "taking back \(plies ?? 1)"
        default: return "the result"
        }
    }
}

enum OTBQueueLogic {
    /// Take back `plies` of a game: moves still waiting to be sent are
    /// simply dropped (not the first when it's being sent); the rest goes
    /// as an undo.
    static func appendUndo(_ ops: inout [OTBOp], gameId: String, plies: Int, firstInFlight: Bool) {
        var left = plies
        while left > 0, let last = ops.last, last.gameId == gameId, last.kind == "move" {
            if firstInFlight && ops.count == 1 { break }
            ops.removeLast()
            left -= 1
        }
        if left > 0 {
            ops.append(.undo(gameId: gameId, plies: left))
        }
    }

    /// How many moves of a game wait to be sent.
    static func waitingMoves(_ ops: [OTBOp], gameId: String) -> Int {
        ops.filter { $0.gameId == gameId && $0.kind == "move" }.count
    }
}

/// The RTMP address for a service and stream key.
enum StreamTarget {
    static let twitch = "rtmp://live.twitch.tv/app"
    static let youtube = "rtmp://a.rtmp.youtube.com/live2"

    static func base(service: String, custom: String) -> String {
        switch service {
        case "twitch": return twitch
        case "youtube": return youtube
        default: return custom.trimmingCharacters(in: .whitespacesAndNewlines)
        }
    }

    /// The URL to publish to: the base and the key as its last path part.
    static func url(service: String, custom: String, key: String) -> URL? {
        let b = base(service: service, custom: custom)
        let k = key.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !k.isEmpty, b.hasPrefix("rtmp://") || b.hasPrefix("rtmps://") else { return nil }
        let joined = b.hasSuffix("/") ? b + k : b + "/" + k
        return URL(string: joined)
    }
}

/// A line of the stream cutout's move list.
struct MoveLine: Identifiable, Equatable {
    let id: Int
    let text: String
    let current: Bool
}

enum MoveWindow {
    /// The last `count` plies, numbered ("12. Nf3", "12… Nc6"), the last current.
    static func lines(san: [String], startsWhite: Bool = true, count: Int = 5) -> [MoveLine] {
        guard !san.isEmpty, count > 0 else { return [] }
        let first = max(0, san.count - count)
        var out: [MoveLine] = []
        for i in first..<san.count {
            let ply = startsWhite ? i : i + 1
            let number = ply / 2 + 1
            let text = ply % 2 == 0 ? "\(number). \(san[i])" : "\(number)… \(san[i])"
            out.append(MoveLine(id: i, text: text, current: i == san.count - 1))
        }
        return out
    }
}

/// "1:05:09" / "4:07" for the stream's elapsed time.
func elapsedText(seconds: Int) -> String {
    let s = max(0, seconds)
    if s >= 3600 {
        return String(format: "%d:%02d:%02d", s / 3600, (s % 3600) / 60, s % 60)
    }
    return String(format: "%d:%02d", s / 60, s % 60)
}
