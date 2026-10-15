import SwiftUI

/// Training: games with hints, lines and odds from the start, from the
/// opening or a position set up by hand.
@MainActor
struct TrainView: View {
    @EnvironmentObject var model: AppModel
    @State var showComputer = false
    @State var showEditor = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Practise with help")
                        .font(OCFont.text(20, weight: .bold))
                    Text("Training games show the best move, the top five lines with their scores, the win/draw/loss odds and the opening as you play. They're never rated.")
                        .font(OCFont.text(14))
                        .foregroundStyle(Color.ocMuted)
                }
                .ocCard()

                Button {
                    showComputer = true
                } label: {
                    Label("Train against the computer", systemImage: "graduationcap")
                }
                .buttonStyle(OCButtonStyle(kind: .primary))

                Button {
                    showEditor = true
                } label: {
                    Label("Set up a position", systemImage: "square.grid.3x3")
                }
                .buttonStyle(OCButtonStyle(kind: .secondary))

                NavigationLink {
                    ScanView()
                } label: {
                    Label("Scan a board", systemImage: "camera.viewfinder")
                }
                .buttonStyle(OCButtonStyle(kind: .secondary))
            }
            .padding(16)
            .frame(maxWidth: 720)
            .frame(maxWidth: .infinity)
        }
        .ocScreen()
        .navigationTitle("Train")
        .sheet(isPresented: $showComputer) {
            NewComputerGameView(training: true)
        }
        .navigationDestination(isPresented: $showEditor) {
            ManualSetupView()
        }
    }
}

/// The board editor without a photo.
@MainActor
struct ManualSetupView: View {
    @StateObject var editor = BoardEditorModel(placement: .start)

    var body: some View {
        ScrollView {
            BoardEditorView(editor: editor, photo: nil)
                .padding(16)
                .frame(maxWidth: 720)
                .frame(maxWidth: .infinity)
        }
        .ocScreen()
        .navigationTitle("Set up a position")
        .navigationBarTitleDisplayMode(.inline)
    }
}
