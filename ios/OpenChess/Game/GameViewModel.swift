import OpenChessCore
import SwiftUI
import UIKit

/// One game on screen: the core's `GameSession`, move input, the bot,
/// clocks, hints, the Chessnut board and, for online games, the server.
@MainActor
final class GameViewModel: ObservableObject, Identifiable {
    nonisolated let id: String
    let kind: GameKind
    let time: TimeControl
    let startedMs: UInt64
    let whiteName: String
    let blackName: String

    @Published private(set) var state: GameState
    @Published private(set) var placement: Placement
    @Published var flipped: Bool
    @Published private(set) var selected: Int?
    @Published private(set) var targets: Set<Int> = []
    @Published var pendingPromotion: (from: Int, to: Int)?
    @Published private(set) var whiteMs: Int
    @Published private(set) var blackMs: Int
    @Published private(set) var thinking = false
    @Published private(set) var hintsOn = false
    @Published private(set) var hintMove: BotMove?
    @Published private(set) var analysis: AnalysisResult?
    @Published private(set) var analysing = false
    @Published private(set) var assistance: Assistance
    @Published var message: String?
    @Published private(set) var finishedNote: String?
    /// A result decided here that the core doesn't name (time out).
    @Published private(set) var statusOverride: String?
    @Published private(set) var onlineConnected = false
    /// The piece to shake after an illegal tap, and a counter per shake.
    @Published private(set) var shakeSquare: Int?
    @Published private(set) var shakes = 0

    private(set) var session: GameSession
    private weak var app: AppModel?
    private var legal: [String] = []
    private var recorded = false
    private var clockTimer: Timer?
    private var lastTick = Date()
    /// Bumped on every change to the game, so a late bot or analysis
    /// result for an older position is dropped.
    private var generation = 0
    private var streamTask: Task<Void, Never>?
    private var serverMoves: [String] = []
    /// When each ply was played (Unix ms), kept beside the core's own so a
    /// saved game resumes with them.
    private var moveTimes: [UInt64] = []
    /// A result decided here or by the server that the core doesn't hold
    /// (time out, an online result): Lichess status name and winner.
    private var endRaw: (status: String, winner: String?)?
    /// Set once the game is on screen: feedback (haptics, sounds, speech)
    /// only for what happens from then on, not for opening a saved game.
    private var live = false
    private var checkedPly: UInt32 = UInt32.max
    private var agreedDraw = false
    private var endAnnounced = false

    // MARK: Creating

    /// A new game.
    /// `training`: hints on from the start, and the game is never uploaded.
    /// `keepLocal`: never uploaded (no account to rate it against).
    init(
        app: AppModel,
        kind: GameKind,
        fen: String?,
        time: TimeControl,
        white: String,
        black: String,
        training: Bool = false,
        keepLocal: Bool = false,
        gameId: String? = nil
    ) throws {
        self.app = app
        self.kind = kind
        self.time = time
        id = gameId ?? newClientId()
        startedMs = nowMs()
        whiteName = white
        blackName = black
        let s = try GameSession(fen: fen, mode: GameViewModel.mode(for: kind))
        session = s
        state = s.state()
        placement = Placement(fen: s.state().fen)
        assistance = s.assistance()
        whiteMs = time.minutes * 60_000
        blackMs = time.minutes * 60_000
        switch kind {
        case .computer(_, let playerWhite): flipped = !playerWhite
        case .online(let link): flipped = !link.myColorWhite
        case .passAndPlay: flipped = false
        }
        recorded = training || keepLocal
        hintsOn = training
        if case .online = kind {
            serverMoves = state.uci
        }
        afterChange(animateBot: true)
        live = true
    }

    /// A saved game, opened again.
    init(app: AppModel, saved: SavedGame) throws {
        self.app = app
        id = saved.id
        time = TimeControl(minutes: saved.minutes, increment: saved.increment)
        startedMs = saved.startedMs
        whiteName = saved.white
        blackName = saved.black
        let k: GameKind
        if let link = saved.online {
            k = .online(link)
        } else if saved.kind == "computer" {
            k = .computer(level: saved.level ?? 3, playerWhite: saved.playerWhite ?? true)
        } else {
            k = .passAndPlay
        }
        kind = k
        let help = Assistance(hints: saved.hints, takebacks: saved.takebacks, analysed: saved.analysed)
        let s = try GameSession.resume(
            fen: saved.initialFen,
            moves: saved.moves.joined(separator: " "),
            moveTimesMs: saved.times,
            mode: GameViewModel.mode(for: k),
            help: help
        )
        session = s
        state = s.state()
        placement = Placement(fen: s.state().fen)
        assistance = s.assistance()
        whiteMs = saved.whiteMs ?? saved.minutes * 60_000
        blackMs = saved.blackMs ?? saved.minutes * 60_000
        switch k {
        case .computer(_, let playerWhite): flipped = !playerWhite
        case .online(let link): flipped = !link.myColorWhite
        case .passAndPlay: flipped = false
        }
        recorded = saved.recorded ?? saved.finished
        hintsOn = help.hints > 0 || help.analysed
        if saved.finished && state.status == "started" {
            // Resigned, agreed or timed out: the core's replay doesn't know.
            endRaw = (status: saved.status, winner: saved.winner)
            statusOverride = statusText(saved.status, winner: saved.winner)
            finishedNote = statusOverride
        }
        serverMoves = saved.moves
        moveTimes = saved.times
        endAnnounced = saved.finished
        afterChange(animateBot: true)
        live = true
    }

    static func mode(for kind: GameKind) -> GameMode {
        switch kind {
        case .computer(let level, _): return .computer(level: UInt8(clamping: level))
        case .passAndPlay: return .passAndPlay
        case .online(let link): return .online(rated: link.rated)
        }
    }

    // MARK: Derived

    var isFinished: Bool { state.status != "started" || statusOverride != nil }
    var whiteToMove: Bool { state.turn == "white" }
    var lastMove: UCIMove? { state.last.flatMap { UCIMove($0) } }
    var hintSquares: UCIMove? { hintMove.flatMap { UCIMove($0.uci) } }
    var usedHelp: Bool { assistance.hints > 0 || assistance.takebacks > 0 || assistance.analysed }

    var isComputer: Bool { if case .computer = kind { return true } else { return false } }
    var isPassAndPlay: Bool { if case .passAndPlay = kind { return true } else { return false } }
    var onlineLink: OnlineLink? { if case .online(let l) = kind { return l } else { return nil } }

    /// Help is refused in rated online games.
    var helpAllowed: Bool { !(onlineLink?.rated ?? false) }
    var takebackAllowed: Bool { onlineLink == nil && state.ply > 0 && !isFinished }

    /// Whether the person holding the device may move now.
    var myTurn: Bool {
        guard !isFinished else { return false }
        switch kind {
        case .computer(_, let playerWhite): return whiteToMove == playerWhite && !thinking
        case .passAndPlay: return true
        case .online(let link): return whiteToMove == link.myColorWhite
        }
    }

    var statusLine: String {
        if let o = statusOverride { return o }
        if state.status == "started" {
            if thinking { return "The computer is thinking…" }
            let side = whiteToMove ? "White" : "Black"
            return state.check ? "\(side) to move · check" : "\(side) to move"
        }
        return statusText(state.status, winner: state.winner)
    }

    /// The king in check, for the board's red square.
    var checkSquare: Int? {
        state.check ? placement.kingSquare(white: whiteToMove) : nil
    }

    /// The result, once the game is over.
    var result: ResultSummary? {
        guard isFinished else { return nil }
        if let e = endRaw {
            return GameText.result(status: e.status, winner: e.winner, fen: state.fen)
        }
        return GameText.result(status: state.status, winner: state.winner, fen: state.fen, agreed: agreedDraw)
    }

    /// Whether the player on this device lost (for the haptic).
    private var playerLost: Bool {
        let winner = endRaw?.winner ?? state.winner
        guard let winner else { return false }
        switch kind {
        case .computer(_, let playerWhite): return winner != (playerWhite ? "white" : "black")
        case .online(let link): return winner != (link.myColorWhite ? "white" : "black")
        case .passAndPlay: return false
        }
    }

    /// "Analyse a copy": allowed while help is, and in any game once it's over.
    var analysisAllowed: Bool { helpAllowed || isFinished }

    /// The game's moves in a new analysis board (the game's own help
    /// counter is untouched).
    func analysisCopy() -> AnalysisBoardModel? {
        guard analysisAllowed else { return nil }
        let st = state
        let fen: String? = st.initialFen == GameViewModel.startFen ? nil : st.initialFen
        do {
            return try AnalysisBoardModel(fen: fen, moves: st.uci, title: "Analysis · \(title)", flipped: flipped)
        } catch {
            message = describe(error)
            return nil
        }
    }

    /// The same game again with colours swapped (computer, pass and play).
    func rematch() -> GameViewModel? {
        guard let app else { return nil }
        do {
            switch kind {
            case .computer(let level, let playerWhite):
                return try GameViewModel(
                    app: app, kind: .computer(level: level, playerWhite: !playerWhite), fen: nil, time: time,
                    white: blackName, black: whiteName
                )
            case .passAndPlay:
                return try GameViewModel(
                    app: app, kind: .passAndPlay, fen: nil, time: time,
                    white: blackName, black: whiteName, keepLocal: recordedLocally
                )
            case .online:
                return nil
            }
        } catch {
            message = describe(error)
            return nil
        }
    }

    /// Pass and play with no account on one side stays on the device.
    private var recordedLocally: Bool {
        app?.settings.username.isEmpty ?? true
    }

    var canRematch: Bool { onlineLink == nil }

    var title: String {
        switch kind {
        case .computer(let level, _): return "Computer · level \(level)"
        case .passAndPlay: return "Pass and play"
        case .online(let link): return "\(link.siteLabel) · \(link.opponent)"
        }
    }

    // MARK: Input

    func tap(_ sq: Int) {
        guard myTurn else { return }
        var input = MoveInput(selected: selected, targets: targets)
        let outcome = input.tap(sq, legal: legal, placement: placement, whiteToMove: whiteToMove)
        selected = input.selected
        targets = input.targets
        switch outcome {
        case .none:
            break
        case .move(let m):
            userMove(m)
        case .promote(let from, let to):
            pendingPromotion = (from: from, to: to)
        case .illegal(let square):
            shakeSquare = square
            shakes += 1
            Feedback.illegal()
        }
    }

    private func clearSelection() {
        selected = nil
        targets = []
    }

    func promote(_ piece: Character) {
        guard let p = pendingPromotion else { return }
        pendingPromotion = nil
        clearSelection()
        userMove(Square.name(p.from) + Square.name(p.to) + String(piece))
    }

    func cancelPromotion() {
        pendingPromotion = nil
        clearSelection()
    }

    /// A move by the person at the device (on screen or on the board).
    private func userMove(_ uci: String, fromBoard: Bool = false) {
        do {
            let played = try session.play(mv: uci, atMs: nowMs())
            // `state` is still the position before the move: its side moved.
            applyIncrement(moverWhite: whiteToMove)
            if !fromBoard {
                // The board shows it once the player copies it over.
                app?.chessnut.opponentMove(played.uci)
            }
            if let link = onlineLink {
                sendOnlineMove(played.uci, link: link)
            }
            afterChange(animateBot: true)
        } catch {
            message = describe(error)
            if fromBoard { refresh() }
        }
    }

    private func applyIncrement(moverWhite: Bool) {
        guard time.isTimed, onlineLink == nil else { return }
        let inc = time.increment * 1000
        if moverWhite { whiteMs += inc } else { blackMs += inc }
    }

    // MARK: After every change

    private func refresh() {
        state = session.state()
        let ply = Int(state.ply)
        if moveTimes.count > ply { moveTimes.removeLast(moveTimes.count - ply) }
        while moveTimes.count < ply { moveTimes.append(nowMs()) }
        placement = Placement(fen: state.fen)
        legal = isFinished ? [] : session.legalUci()
        assistance = session.assistance()
    }

    private func afterChange(animateBot: Bool) {
        generation += 1
        hintMove = nil
        analysis = nil
        refresh()
        if isPassAndPlay, app?.settings.flipPassAndPlay ?? true, UIDevice.current.userInterfaceIdiom == .phone {
            flipped = !whiteToMove
        }
        save()
        updateClock()
        if live && state.check && !isFinished && state.ply != checkedPly {
            checkedPly = state.ply
            Feedback.check()
        }
        if isFinished {
            finish()
            return
        }
        if case .computer(_, let playerWhite) = kind, whiteToMove != playerWhite {
            botReply(delay: animateBot)
        } else if hintsOn && helpAllowed {
            runHints(countHint: false)
        }
    }

    // MARK: The bot

    private func botReply(delay: Bool) {
        guard case .computer(let level, _) = kind else { return }
        thinking = true
        let gen = generation
        let s = session
        let seed = UInt64.random(in: 0...UInt64.max)
        Task {
            let started = Date()
            let move: BotMove? = await Task.detached(priority: .userInitiated) { () -> BotMove? in
                s.botMove(level: UInt8(clamping: level), seed: seed)
            }.value
            // A short pause so the reply doesn't feel instant.
            let elapsed = Date().timeIntervalSince(started)
            let pause = delay ? max(0, 0.45 - elapsed) : 0
            if pause > 0 {
                try? await Task.sleep(nanoseconds: UInt64(pause * 1_000_000_000))
            }
            guard gen == self.generation else { return }
            self.thinking = false
            guard let move else {
                self.refresh()
                return
            }
            do {
                let played = try s.play(mv: move.uci, atMs: nowMs())
                self.applyIncrement(moverWhite: self.whiteToMove)
                self.app?.chessnut.opponentMove(played.uci)
                Feedback.speakMove(san: played.san)
                self.afterChange(animateBot: true)
            } catch {
                self.message = describe(error)
                self.refresh()
            }
        }
    }

    // MARK: Hints and analysis

    /// The Hint button: turn hints on (the game becomes unrated) and show
    /// the best move, the top lines and their odds.
    func hint() {
        guard helpAllowed, !isFinished else { return }
        hintsOn = true
        runHints(countHint: true)
    }

    func hideHints() {
        hintsOn = false
        hintMove = nil
        analysis = nil
    }

    private func runHints(countHint: Bool) {
        guard helpAllowed, !isFinished else { return }
        let gen = generation
        let s = session
        analysing = true
        Task {
            let result: (BotMove?, String?, String?) = await Task.detached(priority: .userInitiated) { () -> (BotMove?, String?, String?) in
                do {
                    var best: BotMove?
                    if countHint {
                        best = try s.hint(timeMs: 400)
                    }
                    let json = try s.analyseJson(depth: 10, nodes: 400_000, lines: 5, plies: 8, timeMs: 900)
                    return (best, json, nil)
                } catch {
                    return (nil, nil, describe(error))
                }
            }.value
            guard gen == self.generation else { return }
            self.analysing = false
            self.assistance = s.assistance()
            if let e = result.2 {
                self.message = e
                return
            }
            let parsed = result.1.flatMap { AnalysisResult.decode($0) }
            self.analysis = parsed
            if let best = result.0 {
                self.hintMove = best
            } else if let first = parsed?.lines.first?.moves.first {
                self.hintMove = BotMove(uci: first.uci, san: first.san)
            }
            if let h = self.hintMove, let u = UCIMove(h.uci) {
                self.app?.chessnut.light([Square.name(u.from), Square.name(u.to)])
            }
            self.save()
        }
    }

    // MARK: Buttons

    func takeBack() {
        guard takebackAllowed else { return }
        var n: UInt32 = 1
        if case .computer(_, let playerWhite) = kind {
            // Back to the player's move: two plies if the bot already replied.
            n = (whiteToMove == playerWhite && state.ply >= 2) ? 2 : 1
            if thinking { n = 1 }
            thinking = false
        }
        session.undo(n: n)
        resyncBoard()
        afterChange(animateBot: true)
    }

    func resign() {
        guard !isFinished else { return }
        let side: String
        switch kind {
        case .computer(_, let playerWhite): side = playerWhite ? "white" : "black"
        case .passAndPlay: side = state.turn
        case .online(let link): side = link.myColorWhite ? "white" : "black"
        }
        if let link = onlineLink {
            Task {
                do {
                    try await self.client(for: link).postForm("/api/board/game/\(APIClient.pathComponent(link.gameId))/resign", form: [:])
                } catch {
                    self.message = describe(error)
                }
            }
        }
        do {
            try session.resign(side: side)
        } catch {
            message = describe(error)
        }
        generation += 1
        thinking = false
        afterChange(animateBot: false)
    }

    /// Pass and play: both players agreed a draw.
    func agreeDraw() {
        guard isPassAndPlay, !isFinished else { return }
        agreedDraw = true
        session.agreeDraw()
        afterChange(animateBot: false)
    }

    func flip() {
        flipped.toggle()
    }

    // MARK: Clocks

    private func updateClock() {
        clockTimer?.invalidate()
        clockTimer = nil
        let runs: Bool
        if let link = onlineLink {
            runs = link.daysPerTurn == nil
        } else {
            runs = time.isTimed && state.ply >= 1
        }
        guard runs, !isFinished else { return }
        lastTick = Date()
        let t = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tickClock() }
        }
        RunLoop.main.add(t, forMode: .common)
        clockTimer = t
    }

    private func tickClock() {
        let now = Date()
        let dt = Int(now.timeIntervalSince(lastTick) * 1000)
        lastTick = now
        guard !isFinished else {
            clockTimer?.invalidate()
            return
        }
        if whiteToMove {
            whiteMs = max(0, whiteMs - dt)
        } else {
            blackMs = max(0, blackMs - dt)
        }
        // Online clocks are the server's; it ends the game.
        guard onlineLink == nil else { return }
        if whiteMs == 0 || blackMs == 0 {
            let flagged = whiteMs == 0 ? "white" : "black"
            // The core decides the result (a draw if the other side
            // couldn't have mated).
            try? session.flag(side: flagged)
            let st = session.state()
            let ended: (status: String, winner: String?) = (status: st.status, winner: st.winner)
            endRaw = ended
            statusOverride = statusText(st.status, winner: st.winner)
            generation += 1
            thinking = false
            afterChange(animateBot: false)
        }
    }

    func clockText(white: Bool) -> String {
        if let days = onlineLink?.daysPerTurn {
            return "\(days) d/move"
        }
        guard time.isTimed || onlineLink != nil else { return "" }
        let ms = max(0, white ? whiteMs : blackMs)
        let total = ms / 1000
        if ms > 0 && ms < 20_000 {
            return String(format: "%d:%02d.%d", total / 60, total % 60, (ms % 1000) / 100)
        }
        return String(format: "%d:%02d", total / 60, total % 60)
    }

    // MARK: Ending, saving, recording

    private func finish() {
        clockTimer?.invalidate()
        clockTimer = nil
        thinking = false
        if finishedNote == nil {
            finishedNote = statusLine
        }
        if live && !endAnnounced {
            endAnnounced = true
            Feedback.gameOver(lost: playerLost, title: result?.title ?? statusLine)
        }
        guard !recorded, let app else { return }
        recorded = true
        save()
        guard onlineLink == nil else { return }
        let speed = time.speed
        // A provisional rating against the bot until the server's arrives.
        if case .computer(let level, let playerWhite) = kind, session.ratable() {
            let me = app.rating(for: speed)
            let score: Score
            switch state.winner ?? "" {
            case "white": score = playerWhite ? .win : .loss
            case "black": score = playerWhite ? .loss : .win
            default: score = .draw
            }
            let after = ratingAfter(player: me, opponent: botRating(level: UInt8(clamping: level)), score: score)
            do {
                try app.store.setLocalRating(speed: speed, value: after)
            } catch {
                message = describe(error)
            }
            app.reloadRatings()
        }
        do {
            let json = try session.offlineRecordJson(
                clientId: id, white: whiteName, black: blackName, speed: speed, startedMs: startedMs
            )
            app.sync.enqueue(key: id, kind: "game", payload: Data(json.utf8))
            if app.auth.signedIn && !app.sync.lowData {
                Task { await app.sync.syncNow(reason: "game over") }
            }
        } catch {
            message = describe(error)
        }
        save()
    }

    func save() {
        guard let app else { return }
        var level: Int?
        var playerWhite: Bool?
        if case .computer(let l, let pw) = kind {
            level = l
            playerWhite = pw
        }
        let st = state
        let savedStatus: String
        let savedWinner: String?
        if let e = endRaw {
            savedStatus = e.status
            savedWinner = e.winner
        } else {
            savedStatus = st.status
            savedWinner = st.winner
        }
        let saved = SavedGame(
            id: id,
            kind: kind.key,
            level: level,
            playerWhite: playerWhite,
            white: whiteName,
            black: blackName,
            initialFen: st.initialFen == GameViewModel.startFen ? nil : st.initialFen,
            moves: st.uci,
            times: moveTimes,
            hints: assistance.hints,
            takebacks: assistance.takebacks,
            analysed: assistance.analysed,
            status: savedStatus,
            winner: savedWinner,
            speed: onlineLink?.speed ?? time.speed,
            startedMs: startedMs,
            updatedMs: nowMs(),
            minutes: time.minutes,
            increment: time.increment,
            whiteMs: whiteMs,
            blackMs: blackMs,
            online: onlineLink,
            recorded: recorded
        )
        do {
            try app.store.saveGame(id: id, record: saved.encoded(), nowMs: nowMs())
        } catch {
            message = describe(error)
        }
        app.reloadGames()
    }

    static let startFen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"

    // MARK: The Chessnut board

    func attachBoard() {
        guard let app else { return }
        let st = session.state()
        let fen: String? = st.initialFen == GameViewModel.startFen ? nil : st.initialFen
        let rotated: Bool
        switch kind {
        case .computer(_, let playerWhite): rotated = !playerWhite
        case .online(let link): rotated = !link.myColorWhite
        case .passAndPlay: rotated = false
        }
        app.chessnut.follow(fen: fen, moves: st.uci, rotated: rotated)
        app.chessnut.onEvent = { [weak self] event in
            self?.boardEvent(event)
        }
    }

    private func resyncBoard() {
        guard app?.chessnut.isConnected ?? false else { return }
        attachBoard()
    }

    func detach() {
        clockTimer?.invalidate()
        clockTimer = nil
        streamTask?.cancel()
        streamTask = nil
        generation += 1
        app?.chessnut.stopFollowing()
        save()
    }

    private func boardEvent(_ event: BoardEvent) {
        switch event {
        case .move(let uci, _):
            if myTurn {
                userMove(uci, fromBoard: true)
            } else {
                message = "That move was made on the board out of turn."
                resyncBoard()
            }
        case .takeback(let plies):
            if takebackAllowed {
                session.undo(n: plies)
                generation += 1
                thinking = false
                afterChange(animateBot: true)
            }
        default:
            break
        }
    }

    // MARK: Online games (Board API)

    private func client(for link: OnlineLink) -> APIClient {
        guard let app else { return APIClient(base: URL(string: AppSettings.defaultServer)!, token: nil) }
        return link.site == "lichess" ? app.auth.lichess : app.auth.server
    }

    private func sendOnlineMove(_ uci: String, link: OnlineLink) {
        Task {
            do {
                let path = "/api/board/game/\(APIClient.pathComponent(link.gameId))/move/\(uci)"
                try await self.client(for: link).postForm(path, form: [:])
            } catch {
                self.message = "The move wasn't sent: \(describe(error))"
                // Back to what the server has.
                self.applyServerMoves(self.serverMoves, status: "started", winner: nil, force: true)
            }
        }
    }

    /// Start following an online game: the live stream when data allows,
    /// else read the current state once (the stream's first line).
    func startOnline() {
        guard let link = onlineLink, let app else { return }
        streamTask?.cancel()
        let live = !app.sync.lowData && link.daysPerTurn == nil
        streamTask = Task { [weak self] in
            await self?.readStream(link: link, live: live)
        }
    }

    func refreshOnline() {
        startOnline()
    }

    private func readStream(link: OnlineLink, live: Bool) async {
        let api = client(for: link)
        var request = api.makeRequest("GET", "/api/board/game/stream/\(APIClient.pathComponent(link.gameId))")
        request.setValue("application/x-ndjson", forHTTPHeaderField: "Accept")
        request.timeoutInterval = live ? 3600 : 30
        do {
            let (bytes, response) = try await URLSession.shared.bytes(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            guard (200..<300).contains(status) else {
                message = "The game couldn't be loaded (\(status))."
                return
            }
            onlineConnected = live
            for try await line in bytes.lines {
                if Task.isCancelled { break }
                handleStreamLine(line)
                if !live { break }
            }
        } catch {
            if !Task.isCancelled {
                message = "Connection to the game lost: \(describe(error))"
            }
        }
        onlineConnected = false
    }

    private func handleStreamLine(_ line: String) {
        guard !line.trimmed.isEmpty, let obj = JSONAny.object(Data(line.utf8)) else { return }
        let type = obj["type"] as? String ?? ""
        var stateObj: [String: Any]?
        if type == "gameFull" {
            stateObj = obj["state"] as? [String: Any]
        } else if type == "gameState" {
            stateObj = obj
        }
        guard let s = stateObj else { return }
        if type == "gameFull", let initial = obj["initialFen"] as? String,
           initial != "startpos", !initial.isEmpty, initial != state.initialFen {
            do {
                session = try GameSession(fen: initial, mode: GameViewModel.mode(for: kind))
                refresh()
            } catch {
                message = describe(error)
            }
        }
        let moves = (s["moves"] as? String ?? "").split(separator: " ").map(String.init)
        if let w = s["wtime"] as? NSNumber { whiteMs = w.intValue }
        if let b = s["btime"] as? NSNumber { blackMs = b.intValue }
        let status = s["status"] as? String ?? "started"
        let winner = s["winner"] as? String
        applyServerMoves(moves, status: status, winner: winner)
    }

    /// Bring the session in line with the server's moves.
    private func applyServerMoves(_ moves: [String], status: String, winner: String?, force: Bool = false) {
        serverMoves = moves
        let mine = session.state().uci
        if !force && mine.count > moves.count && Array(mine.prefix(moves.count)) == moves {
            // Our move is on its way to the server; its echo will follow.
        } else if moves.count >= mine.count && Array(moves.prefix(mine.count)) == mine {
            for m in moves.dropFirst(mine.count) {
                do {
                    let played = try session.play(mv: m, atMs: nowMs())
                    app?.chessnut.opponentMove(played.uci)
                    if live { Feedback.speakMove(san: played.san) }
                } catch {
                    message = describe(error)
                    break
                }
            }
        } else {
            // Diverged (a refused move, a takeback): replay from the server.
            do {
                let st = session.state()
                session = try GameSession.resume(
                    fen: st.initialFen == GameViewModel.startFen ? nil : st.initialFen,
                    moves: moves.joined(separator: " "),
                    moveTimesMs: [],
                    mode: GameViewModel.mode(for: kind),
                    help: session.assistance()
                )
                resyncBoard()
            } catch {
                message = describe(error)
            }
        }
        if status != "started" && status != "created" && session.state().status == "started" {
            endRaw = (status: status, winner: winner)
            statusOverride = statusText(status, winner: winner)
        }
        afterChange(animateBot: false)
    }
}
