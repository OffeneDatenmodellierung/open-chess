import AudioToolbox
import AVFoundation
import UIKit

/// Haptics, short sounds, spoken moves and VoiceOver announcements, each
/// following its setting (Sounds, Haptics, Speak moves).
@MainActor
enum Feedback {
    /// Set by `AppModel` at launch.
    static var settings: AppSettings?
    private static let speech = AVSpeechSynthesizer()

    private static var haptics: Bool { settings?.haptics ?? true }
    private static var sounds: Bool { settings?.sounds ?? true }
    private static var speaks: Bool { settings?.speakMoves ?? false }

    /// An illegal tap: a light tap.
    static func illegal() {
        guard haptics else { return }
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
    }

    /// The side to move is in check.
    static func check() {
        if haptics {
            UINotificationFeedbackGenerator().notificationOccurred(.warning)
        }
        if sounds {
            // A short system tick.
            AudioServicesPlaySystemSound(1103)
        }
    }

    /// Something to put right on a real board.
    static func warning() {
        guard haptics else { return }
        UINotificationFeedbackGenerator().notificationOccurred(.warning)
    }

    /// The game is over: success unless the player on this device lost.
    static func gameOver(lost: Bool, title: String) {
        if haptics {
            UINotificationFeedbackGenerator().notificationOccurred(lost ? .error : .success)
        }
        if sounds {
            AudioServicesPlaySystemSound(1057)
        }
        announce(title)
    }

    /// Read out for VoiceOver users.
    static func announce(_ text: String) {
        UIAccessibility.post(notification: .announcement, argument: text)
    }

    /// Speak a move (SAN) in plain English, when Speak moves is on.
    static func speakMove(san: String) {
        say(GameText.spoken(san: san))
    }

    /// Speak a sentence, when Speak moves is on.
    static func say(_ text: String) {
        guard speaks, !text.isEmpty else { return }
        let utterance = AVSpeechUtterance(string: text)
        utterance.rate = AVSpeechUtteranceDefaultSpeechRate
        speech.speak(utterance)
    }
}
