import OpenChessCore
import SwiftUI

/// Home: ratings, play the computer, pass and play, online, and tiles for
/// scanning a board, the Chessnut board and training.
@MainActor
struct PlayHomeView: View {
    @Binding var tab: AppTab
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var sync: SyncEngine
    @EnvironmentObject var chessnut: ChessnutManager
    @EnvironmentObject var auth: AuthManager
    @State var showComputer = false
    @State var showPassAndPlay = false
    @State var showRealBoard = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                SyncPill()
                if !auth.signedIn {
                    SignInPrompt(message: "Sign in to sync and rate your games, play online and record games on a real board.")
                }
                RatingsCard()
                Button {
                    showComputer = true
                } label: {
                    Label("Play the computer", systemImage: "cpu")
                        .font(OCFont.text(18, weight: .bold))
                        .padding(.vertical, 6)
                }
                .buttonStyle(OCButtonStyle(kind: .primary))

                HStack(spacing: 10) {
                    Button {
                        showPassAndPlay = true
                    } label: {
                        Label("Pass and play", systemImage: "person.2")
                    }
                    NavigationLink {
                        OnlineView()
                    } label: {
                        Label("Play online", systemImage: "globe")
                    }
                }
                .buttonStyle(OCButtonStyle(kind: .secondary))

                Button {
                    showRealBoard = true
                } label: {
                    Label("Play on a real board", systemImage: "video")
                }
                .buttonStyle(OCButtonStyle(kind: .secondary))

                if !sync.confirmations.isEmpty || !sync.playing.filter({ $0.isMyTurn }).isEmpty {
                    YourTurnCard()
                }

                LazyVGrid(columns: [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)], spacing: 10) {
                    NavigationLink {
                        ScanView()
                    } label: {
                        Tile(icon: "camera.viewfinder", title: "Scan a board", detail: "Photo to position")
                    }
                    NavigationLink {
                        ChessnutView()
                    } label: {
                        Tile(icon: "checkerboard.rectangle", title: "Chessnut Go", detail: chessnut.stateText)
                    }
                    Button {
                        tab = .train
                    } label: {
                        Tile(icon: "graduationcap", title: "Training", detail: "Hints, lines and odds")
                    }
                    NavigationLink {
                        SyncSettingsView()
                    } label: {
                        Tile(icon: "arrow.triangle.2.circlepath", title: "Sync and data", detail: sync.lowData ? "Low data" : "Normal")
                    }
                }
                .buttonStyle(.plain)
            }
            .padding(16)
            .frame(maxWidth: 720)
            .frame(maxWidth: .infinity)
        }
        .ocScreen()
        .navigationTitle("open-chess")
        // Inline: the large title left an empty band above the page.
        .navigationBarTitleDisplayMode(.inline)
        .sheet(isPresented: $showComputer) {
            NewComputerGameView()
                .ocAppearance()
        }
        .sheet(isPresented: $showPassAndPlay) {
            NewPassAndPlayView()
                .ocAppearance()
        }
        .sheet(isPresented: $showRealBoard) {
            LiveBoardSetupView()
                .ocAppearance()
        }
        .refreshable {
            await sync.syncNow(reason: "pull")
        }
    }
}

@MainActor
struct Tile: View {
    let icon: String
    let title: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Image(systemName: icon)
                .font(.system(size: 22, weight: .semibold))
                .foregroundStyle(Color.ocAction)
            Text(title).font(OCFont.text(15, weight: .semibold)).foregroundStyle(Color.ocInk)
            Text(detail).font(OCFont.text(12)).foregroundStyle(Color.ocMuted).lineLimit(2)
        }
        .frame(maxWidth: .infinity, minHeight: 92, alignment: .topLeading)
        .ocCard(padding: 12)
    }
}

/// "Synced 4 min ago · Low data"; tap to sync now.
@MainActor
struct SyncPill: View {
    @EnvironmentObject var sync: SyncEngine

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: sync.lastError == nil ? "checkmark.circle" : "exclamationmark.triangle")
                .foregroundStyle(sync.lastError == nil ? Color.ocMuted : Color.ocDanger)
            Text(sync.pillText)
                .font(OCFont.text(13, weight: .medium))
                .foregroundStyle(Color.ocMuted)
            Spacer(minLength: 0)
            if sync.syncing {
                ProgressView().controlSize(.mini)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .background(Color.ocPanel)
        .clipShape(Capsule())
        .overlay(Capsule().stroke(Color.ocRule, lineWidth: 1))
        .onTapGesture {
            Task { await sync.syncNow(reason: "pill") }
        }
        .accessibilityAddTraits(.isButton)
        .accessibilityHint("Sync now")
    }
}

/// A rating per speed; provisional ones carry an amber "?".
@MainActor
struct RatingsCard: View {
    @EnvironmentObject var model: AppModel
    @State private var showNote = false
    private let speeds = ["bullet", "blitz", "rapid", "classical", "correspondence"]

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                SectionTitle(text: "Ratings")
                Spacer()
                Image(systemName: showNote ? "info.circle.fill" : "info.circle")
                    .foregroundStyle(Color.ocMuted)
                    .accessibilityHidden(true)
            }
            HStack(spacing: 0) {
                ForEach(speeds, id: \.self) { speed in
                    let r = model.ratings.first { $0.speed == speed }
                    let value = r?.value ?? newRating()
                    VStack(spacing: 2) {
                        HStack(spacing: 0) {
                            Text("\(Int(value.rating.rounded()))")
                                .font(OCFont.mono(18, weight: .bold))
                                .foregroundStyle(r == nil ? Color.ocMuted : Color.ocInk)
                            if isProvisional(rating: value) {
                                Text("?")
                                    .font(OCFont.mono(18, weight: .bold))
                                    .foregroundStyle(Color.ocAmber)
                            }
                        }
                        Text(label(speed))
                            .font(OCFont.text(11))
                            .foregroundStyle(Color.ocMuted)
                        if r?.local == true {
                            Text("on device")
                                .font(OCFont.text(9))
                                .foregroundStyle(Color.ocAmber)
                        }
                    }
                    .frame(maxWidth: .infinity)
                }
            }
            if showNote {
                Text("Everyone starts at 1,500 in each speed. A ? means the rating is still settling: it moves a lot at first and the ? goes after about a dozen rated games at that speed. Games against the computer count when you play without hints, takebacks or analysis.")
                    .font(OCFont.text(13))
                    .foregroundStyle(Color.ocMuted)
                    .fixedSize(horizontal: false, vertical: true)
                    .transition(.opacity)
            }
        }
        .ocCard()
        .contentShape(Rectangle())
        .onTapGesture {
            withAnimation(.easeInOut(duration: 0.2)) { showNote.toggle() }
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityHint(showNote ? "Hides what the ratings mean." : "Shows what the ratings and the question marks mean.")
    }

    private func label(_ speed: String) -> String {
        speed == "correspondence" ? "Corresp." : speed.capitalized
    }
}

/// Games waiting on me, and offline games to confirm.
@MainActor
struct YourTurnCard: View {
    @EnvironmentObject var sync: SyncEngine

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionTitle(text: "Waiting for you")
            ForEach(sync.playing.filter { $0.isMyTurn }) { g in
                NavigationLink {
                    OnlineView()
                } label: {
                    HStack {
                        Text("Your move").font(OCFont.text(14, weight: .bold)).foregroundStyle(Color.ocAction)
                        Text("vs \(g.opponent.username)").font(OCFont.text(14)).foregroundStyle(Color.ocInk)
                        Spacer()
                        Image(systemName: "chevron.right").foregroundStyle(Color.ocMuted)
                    }
                }
            }
            ForEach(sync.confirmations) { c in
                ConfirmationRow(confirmation: c)
            }
        }
        .ocCard()
    }
}

@MainActor
struct ConfirmationRow: View {
    let confirmation: Confirmation
    @EnvironmentObject var sync: SyncEngine

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("\(confirmation.white) – \(confirmation.black)  \(confirmation.resultText)")
                .font(OCFont.text(14, weight: .semibold))
            Text("\(confirmation.uploadedBy) says you played this \(confirmation.speed) game. Was it you?")
                .font(OCFont.text(13))
                .foregroundStyle(Color.ocMuted)
            HStack {
                Button("Yes, that was me") { sync.confirm(confirmation, accept: true) }
                    .buttonStyle(OCButtonStyle(kind: .primary, compact: true))
                Button("No") { sync.confirm(confirmation, accept: false) }
                    .buttonStyle(OCButtonStyle(kind: .secondary, compact: true))
            }
        }
    }
}
