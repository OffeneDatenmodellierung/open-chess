import CoreBluetooth
import SwiftUI

/// Find and connect a Chessnut board.
@MainActor
struct ChessnutView: View {
    @EnvironmentObject var chessnut: ChessnutManager

    var body: some View {
        List {
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
    }
}
