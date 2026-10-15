import OpenChessCore
import SwiftUI

/// The game: board, players and clocks, the W/D/L bar, the moves, hints
/// and the buttons. Board left and panel right when there's room (iPad,
/// landscape).
@MainActor
struct GameScreen: View {
    @ObservedObject var game: GameViewModel
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var chessnut: ChessnutManager
    @Environment(\.horizontalSizeClass) var hSize
    @State var confirmResign = false
    @State var confirmDraw = false

    var body: some View {
        NavigationStack {
            GeometryReader { geo in
                let wide = hSize == .regular && geo.size.width > geo.size.height * 1.1
                Group {
                    if wide {
                        HStack(alignment: .top, spacing: 16) {
                            boardColumn
                                .frame(width: min(geo.size.height - 32, geo.size.width * 0.6))
                            ScrollView { panel }
                        }
                        .padding(16)
                    } else {
                        ScrollView {
                            VStack(spacing: 12) {
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
            }
            .ocScreen()
            .navigationTitle(game.title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") {
                        game.detach()
                        model.activeGame = nil
                    }
                }
                ToolbarItem(placement: .primaryAction) {
                    if chessnut.isConnected {
                        Image(systemName: "checkerboard.rectangle")
                            .foregroundStyle(Color.ocAction)
                            .accessibilityLabel("Chessnut board connected")
                    }
                }
            }
            .overlay {
                if game.pendingPromotion != nil {
                    ZStack {
                        Color.black.opacity(0.35).ignoresSafeArea()
                        PromotionPicker(
                            white: game.whiteToMove,
                            onPick: { game.promote($0) },
                            onCancel: { game.cancelPromotion() }
                        )
                    }
                }
            }
            .alert("Resign this game?", isPresented: $confirmResign) {
                Button("Resign", role: .destructive) { game.resign() }
                Button("Cancel", role: .cancel) {}
            }
            .alert("Agree a draw?", isPresented: $confirmDraw) {
                Button("Draw", role: .destructive) { game.agreeDraw() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Both players agree the game is drawn.")
            }
            .alert("open-chess", isPresented: Binding(
                get: { game.message != nil },
                set: { if !$0 { game.message = nil } }
            )) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(game.message ?? "")
            }
            .onAppear {
                game.attachBoard()
                game.startOnline()
            }
        }
    }

    // MARK: Board column

    private var boardColumn: some View {
        VStack(spacing: 8) {
            PlayerRow(
                name: game.flipped ? game.whiteName : game.blackName,
                white: game.flipped,
                clock: game.clockText(white: game.flipped),
                active: !game.isFinished && game.whiteToMove == game.flipped
            )
            BoardView(
                placement: game.placement,
                flipped: game.flipped,
                selected: game.selected,
                lastMove: game.lastMove,
                hint: game.hintsOn ? game.hintSquares : nil,
                targets: game.targets,
                onTap: { game.tap($0) }
            )
            PlayerRow(
                name: game.flipped ? game.blackName : game.whiteName,
                white: !game.flipped,
                clock: game.clockText(white: !game.flipped),
                active: !game.isFinished && game.whiteToMove != game.flipped
            )
        }
    }

    // MARK: Panel

    private var panel: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Text(game.statusLine)
                    .font(OCFont.text(15, weight: .semibold))
                Spacer()
                if game.usedHelp || game.hintsOn {
                    OCBadge(text: "Hints on · unrated")
                }
                if let link = game.onlineLink {
                    OCBadge(text: link.rated ? "Rated" : "Casual", color: .ocMuted)
                }
            }
            if let wdl = game.analysis?.wdl, game.hintsOn {
                WDLBar(white: wdl.white, draw: wdl.draw, black: wdl.black)
            }
            if let opening = game.state.opening {
                Text([game.state.eco, opening].compactMap { $0 }.joined(separator: " · "))
                    .font(OCFont.text(13))
                    .foregroundStyle(Color.ocMuted)
            }
            MoveListView(san: game.state.san, startsWhite: startsWhite)
            if game.hintsOn {
                HintPanel(game: game)
            }
            buttons
            if game.isFinished, let note = game.finishedNote {
                finishedCard(note)
            }
            if !chessnut.lastEvent.isEmpty && chessnut.isConnected {
                Text(chessnut.lastEvent)
                    .font(OCFont.text(12))
                    .foregroundStyle(Color.ocMuted)
            }
        }
    }

    private var startsWhite: Bool {
        game.state.initialFen.split(separator: " ").dropFirst().first.map(String.init) != "b"
    }

    private var buttons: some View {
        let columns = [GridItem(.adaptive(minimum: 100), spacing: 8)]
        return LazyVGrid(columns: columns, spacing: 8) {
            if game.helpAllowed {
                Button(game.hintsOn ? "Hide hints" : "Hint") {
                    if game.hintsOn { game.hideHints() } else { game.hint() }
                }
                .disabled(game.isFinished)
            }
            if game.onlineLink == nil {
                Button("Take back") { game.takeBack() }
                    .disabled(!game.takebackAllowed)
            }
            Button("Flip") { game.flip() }
            if game.isPassAndPlay {
                Button("Offer draw") { confirmDraw = true }
                    .disabled(game.isFinished)
            }
            if game.onlineLink != nil {
                Button("Refresh") { game.refreshOnline() }
            }
            Button("Resign") { confirmResign = true }
                .disabled(game.isFinished)
        }
        .buttonStyle(OCButtonStyle(kind: .secondary, compact: true))
    }

    private func finishedCard(_ note: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(note).font(OCFont.text(17, weight: .bold))
            Text(game.usedHelp ? "Help was used, so this game is unrated." : "Saved. It goes to the server with the next sync.")
                .font(OCFont.text(13))
                .foregroundStyle(Color.ocMuted)
            Button("Close") {
                game.detach()
                model.activeGame = nil
            }
            .buttonStyle(OCButtonStyle(kind: .primary))
        }
        .ocCard()
    }
}

@MainActor
struct PlayerRow: View {
    let name: String
    let white: Bool
    let clock: String
    let active: Bool

    var body: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(white ? Color.ocWSeg : Color.ocBSeg)
                .overlay(Circle().stroke(Color.ocRule, lineWidth: 1))
                .frame(width: 14, height: 14)
            Text(name.isEmpty ? (white ? "White" : "Black") : name)
                .font(OCFont.text(15, weight: .semibold))
                .lineLimit(1)
            Spacer()
            if !clock.isEmpty {
                Text(clock)
                    .font(OCFont.mono(17, weight: .semibold))
                    .padding(.horizontal, 10)
                    .padding(.vertical, 4)
                    .foregroundStyle(active ? Color.ocActionInk : Color.ocInk)
                    .background(active ? Color.ocAction : Color.ocPanel)
                    .clipShape(RoundedRectangle(cornerRadius: OCMetrics.corner))
                    .overlay(RoundedRectangle(cornerRadius: OCMetrics.corner).stroke(Color.ocRule, lineWidth: 1))
            }
        }
    }
}

/// The moves in SAN, numbered.
@MainActor
struct MoveListView: View {
    let san: [String]
    let startsWhite: Bool

    private struct Row: Identifiable {
        let id: Int
        let number: Int
        let white: String?
        let black: String?
    }

    private var rows: [Row] {
        var out: [Row] = []
        var moves = san
        var n = 1
        if !startsWhite, !moves.isEmpty {
            out.append(Row(id: 0, number: 1, white: "…", black: moves.removeFirst()))
            n = 2
        }
        var i = 0
        while i < moves.count {
            let w = moves[i]
            let b = i + 1 < moves.count ? moves[i + 1] : nil
            out.append(Row(id: out.count, number: n, white: w, black: b))
            n += 1
            i += 2
        }
        return out
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionTitle(text: "Moves")
                .padding(.bottom, 6)
            if san.isEmpty {
                Text("No moves yet.")
                    .font(OCFont.text(14))
                    .foregroundStyle(Color.ocMuted)
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 2) {
                            ForEach(rows) { r in
                                HStack(spacing: 0) {
                                    Text("\(r.number).")
                                        .font(OCFont.mono(14))
                                        .foregroundStyle(Color.ocMuted)
                                        .frame(width: 40, alignment: .leading)
                                    Text(r.white ?? "")
                                        .font(OCFont.mono(15, weight: .medium))
                                        .frame(width: 80, alignment: .leading)
                                    Text(r.black ?? "")
                                        .font(OCFont.mono(15, weight: .medium))
                                        .frame(width: 80, alignment: .leading)
                                    Spacer(minLength: 0)
                                }
                                .id(r.id)
                            }
                        }
                    }
                    .frame(maxHeight: 180)
                    .onChange(of: san.count) { _, _ in
                        if let last = rows.last { proxy.scrollTo(last.id, anchor: .bottom) }
                    }
                }
            }
        }
        .ocCard(padding: 12)
    }
}

/// Best move, the top lines with their scores, and the opening.
@MainActor
struct HintPanel: View {
    @ObservedObject var game: GameViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                SectionTitle(text: "Hint")
                Spacer()
                if game.analysing {
                    ProgressView().controlSize(.small)
                }
            }
            if let best = game.hintMove {
                HStack(spacing: 6) {
                    Text("Best move").font(OCFont.text(14)).foregroundStyle(Color.ocMuted)
                    Text(best.san).font(OCFont.mono(18, weight: .bold)).foregroundStyle(Color.ocAction)
                }
            } else if !game.myTurn && !game.isFinished {
                Text("Hints show on your move.").font(OCFont.text(14)).foregroundStyle(Color.ocMuted)
            }
            if let a = game.analysis {
                if let opening = a.opening, !opening.name.isEmpty {
                    Text("\(opening.eco) · \(opening.name)")
                        .font(OCFont.text(13))
                        .foregroundStyle(Color.ocMuted)
                }
                ForEach(a.lines.prefix(5)) { line in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(line.score?.text ?? "")
                            .font(OCFont.mono(13, weight: .semibold))
                            .frame(width: 58, alignment: .leading)
                        Text(line.sanText)
                            .font(OCFont.mono(13))
                            .lineLimit(2)
                            .foregroundStyle(Color.ocInk)
                    }
                }
                if let first = a.lines.first?.moves.first, !first.text.isEmpty {
                    Text(first.text)
                        .font(OCFont.text(13))
                        .foregroundStyle(Color.ocMuted)
                }
                if let p = a.lines.first?.moves.first?.patterns.first {
                    Text("\(p.name): \(p.detail)")
                        .font(OCFont.text(13))
                        .foregroundStyle(Color.ocAmber)
                }
            }
        }
        .ocCard(padding: 12)
    }
}
