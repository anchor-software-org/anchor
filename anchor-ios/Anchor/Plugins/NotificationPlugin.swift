import Combine
import Foundation
import UIKit
import UserNotifications

struct NotificationHistory {
    private(set) var entries: [DesktopNotification] = []
    let capacity: Int

    init(capacity: Int = 100) {
        self.capacity = max(1, capacity)
    }

    mutating func insert(_ notification: DesktopNotification) {
        entries.removeAll { $0.id == notification.id }
        entries.insert(notification, at: 0)
        if entries.count > capacity {
            entries.removeLast(entries.count - capacity)
        }
    }

    mutating func clear() {
        entries.removeAll(keepingCapacity: true)
    }
}

final class NotificationPlugin: Plugin, ObservableObject {
    let pluginId = "notifications"

    @Published private(set) var notifications: [DesktopNotification] = []
    @Published private(set) var authorizationStatus: UNAuthorizationStatus = .notDetermined
    @Published private(set) var isAvailable = false

    private let broker: MessageBroker
    private let notificationCenter: UNUserNotificationCenter
    private var history = NotificationHistory()
    private var cancellables = Set<AnyCancellable>()

    init(
        broker: MessageBroker,
        notificationCenter: UNUserNotificationCenter = .current()
    ) {
        self.broker = broker
        self.notificationCenter = notificationCenter
    }

    func start() {
        broker.events
            .receive(on: DispatchQueue.main)
            .filter { $0.target == .service("notifications") }
            .sink { [weak self] event in
                guard case .notification(let message) = event.message else { return }
                self?.handle(message)
            }
            .store(in: &cancellables)
        refreshAuthorizationStatus()
    }

    func stop() {
        cancellables.removeAll()
        isAvailable = false
    }

    func requestAuthorization() {
        notificationCenter.requestAuthorization(options: [.alert, .badge, .sound]) { [weak self] _, _ in
            self?.refreshAuthorizationStatus()
        }
    }

    func refreshAuthorizationStatus() {
        notificationCenter.getNotificationSettings { [weak self] settings in
            DispatchQueue.main.async {
                self?.authorizationStatus = settings.authorizationStatus
            }
        }
    }

    func openSystemSettings() {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
    }

    func clearHistory() {
        history.clear()
        notifications = history.entries
    }

    private func handle(_ message: NotificationWireMessage) {
        switch message {
        case .availability(let available):
            isAvailable = available
        case .posted(let notification):
            isAvailable = true
            history.insert(notification)
            notifications = history.entries
            postSystemNotification(notification)
        }
    }

    private func postSystemNotification(_ notification: DesktopNotification) {
        guard authorizationStatus == .authorized || authorizationStatus == .provisional else { return }
        let content = UNMutableNotificationContent()
        content.title = notification.title.isEmpty ? notification.applicationName : notification.title
        if !notification.title.isEmpty {
            content.subtitle = notification.applicationName
        }
        content.body = notification.body
        content.sound = .default
        let request = UNNotificationRequest(
            identifier: "anchor.desktop.\(notification.id)",
            content: content,
            trigger: nil
        )
        notificationCenter.add(request) { error in
            if let error {
                NSLog("[anchor.notifications] Could not post desktop notification: \(error)")
            }
        }
    }
}
