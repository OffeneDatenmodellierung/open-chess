import Foundation

// Server answers. Tolerant decoding: anything missing gets a default.

struct SyncRating: Decodable {
    var rating: Double = 1500
    var deviation: Double = 350
    var volatility: Double = 0.06
    var provisional = true
    var games: Int = 0

    enum CodingKeys: String, CodingKey { case rating, deviation, volatility, provisional, games }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        rating = try c.decodeIfPresent(Double.self, forKey: .rating) ?? 1500
        deviation = try c.decodeIfPresent(Double.self, forKey: .deviation) ?? 350
        volatility = try c.decodeIfPresent(Double.self, forKey: .volatility) ?? 0.06
        provisional = try c.decodeIfPresent(Bool.self, forKey: .provisional) ?? true
        games = try c.decodeIfPresent(Int.self, forKey: .games) ?? 0
    }
}

struct OpponentInfo: Decodable {
    var username: String = ""
    var rating: Int?

    enum CodingKeys: String, CodingKey { case username, rating }

    init(username: String, rating: Int?) {
        self.username = username
        self.rating = rating
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        username = try c.decodeIfPresent(String.self, forKey: .username) ?? ""
        if let r = try? c.decodeIfPresent(Int.self, forKey: .rating) {
            rating = r
        } else if let d = try? c.decodeIfPresent(Double.self, forKey: .rating) {
            rating = Int(d)
        }
    }
}

/// A game in progress on the server (or Lichess).
struct PlayingGame: Decodable, Identifiable {
    var gameId: String = ""
    var opponent = OpponentInfo(username: "", rating: nil)
    var color: String = "white"
    var isMyTurn = false
    var fen: String = ""
    var lastMove: String = ""
    var speed: String = ""
    var rated = false
    var daysPerTurn: Int?
    /// Set on the client: "open-chess" or "lichess".
    var site: String = "open-chess"
    var id: String { site + ":" + gameId }

    enum CodingKeys: String, CodingKey {
        case gameId, opponent, color, isMyTurn, fen, lastMove, speed, rated, daysPerTurn
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        gameId = try c.decodeIfPresent(String.self, forKey: .gameId) ?? ""
        opponent = try c.decodeIfPresent(OpponentInfo.self, forKey: .opponent) ?? OpponentInfo(username: "", rating: nil)
        color = try c.decodeIfPresent(String.self, forKey: .color) ?? "white"
        isMyTurn = try c.decodeIfPresent(Bool.self, forKey: .isMyTurn) ?? false
        fen = try c.decodeIfPresent(String.self, forKey: .fen) ?? ""
        lastMove = try c.decodeIfPresent(String.self, forKey: .lastMove) ?? ""
        speed = try c.decodeIfPresent(String.self, forKey: .speed) ?? ""
        rated = try c.decodeIfPresent(Bool.self, forKey: .rated) ?? false
        daysPerTurn = try c.decodeIfPresent(Int.self, forKey: .daysPerTurn)
    }
}

/// An offline game someone uploaded with me as the other player.
struct Confirmation: Decodable, Identifiable {
    var id: String = ""
    var white: String = ""
    var black: String = ""
    var winner: String?
    var status: String = ""
    var speed: String = ""
    var uploadedBy: String = ""
    var createdAt: UInt64 = 0

    enum CodingKeys: String, CodingKey { case id, white, black, winner, status, speed, uploadedBy, createdAt }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        white = try c.decodeIfPresent(String.self, forKey: .white) ?? ""
        black = try c.decodeIfPresent(String.self, forKey: .black) ?? ""
        winner = try c.decodeIfPresent(String.self, forKey: .winner)
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? ""
        speed = try c.decodeIfPresent(String.self, forKey: .speed) ?? ""
        uploadedBy = try c.decodeIfPresent(String.self, forKey: .uploadedBy) ?? ""
        if let n = try? c.decodeIfPresent(UInt64.self, forKey: .createdAt) {
            createdAt = n
        } else if let d = try? c.decodeIfPresent(Double.self, forKey: .createdAt) {
            createdAt = UInt64(max(0, d))
        }
    }

    var resultText: String {
        switch winner ?? "" {
        case "white": return "1–0"
        case "black": return "0–1"
        default: return "½–½"
        }
    }
}

struct IncomingChallenge: Decodable, Identifiable {
    var id: String = ""
    var from: String = ""
    var speed: String = ""
    var rated = false
    var daysPerTurn: Int?

    enum CodingKeys: String, CodingKey { case id, from, speed, rated, daysPerTurn }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        // `from` may be a name or a Lichess-style user object.
        if let s = try? c.decodeIfPresent(String.self, forKey: .from) {
            from = s
        } else if let o = try? c.decodeIfPresent(OpponentInfo.self, forKey: .from) {
            from = o.username
        }
        speed = try c.decodeIfPresent(String.self, forKey: .speed) ?? ""
        rated = try c.decodeIfPresent(Bool.self, forKey: .rated) ?? false
        daysPerTurn = try c.decodeIfPresent(Int.self, forKey: .daysPerTurn)
    }
}

struct SyncResponse: Decodable {
    var now: UInt64 = 0
    var ratings: [String: SyncRating] = [:]
    var playing: [PlayingGame] = []
    var confirmations: [Confirmation] = []
    var challenges: [IncomingChallenge] = []

    enum CodingKeys: String, CodingKey { case now, ratings, playing, confirmations, challenges }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let n = try? c.decodeIfPresent(UInt64.self, forKey: .now) {
            now = n
        } else if let d = try? c.decodeIfPresent(Double.self, forKey: .now) {
            now = UInt64(max(0, d))
        }
        ratings = try c.decodeIfPresent([String: SyncRating].self, forKey: .ratings) ?? [:]
        playing = try c.decodeIfPresent([PlayingGame].self, forKey: .playing) ?? []
        confirmations = try c.decodeIfPresent([Confirmation].self, forKey: .confirmations) ?? []
        challenges = try c.decodeIfPresent([IncomingChallenge].self, forKey: .challenges) ?? []
    }
}

struct OfflineResult: Decodable {
    var clientId: String = ""
    var gameId: String?
    var status: String = ""
    var reason: String?
    var ratingDiff: Int?

    enum CodingKeys: String, CodingKey { case clientId, gameId, status, reason, ratingDiff }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        clientId = try c.decodeIfPresent(String.self, forKey: .clientId) ?? ""
        gameId = try c.decodeIfPresent(String.self, forKey: .gameId)
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? ""
        reason = try c.decodeIfPresent(String.self, forKey: .reason)
        ratingDiff = try? c.decodeIfPresent(Int.self, forKey: .ratingDiff)
    }
}

struct OfflineResults: Decodable {
    var results: [OfflineResult] = []

    enum CodingKeys: String, CodingKey { case results }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        results = try c.decodeIfPresent([OfflineResult].self, forKey: .results) ?? []
    }
}

/// `GET /api/account` (Lichess shape; only what the app shows).
struct AccountInfo: Decodable {
    var id: String = ""
    var username: String = ""
    /// An Apple ID is linked (open-chess only).
    var apple = false

    enum CodingKeys: String, CodingKey { case id, username, apple }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        username = try c.decodeIfPresent(String.self, forKey: .username) ?? ""
        apple = (try? c.decodeIfPresent(Bool.self, forKey: .apple)) ?? false
    }
}

struct TokenResponse: Decodable {
    var accessToken: String = ""
    var tokenType: String = ""
    var expiresIn: Double?

    enum CodingKeys: String, CodingKey {
        case accessToken = "access_token"
        case tokenType = "token_type"
        case expiresIn = "expires_in"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        accessToken = try c.decodeIfPresent(String.self, forKey: .accessToken) ?? ""
        tokenType = try c.decodeIfPresent(String.self, forKey: .tokenType) ?? ""
        expiresIn = try? c.decodeIfPresent(Double.self, forKey: .expiresIn)
    }
}

/// `GET /api/esl/photo-terms`: `{terms, accepted, public, text}`.
struct PhotoTerms: Decodable {
    var terms: String = ""
    var accepted = false
    var isPublic = false
    var text: String = ""

    enum CodingKeys: String, CodingKey {
        case terms, accepted, text
        case isPublic = "public"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        terms = try c.decodeIfPresent(String.self, forKey: .terms) ?? ""
        accepted = try c.decodeIfPresent(Bool.self, forKey: .accepted) ?? false
        isPublic = try c.decodeIfPresent(Bool.self, forKey: .isPublic) ?? false
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
    }
}
