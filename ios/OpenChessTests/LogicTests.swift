import XCTest

/// Pure-Swift pieces of the app (no core needed): FEN placement, UCI
/// squares, speeds and the analysis JSON decoding.
final class LogicTests: XCTestCase {
    func testPlacementRoundTrip() {
        let fen = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1"
        let p = Placement(fen: fen)
        XCTAssertEqual(p.fenField, "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR")
        XCTAssertEqual(p[Square.index("e4")!]?.letter, "P")
        XCTAssertNil(p[Square.index("e2")!])
        XCTAssertEqual(Placement.start.fenField, "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR")
    }

    func testSquares() {
        XCTAssertEqual(Square.index("a1"), 0)
        XCTAssertEqual(Square.index("h8"), 63)
        XCTAssertNil(Square.index("z9"))
        XCTAssertEqual(Square.name(12), "e2")
        XCTAssertFalse(Square.isLight(0))
        let m = UCIMove("e7e8q")
        XCTAssertEqual(m?.from, 52)
        XCTAssertEqual(m?.to, 60)
        XCTAssertEqual(m?.promotion, "q")
    }

    func testSpeeds() {
        XCTAssertEqual(TimeControl(minutes: 3, increment: 2).speed, "blitz")
        XCTAssertEqual(TimeControl(minutes: 10, increment: 5).speed, "rapid")
        XCTAssertEqual(TimeControl(minutes: 1, increment: 0).speed, "bullet")
        XCTAssertEqual(TimeControl.untimed.speed, "classical")
    }

    func testAnalysisDecodesWithMissingFields() {
        let json = """
        {"fen":"x","turn":"white","score":{"kind":"cp","value":35},
         "wdl":{"white":0.4,"draw":0.4,"black":0.2},"expected_white":0.6,
         "lines":[{"rank":1,"score":{"kind":"mate","moves":3},
                   "moves":[{"number":"1.","side":"white","uci":"e2e4","san":"e4","patterns":[]}]}],
         "opening":{"eco":"B00","name":"King's Pawn Opening","moves":["e2e4"]}}
        """
        let a = AnalysisResult.decode(json)
        XCTAssertNotNil(a)
        XCTAssertEqual(a?.lines.first?.score?.text, "#3")
        XCTAssertEqual(a?.score?.text, "+0.35")
        XCTAssertEqual(a?.lines.first?.sanText, "1. e4")
        XCTAssertEqual(a?.opening?.eco, "B00")
        XCTAssertEqual(a?.expectedWhite ?? 0, 0.6, accuracy: 1e-9)
        XCTAssertNotNil(AnalysisResult.decode("{}"))
    }

    func testSavedGameRoundTrip() {
        let g = SavedGame(
            id: "a", kind: "computer", level: 3, playerWhite: true, white: "me", black: "ai-level-3",
            initialFen: nil, moves: ["e2e4"], times: [1], hints: 0, takebacks: 0, analysed: false,
            status: "mate", winner: "white", speed: "rapid", startedMs: 1, updatedMs: 2,
            minutes: 10, increment: 5, whiteMs: nil, blackMs: nil, online: nil, recorded: true
        )
        let back = SavedGame.decode(g.encoded())
        XCTAssertEqual(back?.resultText, "1–0")
        XCTAssertEqual(back?.moves, ["e2e4"])
    }
}
