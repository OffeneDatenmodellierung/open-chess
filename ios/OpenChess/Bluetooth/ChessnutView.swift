import CoreBluetooth
import SwiftUI
import UIKit

/// Find and connect a Chessnut board; once it's connected, start a game
/// on it from here. `returnOnConnect`: opened from a game's setup, so go
/// back to it as soon as the board connects.
@MainActor
struct ChessnutView: View {
    var returnOnConnect = false

    @EnvironmentObject var chessnut: ChessnutManager
    @Environment(\.dismiss) private var dismiss
    @State private var showComputer = false
    @State private var showRealBoard = false

    var body: some View {
        List {
            if chessnut.isConnected && !returnOnConnect {
                Section {
                    Button {
                        showComputer = true
                    } label: {
                        Label("Play the computer", systemImage: "cpu")
                            .font(OCFont.text(17, weight: .bold))
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(OCButtonStyle(kind: .primary))
                    .listRowInsets(EdgeInsets(top: 8, leading: 12, bottom: 4, trailing: 12))
                    Button {
                        showRealBoard = true
                    } label: {
                        Label("Play someone on this board", systemImage: "person.2")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(OCButtonStyle(kind: .secondary))
                    .listRowInsets(EdgeInsets(top: 4, leading: 12, bottom: 8, trailing: 12))
                } header: {
                    SectionTitle(text: "The board is ready")
                } footer: {
                    Text("Set the pieces up and start a game: your moves on the board play on screen, and the reply lights up on the board. Playing someone records the game on your account, with photos or a stream of the board if you like.")
                }
                .listRowBackground(Color.clear)
            }

            Section {
                HStack {
                    Text("Status")
                    Spacer()
                    Text(chessnut.stateText).foregroundStyle(Color.ocMuted)
                }
                if let b = chessnut.battery {
                    HStack {
                        Text("Battery")
                        Spacer()
                        Text("\(b.percent)%\(b.charging ? " · charging" : "")")
                            .font(OCFont.mono(15))
                            .foregroundStyle(Color.ocMuted)
                    }
                }
                if chessnut.isConnected {
                    Button("Disconnect", role: .destructive) { chessnut.disconnect() }
                } else if chessnut.state == .scanning {
                    Button("Stop looking") { chessnut.stopScan() }
                } else {
                    Button("Look for boards") { chessnut.startScan() }
                }
            } header: {
                SectionTitle(text: "Chessnut board")
            } footer: {
                Text("Turn the board on and keep it near. Once connected, moves made on the board play in the game on screen, and the computer's or your opponent's reply lights up on the board until you make it.")
            }
            .listRowBackground(Color.ocPanel)

            if !chessnut.isConnected && !chessnut.found.isEmpty {
                Section {
                    ForEach(chessnut.found, id: \.identifier) { p in
                        Button(p.name ?? "Chessnut board") { chessnut.connect(p) }
                    }
                } header: {
                    SectionTitle(text: "Found")
                }
                .listRowBackground(Color.ocPanel)
            }
        }
        .ocScreen()
        .navigationTitle("Chessnut Go")
        .navigationBarTitleDisplayMode(.inline)
        .onDisappear {
            if chessnut.state == .scanning { chessnut.stopScan() }
        }
        .onChange(of: chessnut.isConnected) { _, connected in
            if connected {
                UINotificationFeedbackGenerator().notificationOccurred(.success)
                if returnOnConnect { dismiss() }
            }
        }
        .sheet(isPresented: $showComputer) {
            NewComputerGameView()
                .ocAppearance()
        }
        .sheet(isPresented: $showRealBoard) {
            LiveBoardSetupView()
                .ocAppearance()
        }
    }
}
