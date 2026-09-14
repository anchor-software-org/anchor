import SwiftUI
import UIKit

@main
struct AnchorApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        WindowGroup {
            MainTabView()
                .preferredColorScheme(.dark)
        }
    }
}

/// App delegate for background task management.
class AppDelegate: NSObject, UIApplicationDelegate {
    private var backgroundTaskId: UIBackgroundTaskIdentifier = .invalid

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        // Keep screen on when app is active (useful during streaming)
        UIApplication.shared.isIdleTimerDisabled = true
        return true
    }

    func applicationDidEnterBackground(_ application: UIApplication) {
        // Request background time to keep the TCP connection alive
        backgroundTaskId = application.beginBackgroundTask(withName: "anchor.keepalive") {
            // Cleanup when time expires
            application.endBackgroundTask(self.backgroundTaskId)
            self.backgroundTaskId = .invalid
        }
        NSLog("[anchor] Background task started (remaining: %.0fs)", application.backgroundTimeRemaining)
    }

    func applicationWillEnterForeground(_ application: UIApplication) {
        // End background task
        if backgroundTaskId != .invalid {
            application.endBackgroundTask(backgroundTaskId)
            backgroundTaskId = .invalid
        }
        NSLog("[anchor] Returned to foreground")
    }
}
