import SwiftUI
import UIKit

/// A real-board game on screen: the camera (or the stream's picture), the
/// board as recorded, the moves, the photo prompts, the controls, the
/// stream and the overlay link.
@MainActor
struct LiveBoardScreen: View {
    @ObservedObject var live: LiveBoardModel
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var settings: AppSettings
    @EnvironmentObject var otb: OTBSender
    @EnvironmentObject var auth: AuthManager
    @Environment(\.scenePhase) var scenePhase

    @State var askResign = false
    @State var askFlag = false
    @State var askDraw = false
    @State var askAbort = false
    @State var askLeave = false
    @State var showStreamSettings = false
    @State var showPicker = false
    @State var showFix = false
    @State var hideResult = false
    @State var analysis: AnalysisBoardModel?

    var body: some View {
        NavigationStack {
            GeometryReader { geo in
                let wide = geo.size.width > geo.size.height * 1.1
                Group {
                    if wide {
                        HStack(alignment: .top, spacing: 16) {
                            VStack(spacing: 12) {
                                if live.hasCamera {
                                    cameraView
                                }
                                if !live.hasCamera || geo.size.width > 900 {
                                    boardColumn
                                }
                            }
                            .frame(width: geo.size.width * 0.52)
                            ScrollView {
                                VStack(alignment: .leading, spacing: 12) {
                                    if live.hasCamera && geo.size.width <= 900 {
                                        boardColumn.frame(maxWidth: 380)
                                    }
                                    panel
                                }
                            }
                        }
                        .padding(16)
                    } else {
                        ScrollView {
                            VStack(spacing: 12) {
                                if live.hasCamera {
                                    cameraView
                                }
                                boardColumn
                                panel
                            }
                            .padding(.horizontal, 12)
                            .padding(.vertical, 8)
                            .frame(maxWidth: 720)
                            .frame(maxWidth: .infinity)
                        }
                    }
                }
                .onChange(of: geo.size) { _, _ in
                    live.orientationChanged()
                }
            }
            .ocScreen()
            .navigationTitle("\(live.setup.white) – \(live.setup.black)")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") {
                        if live.isFinished { close() } else { askLeave = true }
                    }
                }
                if live.stream != nil {
                    ToolbarItem(placement: .primaryAction) {
                        Button {
                            showStreamSettings = true
                        } label: {
                            Image(systemName: "gearshape")
                        }
                        .accessibilityLabel("Streaming settings")
                    }
                }
            }
            .overlay {
                if live.pendingPromotion != nil {
                    ZStack {
                        Color.black.opacity(0.35).ignoresSafeArea()
                        PromotionPicker(
                            white: live.whiteToMove,
                            onPick: { live.promote($0) },
                            onCancel: { live.cancelPromotion() }
                        )
                    }
                }
            }
            .confirmationDialog("Who resigns?", isPresented: $askResign, titleVisibility: .visible) {
                Button("\(live.setup.white) (White) resigns", role: .destructive) { live.resign(side: "white") }
                Button("\(live.setup.black) (Black) resigns", role: .destructive) { live.resign(side: "black") }
                Button("Cancel", role: .cancel) {}
            }
            .confirmationDialog("Whose flag fell?", isPresented: $askFlag, titleVisibility: .visible) {
                Button("\(live.setup.white) (White) ran out of time", role: .destructive) { live.flag(side: "white") }
                Button("\(live.setup.black) (Black) ran out of time", role: .destructive) { live.flag(side: "black") }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("On the real clock. If the other side couldn't have mated, it's a draw.")
            }
            .alert("Agree a draw?", isPresented: $askDraw) {
                Button("Draw", role: .destructive) { live.agreeDraw() }
                Button("Cancel", role: .cancel) {}
            }
            .alert("Abort this game?", isPresented: $askAbort) {
                Button("Abort", role: .destructive) { live.abort() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("It ends with no result.")
            }
            .alert("Leave this game?", isPresented: $askLeave) {
                Button("Abort and leave", role: .destructive) {
                    live.abort()
                    close()
                }
                Button("Stay", role: .cancel) {}
            } message: {
                Text("A real-board game can't be reopened here. Leaving aborts it; to record a result, use Resign, Draw or Flag first.")
            }
            .alert("open-chess", isPresented: Binding(
                get: { live.message != nil },
                set: { if !$0 { live.message = nil } }
            )) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(live.message ?? "")
            }
            .sheet(isPresented: $showStreamSettings) {
                NavigationStack {
                    StreamSettingsView()
                        .toolbar {
                            ToolbarItem(placement: .confirmationAction) {
                                Button("Done") { showStreamSettings = false }
                            }
                        }
                }
                .environmentObject(settings)
                .ocAppearance()
            }
            .sheet(isPresented: $showPicker) {
                CameraPicker(source: CameraPicker.cameraAvailable ? .camera : .photoLibrary) { image in
                    showPicker = false
                    if let image { live.addPhoto(image) }
                }
                .ignoresSafeArea()
            }
            .sheet(isPresented: $showFix) {
                FixBoardSheet(live: live) { showFix = false }
                    .ocAppearance()
            }
            .fullScreenCover(item: $analysis) { a in
                AnalysisCover(board: a) { analysis = nil }
                    .ocAppearance()
            }
            .onAppear { live.start() }
            .onChange(of: scenePhase) { _, phase in
                switch phase {
                case .background: live.enterBackground()
                case .active: live.enterForeground()
                default: break
                }
            }
        }
    }

    private func close() {
        live.close()
        model.liveBoard = nil
    }

    // MARK: Camera

    @ViewBuilder
    private var cameraView: some View {
        if let stream = live.stream {
            StreamPreview(controller: stream)
                .aspectRatio(stream.portrait ? 9.0 / 16.0 : 16.0 / 9.0, contentMode: .fit)
                .frame(maxHeight: 420)
                .background(Color.black)
                .clipShape(RoundedRectangle(cornerRadius: OCMetrics.corner))
        } else if let camera = live.camera {
            CameraPreview(session: camera.session)
                .aspectRatio(4.0 / 3.0, contentMode: .fit)
                .frame(maxHeight: 300)
                .clipShape(RoundedRectangle(cornerRadius: OCMetrics.corner))
        }
        if let e = live.cameraError ?? live.stream?.cameraError {
            Text(e).font(OCFont.text(13)).foregroundStyle(Color.ocDanger)
        }
    }

    // MARK: Board

    private var boardColumn: some View {
        VStack(spacing: 8) {
            PlayerRow(
                name: live.flipped ? live.setup.white : live.setup.black,
                white: live.flipped,
                clock: "",
                active: !live.isFinished && live.whiteToMove == live.flipped
            )
            BoardView(
                placement: live.shownPlacement,
                flipped: live.flipped,
                selected: live.selected,
                lastMove: live.corrections == nil ? live.lastMove : nil,
                hint: live.corrections == nil ? live.hintMove : nil,
                targets: live.targets,
                check: live.checkSquare,
                shakeSquare: live.shakeSquare,
                shakes: live.shakes,
                marks: live.marks,
                onTap: { live.tap($0) }
            )
            .overlay {
                if let summary = live.result, !hideResult {
                    ResultCard(
                        summary: summary,
                        detail: live.resultNote ?? (live.setup.rated ? "Rated once \(live.setup.opponent) confirms it." : nil),
                        onAnalyse: { analysis = live.analysisCopy() },
                        onRematch: nil,
                        onDone: { close() },
                        onHide: { hideResult = true }
                    )
                    .padding(12)
                }
            }
            PlayerRow(
                name: live.flipped ? live.setup.black : live.setup.white,
                white: !live.flipped,
                clock: "",
                active: !live.isFinished && live.whiteToMove != live.flipped
            )
            if let c = live.corrections {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Put the board right")
                        .font(OCFont.text(14, weight: .bold))
                        .foregroundStyle(Color.ocDanger)
                    ForEach(c.sentences, id: \.self) { s in
                        Text(s).font(OCFont.text(13))
                    }
                }
                .ocCard(padding: 10)
                .accessibilityElement(children: .combine)
            }
        }
    }

    // MARK: Panel

    private var panel: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Text(live.statusLine).font(OCFont.text(15, weight: .semibold))
                Spacer()
                OCBadge(text: live.setup.rated ? "Rated" : "Casual", color: .ocMuted)
                if live.usedHelp {
                    OCBadge(text: "Hints on · assisted")
                }
            }
            serverLine
            if live.setup.source == .photos && !live.isFinished {
                photoCard
            }
            if live.hintsOn {
                AdvicePanel(advice: live.advice)
            }
            MoveListView(san: live.state.san, startsWhite: true, words: MoveWords.words(for: live.state))
            controls
            if live.setup.helpTrain {
                HStack {
                    Image(systemName: "camera.metering.matrix").foregroundStyle(Color.ocAction)
                    Text("Frames kept: \(live.framesKept)").font(OCFont.mono(14))
                    Spacer()
                    if !settings.photoConsentGiven {
                        Text("Photo licence not accepted").font(OCFont.text(12)).foregroundStyle(Color.ocAmber)
                    }
                }
                .ocCard(padding: 10)
            }
            if let stream = live.stream {
                StreamControls(
                    stream: stream,
                    onStart: { live.startStream() },
                    onStop: { live.stopStream() }
                )
            }
            if let link = live.broadcastURL {
                overlayCard(link)
            }
        }
    }

    @ViewBuilder
    private var serverLine: some View {
        let waiting = otb.waiting(gameId: live.id)
        if !auth.signedIn {
            SignInPrompt(message: "Sign in again to send this game's moves to the server. They wait here until then.")
        }
        if waiting > 0 || otb.lastError != nil {
            VStack(alignment: .leading, spacing: 2) {
                if waiting > 0 {
                    Text("\(waiting) waiting to send to the server")
                        .font(OCFont.text(12)).foregroundStyle(Color.ocAmber)
                }
                if let e = otb.lastError {
                    Text(e).font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
                }
            }
        }
    }

    private var controls: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 100), spacing: 8)], spacing: 8) {
            Button("Undo last move") { live.undoLast() }
                .disabled(live.isFinished || live.state.ply == 0)
            if live.hintsAllowed {
                Button(live.hintsOn ? "Hide hints" : "Hint") { live.toggleHints() }
                    .disabled(live.isFinished)
            }
            if live.analysisAllowed {
                Button("Analyse a copy") { analysis = live.analysisCopy() }
            }
            Button("Flip") { live.flipped.toggle() }
            Button("Resign") { askResign = true }
                .disabled(live.isFinished)
            Button("Draw") { askDraw = true }
                .disabled(live.isFinished)
            Button("Flag") { askFlag = true }
                .disabled(live.isFinished)
            Button("Abort") { askAbort = true }
                .disabled(live.isFinished)
            if live.isFinished && hideResult {
                Button("Show result") { hideResult = false }
            }
        }
        .buttonStyle(OCButtonStyle(kind: .secondary, compact: true))
    }

    // MARK: Photos

    private var photoCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            switch live.photoStage {
            case .idle:
                Text(live.photoCount == 0
                     ? "After each move, take a photo of the board from above."
                     : "Add another angle, or wait for the move.")
                    .font(OCFont.text(14))
                    .foregroundStyle(Color.ocMuted)
                Button {
                    takePhoto()
                } label: {
                    Label("Take a photo", systemImage: "camera")
                }
                .buttonStyle(OCButtonStyle(kind: .primary))
            case .reading:
                HStack {
                    ProgressView()
                    Text("Reading the board…").foregroundStyle(Color.ocMuted)
                }
            case .autoPlayed(let san):
                HStack {
                    Text("Played \(san)").font(OCFont.text(15, weight: .bold))
                    Spacer()
                    Button("Undo") { live.undoLast() }
                        .buttonStyle(OCButtonStyle(kind: .secondary, compact: true))
                }
            case .choose:
                Text(live.candidates.isEmpty ? "Which move was it? Tap it on the board." : "Which move was it?")
                    .font(OCFont.text(14, weight: .semibold))
                ForEach(live.candidates) { c in
                    Button {
                        live.confirmMove(c.uci)
                    } label: {
                        HStack {
                            Text(c.san).font(OCFont.mono(15, weight: .bold))
                            Spacer()
                            Text("\(Int((c.probability * 100).rounded()))%")
                                .font(OCFont.mono(13)).foregroundStyle(Color.ocMuted)
                        }
                    }
                    .buttonStyle(OCButtonStyle(kind: .secondary, compact: true))
                }
                HStack(spacing: 8) {
                    Button("Add another angle") { takePhoto() }
                    Button("Edit the board") { showFix = true }
                }
                .buttonStyle(OCButtonStyle(kind: .secondary, compact: true))
            }
            if let n = live.photoNote {
                Text(n).font(OCFont.text(13)).foregroundStyle(Color.ocMuted)
            }
            if live.readerMissing {
                Text("No board reader in this build: tap each move on the board.")
                    .font(OCFont.text(12)).foregroundStyle(Color.ocAmber)
            }
        }
        .ocCard(padding: 12)
    }

    private func takePhoto() {
        if live.hasCamera && CameraFeed.available {
            live.takePhoto()
        } else {
            showPicker = true
        }
    }

    // MARK: Overlay link

    private func overlayCard(_ link: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionTitle(text: "Live board overlay")
            Text(link)
                .font(OCFont.mono(12))
                .lineLimit(2)
                .textSelection(.enabled)
            if let url = URL(string: link) {
                ShareLink(item: url) {
                    Label("Share the link", systemImage: "square.and.arrow.up")
                }
                .buttonStyle(OCButtonStyle(kind: .secondary, compact: true))
            }
            Text("Add as a Browser Source in OBS; add &bg=transparent.")
                .font(OCFont.text(12))
                .foregroundStyle(Color.ocMuted)
        }
        .ocCard(padding: 12)
    }
}

/// Start/stop the stream, its state and how long it has been live.
@MainActor
struct StreamControls: View {
    @ObservedObject var stream: StreamController
    let onStart: () -> Void
    let onStop: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Circle()
                    .fill(dotColor)
                    .frame(width: 10, height: 10)
                Text(stream.stateText).font(OCFont.text(14, weight: .semibold))
                Spacer()
                if let start = stream.startedAt {
                    TimelineView(.periodic(from: start, by: 1)) { context in
                        Text(elapsedText(seconds: Int(context.date.timeIntervalSince(start))))
                            .font(OCFont.mono(14, weight: .semibold))
                    }
                }
            }
            if stream.isActive {
                Button("Stop stream", action: onStop)
                    .buttonStyle(OCButtonStyle(kind: .danger))
            } else {
                Button("Start stream", action: onStart)
                    .buttonStyle(OCButtonStyle(kind: .primary))
            }
            if let n = stream.recordingNote {
                Text(n).font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
            }
        }
        .ocCard(padding: 12)
    }

    private var dotColor: Color {
        switch stream.state {
        case .live: return .ocDanger
        case .connecting: return .ocAmber
        case .failed: return .ocDanger.opacity(0.5)
        case .idle: return .ocMuted
        }
    }
}

/// Fix the board by hand (photos source): the editor, then the one legal
/// move that gives that position.
@MainActor
struct FixBoardSheet: View {
    let live: LiveBoardModel
    let onDone: () -> Void
    @StateObject var editor: BoardEditorModel
    @State var error: String?

    init(live: LiveBoardModel, onDone: @escaping () -> Void) {
        self.live = live
        self.onDone = onDone
        _editor = StateObject(wrappedValue: BoardEditorModel(placement: live.placement))
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text("Set the board as it is now, after the move.")
                        .font(OCFont.text(14)).foregroundStyle(Color.ocMuted)
                    BoardView(
                        placement: editor.placement,
                        flipped: live.flipped,
                        onTap: { editor.tap($0) }
                    )
                    PiecePalette(editor: editor)
                    if let error {
                        Text(error).font(OCFont.text(13)).foregroundStyle(Color.ocDanger)
                    }
                    Button("Use this position") {
                        if let m = live.moveMatching(editor.placement) {
                            live.confirmMove(m)
                            onDone()
                        } else {
                            error = "That isn't one legal move from the game's position."
                        }
                    }
                    .buttonStyle(OCButtonStyle(kind: .primary))
                }
                .padding(16)
            }
            .ocScreen()
            .navigationTitle("Edit the board")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", action: onDone)
                }
            }
        }
    }
}
