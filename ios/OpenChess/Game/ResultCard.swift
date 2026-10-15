import SwiftUI

/// The result over the board when a game ends: the result and its reason,
/// then Analyse this game, Rematch (where it makes sense) and Done.
@MainActor
struct ResultCard: View {
    let summary: ResultSummary
    var detail: String?
    var onAnalyse: (() -> Void)?
    var onRematch: (() -> Void)?
    let onDone: () -> Void
    /// Hide the card to look at the final position.
    let onHide: () -> Void

    var body: some View {
        VStack(spacing: 10) {
            HStack {
                Spacer()
                Button {
                    onHide()
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundStyle(Color.ocMuted)
                }
                .accessibilityLabel("Show the board")
            }
            if !summary.score.isEmpty {
                Text(summary.score)
                    .font(OCFont.mono(30, weight: .bold))
                    .foregroundStyle(Color.ocAction)
            }
            Text(summary.title)
                .font(OCFont.text(20, weight: .bold))
                .multilineTextAlignment(.center)
            if let detail, !detail.isEmpty {
                Text(detail)
                    .font(OCFont.text(13))
                    .foregroundStyle(Color.ocMuted)
                    .multilineTextAlignment(.center)
            }
            if let onAnalyse {
                Button("Analyse this game", action: onAnalyse)
                    .buttonStyle(OCButtonStyle(kind: .primary))
            }
            if let onRematch {
                Button("Rematch", action: onRematch)
                    .buttonStyle(OCButtonStyle(kind: .secondary))
            }
            Button("Done", action: onDone)
                .buttonStyle(OCButtonStyle(kind: .secondary))
        }
        .padding(16)
        .frame(maxWidth: 340)
        .background(Color.ocPanel)
        .clipShape(RoundedRectangle(cornerRadius: OCMetrics.corner))
        .overlay(RoundedRectangle(cornerRadius: OCMetrics.corner).stroke(Color.ocAction, lineWidth: 2))
        .shadow(color: Color.black.opacity(0.35), radius: 12, x: 0, y: 4)
        .foregroundStyle(Color.ocInk)
        .accessibilityElement(children: .contain)
    }
}
