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

    // MARK: Results, spoken moves, verdicts

    func testKingSquareAndMaterial() {
        XCTAssertEqual(Placement.start.kingSquare(white: true), Square.index("e1"))
        XCTAssertEqual(Placement.start.kingSquare(white: false), Square.index("e8"))
        XCTAssertTrue(Placement(fen: "8/8/4k3/8/8/4K3/8/8").insufficientMaterial)
        XCTAssertTrue(Placement(fen: "8/8/4k3/8/8/4KN2/8/8").insufficientMaterial)
        XCTAssertFalse(Placement(fen: "8/8/4k3/8/8/4KR2/8/8").insufficientMaterial)
        // Bishops on squares of one colour (c1 and f4 are both dark).
        XCTAssertTrue(Placement(fen: "8/8/4k3/8/5b2/8/8/2B1K3").insufficientMaterial)
    }

    func testResultTitles() {
        XCTAssertEqual(GameText.result(status: "mate", winner: "black", fen: "x").title, "Checkmate · Black wins")
        XCTAssertEqual(GameText.result(status: "mate", winner: "black", fen: "x").score, "0–1")
        let bare = "8/8/4k3/8/8/4K3/8/8 w - - 0 60"
        XCTAssertEqual(GameText.result(status: "draw", winner: nil, fen: bare).title, "Draw · insufficient material")
        let fifty = "8/8/4k3/8/8/4KR2/8/8 w - - 100 90"
        XCTAssertEqual(GameText.result(status: "draw", winner: nil, fen: fifty).title, "Draw · fifty-move rule")
        let rep = "8/8/4k3/8/8/4KR2/8/8 w - - 6 40"
        XCTAssertEqual(GameText.result(status: "draw", winner: nil, fen: rep).title, "Draw · threefold repetition")
        XCTAssertEqual(GameText.result(status: "draw", winner: nil, fen: rep, agreed: true).title, "Draw · agreed")
        XCTAssertEqual(GameText.result(status: "stalemate", winner: nil, fen: rep).title, "Draw · stalemate")
        XCTAssertEqual(GameText.result(status: "aborted", winner: nil, fen: rep).score, "")
    }

    func testSpokenMoves() {
        XCTAssertEqual(GameText.spoken(san: "Nf3+"), "Knight to f3, check")
        XCTAssertEqual(GameText.spoken(san: "exd5"), "e pawn takes d5")
        XCTAssertEqual(GameText.spoken(san: "e4"), "Pawn to e4")
        XCTAssertEqual(GameText.spoken(san: "O-O"), "Castles kingside")
        XCTAssertEqual(GameText.spoken(san: "O-O-O#"), "Castles queenside, checkmate")
        XCTAssertEqual(GameText.spoken(san: "e8=Q+"), "Pawn to e8, promotes to queen, check")
        XCTAssertEqual(GameText.spoken(san: "Nbxd2"), "Knight from b takes d2")
        XCTAssertEqual(GameText.spoken(san: "Qh5!?"), "Queen to h5")
    }

    func testCorrectionsAndVerdicts() {
        XCTAssertEqual(GameText.correction(piece: "N", from: "e5", to: "f3"), "Knight on e5 should go back to f3")
        XCTAssertEqual(GameText.correction(piece: "Q", from: "g1", to: nil), "Remove the white queen from g1")
        XCTAssertEqual(GameText.correction(piece: "p", from: nil, to: "e7"), "Put the black pawn back on e7")
        XCTAssertEqual(GameText.verdict(cp: 10, mate: nil), "About level")
        XCTAssertEqual(GameText.verdict(cp: 120, mate: nil), "White is better")
        XCTAssertEqual(GameText.verdict(cp: -400, mate: nil), "Black is winning")
        XCTAssertEqual(GameText.verdict(cp: nil, mate: -2), "Black mates in 2")
        XCTAssertEqual(
            GameText.otherSideToMove(fen: "8/8/4k3/8/8/4K3/8/8 w - e3 0 1"),
            "8/8/4k3/8/8/4K3/8/8 b - - 0 1"
        )
    }

    // MARK: Move input

    func testMoveInput() {
        var input = MoveInput()
        let legal = ["e2e4", "e2e3", "g1f3", "e7e8q", "e7e8n"]
        let start = Placement.start
        XCTAssertEqual(input.tap(Square.index("e2")!, legal: legal, placement: start, whiteToMove: true), .none)
        XCTAssertEqual(input.targets, [Square.index("e4")!, Square.index("e3")!])
        XCTAssertEqual(input.tap(Square.index("e4")!, legal: legal, placement: start, whiteToMove: true), .move("e2e4"))
        XCTAssertNil(input.selected)
        // A piece of the side to move with no legal move shakes.
        XCTAssertEqual(input.tap(Square.index("a1")!, legal: legal, placement: start, whiteToMove: true), .illegal(Square.index("a1")!))
        // Selected, then a square it can't reach: shake the selected piece.
        _ = input.tap(Square.index("g1")!, legal: legal, placement: start, whiteToMove: true)
        XCTAssertEqual(input.tap(Square.index("g4")!, legal: legal, placement: start, whiteToMove: true), .illegal(Square.index("g1")!))
        // Two moves to one square: a promotion.
        _ = input.tap(Square.index("e7")!, legal: legal, placement: Placement(), whiteToMove: true)
        XCTAssertEqual(
            input.tap(Square.index("e8")!, legal: legal, placement: Placement(), whiteToMove: true),
            .promote(from: Square.index("e7")!, to: Square.index("e8")!)
        )
    }

    // MARK: Live board helpers

    func testUndoCollapsesUnsentMoves() {
        var ops: [OTBOp] = [.move(gameId: "g", uci: "e2e4"), .move(gameId: "g", uci: "e7e5")]
        OTBQueueLogic.appendUndo(&ops, gameId: "g", plies: 1, firstInFlight: false)
        XCTAssertEqual(ops.map { $0.uci ?? "" }, ["e2e4"])
        // The first is being sent: it can't be dropped, so an undo follows it.
        OTBQueueLogic.appendUndo(&ops, gameId: "g", plies: 2, firstInFlight: true)
        XCTAssertEqual(ops.count, 2)
        XCTAssertEqual(ops.last?.kind, "undo")
        XCTAssertEqual(ops.last?.plies, 2)
        XCTAssertEqual(OTBQueueLogic.waitingMoves(ops, gameId: "g"), 1)
        let end = OTBOp.end(gameId: "g", status: "resign", winner: "black")
        XCTAssertEqual(end.body["winner"] as? String, "black")
        let data = try? JSONEncoder().encode(ops)
        XCTAssertEqual(data.flatMap { try? JSONDecoder().decode([OTBOp].self, from: $0) }, ops)
    }

    func testStreamTargetsAndMoveWindow() {
        XCTAssertEqual(StreamTarget.url(service: "twitch", custom: "", key: " live_1_abc ")?.absoluteString,
                       "rtmp://live.twitch.tv/app/live_1_abc")
        XCTAssertEqual(StreamTarget.url(service: "youtube", custom: "", key: "k")?.absoluteString,
                       "rtmp://a.rtmp.youtube.com/live2/k")
        XCTAssertEqual(StreamTarget.url(service: "custom", custom: "rtmps://x.example/app/", key: "k")?.absoluteString,
                       "rtmps://x.example/app/k")
        XCTAssertNil(StreamTarget.url(service: "custom", custom: "https://x", key: "k"))
        XCTAssertNil(StreamTarget.url(service: "twitch", custom: "", key: ""))
        let lines = MoveWindow.lines(san: ["e4", "e5", "Nf3", "Nc6", "Bb5", "a6"])
        XCTAssertEqual(lines.map { $0.text }, ["1… e5", "2. Nf3", "2… Nc6", "3. Bb5", "3… a6"])
        XCTAssertEqual(lines.last?.current, true)
        XCTAssertEqual(elapsedText(seconds: 3909), "1:05:09")
        XCTAssertEqual(elapsedText(seconds: 247), "4:07")
        let setup = LiveSetup(white: "Ann", black: "bob", me: "ann", speed: "rapid", rated: true,
                              source: .photos, helpTrain: false, stream: false, overlay: true)
        XCTAssertTrue(setup.recorderIsWhite)
        XCTAssertEqual(setup.opponent, "bob")
        XCTAssertEqual(setup.startBody["board"] as? String, "photos")
    }
}
