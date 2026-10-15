import Foundation
import UIKit
import UserNotifications

/// "Tell me when it's my move": notification permission, APNs
/// registration, and the device token on the server.
@MainActor
final class PushManager: ObservableObject {
    @Published private(set) var status: String = ""

    private let settings: AppSettings
    private let auth: AuthManager

    init(settings: AppSettings, auth: AuthManager) {
        self.settings = settings
        self.auth = auth
    }

    /// Whether this build talks to the APNs sandbox (debug builds).
    private var sandbox: Bool {
        #if DEBUG
        return true
        #else
        return false
        #endif
    }

    func setEnabled(_ on: Bool) {
        settings.notifyMyMove = on
        if on {
            Task { await self.requestAndRegister() }
        } else {
            Task { await self.unregister() }
        }
    }

    /// On launch and coming back to the foreground: keep the token fresh.
    func refreshRegistration() {
        guard settings.notifyMyMove else { return }
        UIApplication.shared.registerForRemoteNotifications()
    }

    private func requestAndRegister() async {
        do {
            let granted = try await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .sound, .badge])
            if granted {
                UIApplication.shared.registerForRemoteNotifications()
                status = "Asking iOS for a push token…"
            } else {
                status = "Notifications are off for open-chess in Settings."
                settings.notifyMyMove = false
            }
        } catch {
            status = describe(error)
            settings.notifyMyMove = false
        }
    }

    func didRegister(token: Data) {
        let hex = token.hexString
        guard auth.signedIn else {
            status = "Sign in to get move notifications."
            return
        }
        Task {
            do {
                try await auth.server.postJSON("/api/push/devices", object: ["token": hex, "sandbox": sandbox])
                settings.pushToken = hex
                status = "Notifications on."
            } catch {
                status = "Couldn't register for notifications: \(describe(error))"
            }
        }
    }

    func didFail(_ error: Error) {
        // Without an Apple team (and the aps-environment entitlement) this
        // always fails; say so plainly.
        status = "Push isn't available in this build: \(error.localizedDescription)"
    }

    private func unregister() async {
        UIApplication.shared.unregisterForRemoteNotifications()
        let token = settings.pushToken
        guard !token.isEmpty, auth.signedIn else { return }
        do {
            try await auth.server.delete("/api/push/devices/\(token)")
            settings.pushToken = ""
            status = "Notifications off."
        } catch {
            status = describe(error)
        }
    }
}

/// Show "your move" banners while the app is open too.
final class NotificationDelegate: NSObject, UNUserNotificationCenterDelegate {
    static let shared = NotificationDelegate()

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }
}
