import Foundation

/// A time control: minutes plus increment seconds; 0 minutes is untimed.
struct TimeControl: Hashable, Codable {
    var minutes: Int
    var increment: Int

    static let untimed = TimeControl(minutes: 0, increment: 0)
    static let choices: [TimeControl] = [
        .untimed,
        TimeControl(minutes: 3, increment: 2),
        TimeControl(minutes: 5, increment: 3),
        TimeControl(minutes: 10, increment: 0),
        TimeControl(minutes: 10, increment: 5),
        TimeControl(minutes: 15, increment: 10),
        TimeControl(minutes: 30, increment: 0),
    ]

    var isTimed: Bool { minutes > 0 }

    var label: String {
        isTimed ? "\(minutes)+\(increment)" : "Untimed"
    }

    /// Lichess's speed names from the estimated game length
    /// (limit + 40 × increment). Untimed games count as classical.
    var speed: String {
        guard isTimed else { return "classical" }
        let est = minutes * 60 + 40 * increment
        if est < 30 { return "ultraBullet" }
        if est < 180 { return "bullet" }
        if est < 480 { return "blitz" }
        if est < 1500 { return "rapid" }
        return "classical"
    }
}

/// Where an online game is played.
struct OnlineLink: Hashable, Codable {
    var site: String  // "open-chess" or "lichess"
    var gameId: String
    var myColorWhite: Bool
    var rated: Bool
    var opponent: String
    var opponentRating: Int?
    var speed: String
    var daysPerTurn: Int?

    var siteLabel: String { site == "lichess" ? "Lichess" : "open-chess" }
}

enum GameKind {
    case computer(level: Int, playerWhite: Bool)
    case passAndPlay
    case online(OnlineLink)

    var key: String {
        switch self {
        case .computer: return "computer"
        case .passAndPlay: return "passAndPlay"
        case .online: return "online"
        }
    }
}

/// How the app saves a game in the store (`saveGame`), so it can be
/// opened again. Its own JSON; the core only keeps the string.
struct SavedGame: Codable {
    var id: String
    var kind: String
    var level: Int?
    var playerWhite: Bool?
    var white: String
    var black: String
    var initialFen: String?
    var moves: [String]
    var times: [UInt64]
    var hints: UInt32
    var takebacks: UInt32
    var analysed: Bool
    var status: String
    var winner: String?
    var speed: String
    var startedMs: UInt64
    var updatedMs: UInt64
    var minutes: Int
    var increment: Int
    var whiteMs: Int?
    var blackMs: Int?
    var online: OnlineLink?
    var recorded: Bool?

    var finished: Bool { status != "started" }

    var resultText: String {
        guard finished else { return "In progress" }
        switch winner ?? "" {
        case "white": return "1–0"
        case "black": return "0–1"
        default: return "½–½"
        }
    }

    static func decode(_ s: String) -> SavedGame? {
        try? JSONDecoder().decode(SavedGame.self, from: Data(s.utf8))
    }

    func encoded() -> String {
        guard let d = try? JSONEncoder().encode(self) else { return "{}" }
        return String(data: d, encoding: .utf8) ?? "{}"
    }
}

/// Human words for Lichess's status names.
func statusText(_ status: String, winner: String?) -> String {
    let who: String
    switch winner ?? "" {
    case "white": who = "White wins"
    case "black": who = "Black wins"
    default: who = "Draw"
    }
    switch status {
    case "started", "created": return "Playing"
    case "mate": return "Checkmate · \(who)"
    case "resign": return "Resignation · \(who)"
    case "stalemate": return "Stalemate · Draw"
    case "outoftime", "timeout": return "Time out · \(who)"
    case "draw": return "Draw"
    case "aborted": return "Aborted"
    default: return "\(status.capitalized) · \(who)"
    }
}
