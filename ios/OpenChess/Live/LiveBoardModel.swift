import Combine
import OpenChessCore
import SwiftUI
import UIKit

/// A photo taken of the real board after a move (photos source).
struct PhotoShot {
    let image: CGImage
    let atMs: UInt64
    let angle: UInt8
    /// The board reader's whole reading (JSON), once read.
    var reading: String?
    var placement: String?
    var confidence: Double?
}

/// A move the photos might show.
struct MoveCandidate: Identifiable, Equatable {
    var uci: String
    var san: String
    var distance: Int
    var probability: Double
    var id: String { uci.isEmpty ? "none" : uci }
}

/// `Capture.matchPhotos`'s JSON.
struct PhotoMatchResult {
    var candidates: [MoveCandidate] = []
    var sure = false

    static func decode(_ json: String) -> PhotoMatchResult? {
        guard let obj = JSONAny.object(Data(json.utf8)) else { return nil }
        var out = PhotoMatchResult()
        out.sure = obj["sure"] as? Bool ?? false
        for c in obj["candidates"] as? [[String: Any]] ?? [] {
            out.candidates.append(MoveCandidate(
                uci: c["uci"] as? String ?? "",
                san: c["san"] as? String ?? "",
                distance: (c["distance"] as? NSNumber)?.intValue ?? 99,
                probability: (c["probability"] as? NSNumber)?.doubleValue ?? 0
            ))
        }
        return out
    }
}

enum PhotoStage: Equatable {
    /// Waiting for the next move's photo.
    case idle
    case reading
    /// Played from the photos without asking; Undo for a few seconds.
    case autoPlayed(String)
    /// Not sure: pick a move, fix the board, or tap the move.
    case choose
}

/// A game on a real board, recorded live: the moves (from a Chessnut,
/// photos or taps) mirrored in a local session for the board on screen,
/// sent to the server in order, labelled frames for the board reader,
/// and the stream's cutout.
@MainActor
final class LiveBoardModel: ObservableObject, Identifiable {
    nonisolated let id: String
    let setup: LiveSetup
    let gameURL: String
    let broadcastURL: String?

    @Published private(set) var state: GameState
    @Published private(set) var placement: Placement
    @Published private(set) var selected: Int?
    @Published private(set) var targets: Set<Int> = []
    @Published var pendingPromotion: (from: Int, to: Int)?
    @Published var flipped: Bool
    @Published var message: String?
    @Published private(set) var shakeSquare: Int?
    @Published private(set) var shakes = 0
    /// The real board doesn't match the game: what to put right.
    @Published private(set) var corrections: BoardCorrections?
    @Published private(set) var resultNote: String?
    @Published private(set) var framesKept = 0
    @Published private(set) var photoStage: PhotoStage = .idle
    @Published private(set) var candidates: [MoveCandidate] = []
    @Published private(set) var photoCount = 0
    @Published private(set) var photoNote: String?
    @Published private(set) var readerMissing = false
    @Published private(set) var cameraError: String?
    @Published private(set) var hintsOn = false
    @Published private(set) var usedHelp = false

    let stream: StreamController?
    let camera: CameraFeed?
    let sink: FrameSink
    let advice = AdviceModel()
    private var adviceWatch: AnyCancellable?

    private weak var app: AppModel?
    private let session: GameSession
    private let capture: Capture
    private let startedMs: UInt64
    private var input = MoveInput()
    private var legal: [String] = []
    private var reader: BoardReader?
    private var modelName = ""
    private var boardTimer: Timer?
    private var lastBoardNow: BoardNow?
    private var mismatchSince: Date?
    private var sawWaiting = false
    private var lastCaptureMs: Int64 = 0
    private var shots: [PhotoShot] = []
    private var photoGeneration = 0
    private var ended: (status: String, winner: String?)?
    private var agreedDraw = false
    private var autoTask: Task<Void, Never>?
    private var cameraOn = false
    private var started = false
    private var moveTimes: [UInt64] = []
    private var checkedPly: UInt32 = UInt32.max
    private var endAnnounced = false

    init(app: AppModel, setup: LiveSetup, gameId: String, url: String, broadcastURL: String?) throws {
        self.app = app
        self.setup = setup
        id = gameId
        gameURL = url
        self.broadcastURL = (broadcastURL ?? "").isEmpty ? nil : broadcastURL
        startedMs = nowMs()
        let s = try GameSession(fen: nil, mode: .passAndPlay)
        session = s
        capture = try Capture(fen: nil, moves: "", policy: defaultCapturePolicy())
        let st = s.state()
        state = st
        placement = Placement(fen: st.fen)
        flipped = !setup.recorderIsWhite
        let sink = FrameSink()
        self.sink = sink
        if setup.stream {
            stream = StreamController(sink: sink, portrait: app.settings.streamPortrait)
            camera = nil
        } else if setup.helpTrain || setup.source == .photos {
            stream = nil
            camera = CameraFeed(sink: sink)
        } else {
            stream = nil
            camera = nil
        }
        legal = s.legalUci()
        // The hint arrow follows the advice.
        adviceWatch = advice.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
        }
    }

    // MARK: Derived

    var whiteToMove: Bool { state.turn == "white" }
    var isFinished: Bool { state.status != "started" || ended != nil }
    var lastMove: UCIMove? { state.last.flatMap { UCIMove($0) } }
    var hintsAllowed: Bool { !setup.rated }
    /// "Analyse a copy": in unrated games, and in any game once it's over.
    var analysisAllowed: Bool { hintsAllowed || isFinished }
    var hasCamera: Bool { stream != nil || camera != nil }
    var canTap: Bool { setup.source != .chessnut && !isFinished && photoStage != .reading }

    /// The board as shown: the real board when it needs putting right.
    var shownPlacement: Placement { corrections?.seen ?? placement }
    var marks: BoardMarks { corrections?.marks ?? BoardMarks() }

    var checkSquare: Int? {
        guard corrections == nil, state.check else { return nil }
        return placement.kingSquare(white: whiteToMove)
    }

    var hintMove: UCIMove? {
        guard hintsOn, let r = advice.result, r.fen == state.fen,
              let uci = r.analysis?.lines.first?.moves.first?.uci else { return nil }
        return UCIMove(uci)
    }

    var result: ResultSummary? {
        guard isFinished else { return nil }
        if let e = ended {
            return GameText.result(status: e.status, winner: e.winner, fen: state.fen, agreed: agreedDraw)
        }
        return GameText.result(status: state.status, winner: state.winner, fen: state.fen)
    }

    var statusLine: String {
        if let r = result { return r.title }
        let side = whiteToMove ? "White" : "Black"
        return state.check ? "\(side) to move · check" : "\(side) to move"
    }

    // MARK: Starting and stopping

    func start() {
        guard !started, let app else { return }
        started = true
        app.otb.onResult = { [weak self] op, obj in
            self?.serverAnswered(op, obj)
        }
        if setup.source == .chessnut {
            app.chessnut.follow(fen: nil, moves: state.uci, rotated: !setup.recorderIsWhite)
            app.chessnut.onEvent = { [weak self] e in
                self?.boardEvent(e)
            }
            let t = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in
                Task { @MainActor in self?.boardTick() }
            }
            RunLoop.main.add(t, forMode: .common)
            boardTimer = t
        }
        Task {
            let (r, _) = await app.boardReader()
            self.reader = r
            self.readerMissing = r == nil
            if let r, let card = ModelCardInfo.decode(r.cardJson()) {
                self.modelName = [card.name, card.version].filter { !$0.isEmpty }.joined(separator: " ")
            }
            self.startSampling()
        }
        startCamera()
        updateCutout()
    }

    /// Frames from the camera, about two a second, read and offered for
    /// labelling: with a connected board, or from the stream's camera.
    private func startSampling() {
        guard setup.helpTrain, setup.source == .chessnut || setup.stream else { return }
        let reader = self.reader
        sink.setSampler(interval: 500) { [weak self] image, atMs in
            var placement: String?
            var confidence: Double?
            if let reader, let json = FrameTools.read(image, reader: reader), let r = BoardReadingResult.decode(json) {
                placement = r.placement
                confidence = r.confidence
            }
            let p = placement
            let c = confidence
            Task { @MainActor in
                // Hold the frame a moment, so a change just after it is seen too.
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                self?.offer(image: image, atMs: atMs, angle: 0, readerPlacement: p, confidence: c)
            }
        }
    }

    func startCamera() {
        guard !cameraOn, hasCamera else { return }
        cameraOn = true
        if let stream {
            Task {
                await stream.startCamera()
                self.updateCutout()
            }
        } else if let camera {
            Task {
                guard await CameraFeed.authorize() else {
                    self.cameraError = "Camera access is off for open-chess in Settings."
                    return
                }
                camera.setRotation(CameraFeed.angle(for: currentInterfaceOrientation()))
                self.cameraError = await camera.start()
            }
        }
    }

    private func stopCamera() {
        guard cameraOn else { return }
        cameraOn = false
        if let stream {
            Task {
                await stream.stopStream()
                await stream.stopCamera()
            }
        }
        camera?.stop()
    }

    func orientationChanged() {
        stream?.orientationChanged()
        camera?.setRotation(CameraFeed.angle(for: currentInterfaceOrientation()))
    }

    /// The app went to the background: the stream stops, the camera too.
    func enterBackground() {
        stopCamera()
    }

    func enterForeground() {
        startCamera()
    }

    func close() {
        boardTimer?.invalidate()
        boardTimer = nil
        autoTask?.cancel()
        sink.setSampler(interval: 0, nil)
        if setup.source == .chessnut {
            app?.chessnut.stopFollowing()
        }
        stopCamera()
        app?.otb.onResult = nil
        UIApplication.shared.isIdleTimerDisabled = false
        save()
    }

    // MARK: Streaming

    func startStream() {
        guard let stream, let app else { return }
        let key = Keychain.get(AppSettings.streamKeyAccount) ?? ""
        guard let url = StreamTarget.url(service: app.settings.streamService, custom: app.settings.streamURL, key: key) else {
            message = "Add the stream key (and the server, for Custom) in Streaming settings first."
            return
        }
        let kbps = app.settings.streamKbps
        let record = app.settings.streamRecord
        Task {
            await stream.startStream(url: url, bitrate: kbps * 1000, record: record)
        }
    }

    func stopStream() {
        guard let stream else { return }
        Task { await stream.stopStream() }
    }

    private func updateCutout() {
        guard let stream else { return }
        let view = StreamCutoutView(
            placement: placement,
            lastMove: lastMove,
            check: checkSquare,
            white: setup.white,
            black: setup.black,
            lines: MoveWindow.lines(san: state.san),
            result: result?.title,
            width: stream.cutoutWidth
        )
        stream.setCutout(StreamCutoutView.render(view))
    }

    // MARK: Moves

    func tap(_ sq: Int) {
        guard canTap else { return }
        let outcome = input.tap(sq, legal: legal, placement: placement, whiteToMove: whiteToMove)
        selected = input.selected
        targets = input.targets
        switch outcome {
        case .none:
            break
        case .move(let m):
            confirmMove(m)
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
        confirmMove(Square.name(p.from) + Square.name(p.to) + String(piece))
    }

    func cancelPromotion() {
        pendingPromotion = nil
        input.clear()
        selected = nil
        targets = []
    }

    /// A move entered on the phone (a tap, a photo candidate, the fixed board).
    func confirmMove(_ uci: String) {
        if setup.source == .photos && !shots.isEmpty {
            confirmPhotoMove(uci)
        } else {
            playMove(uci, connectedBoard: false)
        }
    }

    /// Play a move here, label what follows, and send it to the server.
    @discardableResult
    private func playMove(_ uci: String, connectedBoard: Bool, captureAt: Int64? = nil) -> Bool {
        guard !isFinished else { return false }
        do {
            let played = try session.play(mv: uci, atMs: nowMs())
            let stamp = max(lastCaptureMs + 1, captureAt ?? Int64(nowMs()))
            lastCaptureMs = stamp
            _ = try? capture.movePlayed(uci: played.uci, atMs: stamp, connectedBoard: connectedBoard)
            moveTimes.append(nowMs())
            app?.otb.enqueue(.move(gameId: id, uci: played.uci))
            Feedback.speakMove(san: played.san, after: session.state())
            afterChange()
            return true
        } catch {
            message = describe(error)
            if connectedBoard { resyncBoard() }
            return false
        }
    }

    func undoLast() {
        undo(plies: 1, fromBoard: false)
    }

    private func undo(plies: UInt32, fromBoard: Bool) {
        guard !isFinished, state.ply > 0 else { return }
        let n = min(plies, state.ply)
        session.undo(n: n)
        let stamp = max(lastCaptureMs + 1, Int64(nowMs()))
        lastCaptureMs = stamp
        capture.undo(plies: n, atMs: stamp, connectedBoard: setup.source == .chessnut)
        if moveTimes.count >= Int(n) { moveTimes.removeLast(Int(n)) }
        app?.otb.enqueueUndo(gameId: id, plies: Int(n))
        if setup.source == .chessnut && !fromBoard { resyncBoard() }
        autoTask?.cancel()
        resetPhotos()
        afterChange()
    }

    private func afterChange() {
        state = session.state()
        placement = Placement(fen: state.fen)
        legal = isFinished ? [] : session.legalUci()
        input.clear()
        selected = nil
        targets = []
        if state.check && !isFinished && state.ply != checkedPly {
            checkedPly = state.ply
            Feedback.check()
        }
        if isFinished {
            finish()
        } else if hintsOn {
            advice.run(fen: state.fen, delay: 0.3)
        }
        updateCutout()
        save()
    }

    private func finish() {
        boardTimer?.invalidate()
        boardTimer = nil
        clearCorrections()
        guard !endAnnounced else { return }
        endAnnounced = true
        let winner = ended?.winner ?? state.winner
        let mine = setup.recorderIsWhite ? "white" : "black"
        Feedback.gameOver(lost: winner != nil && winner != mine, title: result?.title ?? statusLine)
        if setup.source == .chessnut {
            app?.chessnut.lightsOff()
        }
    }

    // MARK: Ending

    /// `side` resigns.
    func resign(side: String) {
        guard !isFinished else { return }
        try? session.resign(side: side)
        end(status: "resign", winner: side == "white" ? "black" : "white")
    }

    func agreeDraw() {
        guard !isFinished else { return }
        agreedDraw = true
        session.agreeDraw()
        end(status: "draw", winner: nil)
    }

    /// `side` ran out of time on the real clock (a draw if the other side
    /// couldn't have mated: the core and the server both decide so).
    func flag(side: String) {
        guard !isFinished else { return }
        try? session.flag(side: side)
        let st = session.state()
        end(status: "outoftime", winner: st.winner, sent: side == "white" ? "black" : "white")
    }

    func abort() {
        guard !isFinished else { return }
        end(status: "aborted", winner: nil)
    }

    private func end(status: String, winner: String?, sent: String? = nil) {
        ended = (status: status, winner: winner)
        let sendWinner: String? = status == "outoftime" ? sent : winner
        app?.otb.enqueue(.end(gameId: id, status: status, winner: sendWinner))
        afterChange()
    }

    private func serverAnswered(_ op: OTBOp, _ obj: [String: Any]) {
        guard op.gameId == id else { return }
        if let e = obj["error"] as? String {
            message = "The server refused \(op.summary): \(e)"
            return
        }
        if op.kind == "end" {
            if (obj["confirmation"] as? String) == "pending" {
                resultNote = "Rated once \(setup.opponent) confirms the result in their app."
            } else {
                resultNote = "Recorded on the server."
            }
        }
    }

    // MARK: Hints (unrated games only)

    func toggleHints() {
        guard hintsAllowed else { return }
        if hintsOn {
            hintsOn = false
            advice.clear()
        } else {
            hintsOn = true
            usedHelp = true
            advice.run(fen: state.fen)
        }
    }

    /// The game so far on an analysis board (unrated games, or once over).
    func analysisCopy() -> AnalysisBoardModel? {
        guard analysisAllowed else { return nil }
        do {
            return try AnalysisBoardModel(
                fen: nil, moves: state.uci, title: "Analysis · \(setup.white) – \(setup.black)", flipped: flipped
            )
        } catch {
            message = describe(error)
            return nil
        }
    }

    // MARK: The Chessnut board

    private func resyncBoard() {
        guard setup.source == .chessnut, let app else { return }
        app.chessnut.follow(fen: nil, moves: state.uci, rotated: !setup.recorderIsWhite)
    }

    private func boardEvent(_ event: BoardEvent) {
        switch event {
        case .move(let uci, _):
            sawWaiting = false
            playMove(uci, connectedBoard: true)
        case .takeback(let plies):
            sawWaiting = false
            undo(plies: plies, fromBoard: true)
        case .waiting:
            sawWaiting = true
        case .caught, .newGame:
            sawWaiting = false
        default:
            break
        }
    }

    /// Every 100 ms: what the board shows, for labelling frames and for
    /// spotting a position the game can't reach.
    private func boardTick() {
        guard let app, let now = app.chessnut.boardNow() else { return }
        if now != lastBoardNow {
            capture.boardState(placement: now.placement, settled: now.settled, atMs: Int64(nowMs()))
            lastBoardNow = now
        }
        guard !isFinished else { return }
        let seen = now.placement.split(separator: " ").first.map(String.init) ?? now.placement
        if seen == placement.fenField {
            mismatchSince = nil
            sawWaiting = false
            if corrections != nil { clearCorrections() }
            return
        }
        guard now.settled else { return }
        let since = mismatchSince ?? Date()
        mismatchSince = since
        let held = Date().timeIntervalSince(since)
        guard held >= 1.5 || (sawWaiting && held >= 0.8) else { return }
        if corrections?.seen.fenField != seen,
           let c = BoardCorrections.make(expected: placement.fenField, seen: seen) {
            showCorrections(c)
        }
    }

    private func showCorrections(_ c: BoardCorrections) {
        let first = corrections == nil
        let changed = corrections?.summary != c.summary
        corrections = c
        if setup.source == .chessnut {
            app?.chessnut.light(c.squares)
        }
        if first {
            Feedback.warning()
        }
        if changed {
            Feedback.announce(c.summary)
            Feedback.say(c.summary)
        }
    }

    private func clearCorrections() {
        guard corrections != nil else { return }
        corrections = nil
        if setup.source == .chessnut {
            app?.chessnut.lightsOff()
        }
    }

    // MARK: Photos after each move

    /// Take a photo with the camera on screen.
    func takePhoto() {
        Task {
            guard let image = await self.sink.snapshot() else {
                self.photoNote = "No picture from the camera; try again."
                return
            }
            self.addPhoto(image, atMs: nowMs())
        }
    }

    /// A photo chosen or taken with the system camera.
    func addPhoto(_ image: UIImage) {
        guard let cg = ImageTools.fit(image, maxSide: 1280).cgImage else { return }
        addPhoto(cg, atMs: nowMs())
    }

    private func addPhoto(_ image: CGImage, atMs: UInt64) {
        guard setup.source == .photos, !isFinished else { return }
        let angle = UInt8(clamping: shots.count)
        shots.append(PhotoShot(image: image, atMs: atMs, angle: angle))
        photoCount = shots.count
        readAndMatch()
    }

    private func resetPhotos() {
        photoGeneration += 1
        shots = []
        candidates = []
        photoCount = 0
        photoNote = nil
        if case .autoPlayed = photoStage {
            // Keep the Undo banner's own timer in charge.
        } else {
            photoStage = .idle
        }
    }

    private func readAndMatch() {
        guard let reader else {
            photoNote = "This build has no board reader: tap the move on the board."
            photoStage = .choose
            return
        }
        photoStage = .reading
        photoNote = nil
        let gen = photoGeneration
        let pending: [(Int, CGImage)] = shots.enumerated().filter { $0.element.reading == nil }.map { ($0.offset, $0.element.image) }
        Task {
            let results: [(Int, String?)] = await Task.detached(priority: .userInitiated) { () -> [(Int, String?)] in
                pending.map { item in (item.0, FrameTools.read(item.1, reader: reader)) }
            }.value
            guard gen == self.photoGeneration else { return }
            for (i, json) in results where i < self.shots.count {
                self.shots[i].reading = json
                if let json, let r = BoardReadingResult.decode(json) {
                    self.shots[i].placement = r.placement
                    self.shots[i].confidence = r.confidence
                }
            }
            self.match()
        }
    }

    private func match() {
        let readings = shots.compactMap { $0.reading }
        guard !readings.isEmpty else {
            photoNote = "The board reader couldn't read that photo. Try again, or tap the move."
            photoStage = .choose
            return
        }
        let m: PhotoMatchResult?
        do {
            m = PhotoMatchResult.decode(try capture.matchPhotos(readings: readings))
        } catch {
            photoNote = describe(error)
            photoStage = .choose
            return
        }
        let found = m?.candidates ?? []
        candidates = found.filter { !$0.uci.isEmpty }
        let top = found.first
        if let top, m?.sure == true, !top.uci.isEmpty {
            clearCorrections()
            autoPlay(top)
            return
        }
        if let top, top.uci.isEmpty, top.distance <= 1 {
            clearCorrections()
            photoNote = "That looks like the position before the move. Take the photo once the move is made."
            photoStage = .choose
            return
        }
        if let top, top.distance >= 2, let seen = shots.last?.placement,
           let c = BoardCorrections.make(expected: placement.fenField, seen: seen) {
            // No legal move explains the photo: show what to put right.
            showCorrections(c)
            photoNote = "That isn't one legal move from the game. Put the board right (shown in red), then take another photo."
        } else {
            clearCorrections()
        }
        photoStage = .choose
    }

    private func autoPlay(_ c: MoveCandidate) {
        let san = c.san
        confirmPhotoMove(c.uci)
        photoStage = .autoPlayed(san)
        autoTask?.cancel()
        autoTask = Task {
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            if Task.isCancelled { return }
            if case .autoPlayed = self.photoStage {
                self.photoStage = .idle
            }
        }
    }

    /// The move the photos show, confirmed: play it, labelled from the
    /// first photo's time, and offer the photos for labelling.
    func confirmPhotoMove(_ uci: String) {
        let taken = shots
        let firstAt = taken.map { $0.atMs }.min() ?? nowMs()
        // The board showed the new position when the first photo was taken.
        let settle = Int64(defaultCapturePolicy().settleMs)
        let at = Int64(firstAt) - settle - 1
        let ok = playMove(uci, connectedBoard: false, captureAt: at)
        clearCorrections()
        resetPhotos()
        photoStage = .idle
        guard ok, setup.helpTrain else { return }
        Task {
            try? await Task.sleep(nanoseconds: 1_000_000_000)
            for s in taken {
                self.offer(image: s.image, atMs: s.atMs, angle: s.angle, readerPlacement: s.placement, confidence: s.confidence)
            }
        }
    }

    /// The move from a board fixed in the editor, if it's one legal move away.
    func moveMatching(_ target: Placement) -> String? {
        let fen = state.fen
        for m in legal {
            guard let s = try? GameSession(fen: fen, mode: .passAndPlay) else { continue }
            guard (try? s.play(mv: m, atMs: 0)) != nil else { continue }
            if Placement(fen: s.state().fen) == target {
                return m
            }
        }
        return nil
    }

    // MARK: Labelled frames

    private func offer(image: CGImage, atMs: UInt64, angle: UInt8, readerPlacement: String?, confidence: Double?) {
        guard setup.helpTrain, let app, app.settings.photoConsentGiven else { return }
        let d = capture.offerFrame(atMs: Int64(atMs), nowMs: Int64(nowMs()), angle: angle, readerPlacement: readerPlacement)
        guard d.keep, let label = d.label else { return }
        framesKept += 1
        let source = setup.source == .chessnut ? "board" : "player"
        let gameId = id
        let model = modelName
        let distance = d.distance
        Task {
            let jpeg: Data? = await Task.detached(priority: .utility) { () -> Data? in
                FrameTools.jpeg(image, maxSide: 1280, quality: 0.8)
            }.value
            guard let jpeg else { return }
            let key = "photo-" + newClientId()
            var body: [String: Any] = [
                "clientId": key,
                "image": jpeg.base64EncodedString(),
                "placement": label,
                "model": model,
                "labelSource": source,
                "gameId": gameId,
                "angle": Int(angle),
                "atMs": Int64(atMs),
            ]
            if let confidence { body["confidence"] = confidence }
            if let distance { body["distance"] = Int(distance) }
            app.sync.enqueue(key: key, kind: "photo", payload: JSONAny.data(body))
        }
    }

    // MARK: Saving

    private func save() {
        guard let app else { return }
        let st = state
        let status = ended?.status ?? st.status
        let winner = ended != nil ? ended?.winner : st.winner
        var times = moveTimes
        while times.count < st.uci.count { times.append(nowMs()) }
        if times.count > st.uci.count { times = Array(times.prefix(st.uci.count)) }
        let saved = SavedGame(
            id: "otb:" + id,
            kind: "otb",
            level: nil,
            playerWhite: setup.recorderIsWhite,
            white: setup.white,
            black: setup.black,
            initialFen: nil,
            moves: st.uci,
            times: times,
            hints: usedHelp ? 1 : 0,
            takebacks: 0,
            analysed: usedHelp,
            status: status,
            winner: winner,
            speed: setup.speed,
            startedMs: startedMs,
            updatedMs: nowMs(),
            minutes: 0,
            increment: 0,
            whiteMs: nil,
            blackMs: nil,
            online: nil,
            recorded: true
        )
        do {
            try app.store.saveGame(id: saved.id, record: saved.encoded(), nowMs: nowMs())
        } catch {
            message = describe(error)
        }
        app.reloadGames()
    }
}
