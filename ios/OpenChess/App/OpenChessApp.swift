import BackgroundTasks
import SwiftUI
import UIKit
import UserNotifications

@main
@MainActor
struct OpenChessApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject var model = AppModel()
    @Environment(\.scenePhase) var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .environmentObject(model.settings)
                .environmentObject(model.sync)
                .environmentObject(model.auth)
                .environmentObject(model.chessnut)
                .environmentObject(model.network)
                .environmentObject(model.push)
                .tint(Color.ocAction)
                .onChange(of: scenePhase) { _, phase in
                    model.scenePhaseChanged(phase)
                }
        }
        .backgroundTask(.appRefresh(SyncEngine.backgroundIdentifier)) {
            await OpenChessApp.backgroundSync()
        }
    }

    /// iOS gave the app background time: sync once and ask for the next slot.
    @MainActor
    static func backgroundSync() async {
        guard let model = AppModel.current else { return }
        SyncEngine.scheduleBackgroundRefresh(minutes: model.settings.syncMinutes)
        await model.sync.syncNow(reason: "background")
    }
}

/// Push registration callbacks.
@MainActor
final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = NotificationDelegate.shared
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        AppModel.current?.push.didRegister(token: deviceToken)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        AppModel.current?.push.didFail(error)
    }

    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification userInfo: [AnyHashable: Any],
        fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void
    ) {
        // "Your move": pull the new state.
        Task { @MainActor in
            if let engine = AppModel.current?.sync {
                await engine.syncNow(reason: "push")
                completionHandler(engine.lastError == nil ? .newData : .failed)
            } else {
                completionHandler(.noData)
            }
        }
    }
}
