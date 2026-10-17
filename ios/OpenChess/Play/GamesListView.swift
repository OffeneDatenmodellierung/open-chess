import SwiftUI

/// Saved games on this device, and online games in play.
@MainActor
struct GamesListView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var sync: SyncEngine
    @State var error: String?

    var body: some View {
        List {
            if !sync.playing.isEmpty {
                Section {
                    ForEach(sync.playing) { g in
                        NavigationLink {
                            OnlineView()
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("vs \(g.opponent.username)").font(OCFont.text(15, weight: .semibold))
                                    Text(g.daysPerTurn.map { "\($0) days per move" } ?? g.speed.capitalized)
                                        .font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
                                }
                                Spacer()
                                if g.isMyTurn {
                                    OCBadge(text: "Your move", color: .ocAction)
                                }
                            }
                        }
                    }
                } header: {
                    SectionTitle(text: "Online")
                }
                .listRowBackground(Color.ocPanel)
            }
            Section {
                if model.savedGames.isEmpty {
                    Text("Games you play are kept here, also offline.")
                        .foregroundStyle(Color.ocMuted)
                }
                ForEach(model.savedGames, id: \.id) { g in
                    Button {
                        openSaved(g)
                    } label: {
                        SavedGameRow(game: g)
                    }
                    .buttonStyle(.plain)
                }
                .onDelete { idx in
                    let ids = idx.map { model.savedGames[$0].id }
                    for id in ids {
                        model.deleteGame(id)
                    }
                }
            } header: {
                SectionTitle(text: "On this device")
            }
            .listRowBackground(Color.ocPanel)
        }
        .ocScreen()
        .navigationTitle("Games")
        // Inline: the large title left an empty band above the page.
        .navigationBarTitleDisplayMode(.inline)
        .alert("Couldn't open the game", isPresented: Binding(
            get: { error != nil },
            set: { if !$0 { error = nil } }
        )) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(error ?? "")
        }
    }

    private func openSaved(_ saved: SavedGame) {
        do {
            model.showGame(try GameViewModel(app: model, saved: saved))
        } catch {
            self.error = describe(error)
        }
    }
}

@MainActor
struct SavedGameRow: View {
    let game: SavedGame

    var body: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                Text("\(game.white) – \(game.black)")
                    .font(OCFont.text(15, weight: .semibold))
                    .lineLimit(1)
                Text(detail)
                    .font(OCFont.text(12))
                    .foregroundStyle(Color.ocMuted)
            }
            Spacer()
            Text(game.resultText)
                .font(OCFont.mono(15, weight: .semibold))
                .foregroundStyle(game.finished ? Color.ocInk : Color.ocAction)
        }
        .contentShape(Rectangle())
    }

    private var detail: String {
        var parts: [String] = []
        switch game.kind {
        case "computer": parts.append("Computer \(game.level ?? 0)")
        case "passAndPlay": parts.append("Pass and play")
        case "otb": parts.append("Real board")
        default: parts.append(game.online?.siteLabel ?? "Online")
        }
        parts.append(game.speed.capitalized)
        parts.append("\(game.moves.count) plies")
        if game.hints > 0 || game.takebacks > 0 || game.analysed {
            parts.append("unrated")
        }
        parts.append(agoText(ms: game.updatedMs))
        return parts.joined(separator: " · ")
    }
}
