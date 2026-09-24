import SwiftUI
import UserNotifications

struct NotificationsView: View {
    @ObservedObject var notificationPlugin: NotificationPlugin

    var body: some View {
        VStack(spacing: 0) {
            permissionNotice

            if notificationPlugin.notifications.isEmpty {
                Spacer()
                VStack(spacing: 10) {
                    Image("MaterialNotifications")
                        .renderingMode(.template)
                        .resizable()
                        .scaledToFit()
                        .frame(width: 42, height: 42)
                        .foregroundStyle(Color.anchorGray.opacity(0.55))
                    Text("No desktop notifications yet")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(Color.offWhite)
                    Text("New desktop notifications will appear here.")
                        .font(.system(size: 12))
                        .foregroundStyle(Color.anchorGray)
                }
                Spacer()
            } else {
                ScrollView {
                    LazyVStack(spacing: 1) {
                        ForEach(notificationPlugin.notifications) { notification in
                            NotificationRow(notification: notification)
                        }
                    }
                    .padding(.vertical, 8)
                }
            }
        }
        .background(Color.charcoalBlack)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if !notificationPlugin.notifications.isEmpty {
                HStack {
                    Spacer()
                    Button("Clear") { notificationPlugin.clearHistory() }
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(Color.anchorGray)
                }
                .padding(.horizontal, 16)
                .frame(height: 44)
                .background(Color.charcoalBlack)
                .overlay(alignment: .top) {
                    Rectangle().fill(Color.white.opacity(0.08)).frame(height: 1)
                }
            }
        }
        .onAppear { notificationPlugin.refreshAuthorizationStatus() }
    }

    @ViewBuilder
    private var permissionNotice: some View {
        switch notificationPlugin.authorizationStatus {
        case .notDetermined:
            notice(
                text: "Allow alerts to show desktop notifications outside Anchor.",
                button: "Allow",
                action: notificationPlugin.requestAuthorization
            )
        case .denied:
            notice(
                text: "Alerts are off. Notifications will still appear in this list.",
                button: "Settings",
                action: notificationPlugin.openSystemSettings
            )
        default:
            EmptyView()
        }
    }

    private func notice(text: String, button: String, action: @escaping () -> Void) -> some View {
        HStack(spacing: 12) {
            Text(text)
                .font(.system(size: 12))
                .foregroundStyle(Color.anchorGray)
            Spacer(minLength: 8)
            Button(button, action: action)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Color.offWhite)
        }
        .padding(.horizontal, 16)
        .frame(minHeight: 48)
        .background(Color.darkGray.opacity(0.35))
        .overlay(alignment: .bottom) {
            Rectangle().fill(Color.white.opacity(0.08)).frame(height: 1)
        }
    }
}

private struct NotificationRow: View {
    let notification: DesktopNotification

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image("MaterialNotifications")
                .renderingMode(.template)
                .resizable()
                .scaledToFit()
                .frame(width: 17, height: 17)
                .foregroundStyle(Color.anchorGray)
                .frame(width: 24, height: 24)

            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline) {
                    Text(notification.applicationName)
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(Color.anchorGray)
                        .lineLimit(1)
                    Spacer()
                    Text(notification.postedAt, style: .time)
                        .font(.system(size: 10))
                        .foregroundStyle(Color.mediumGray)
                }
                if !notification.title.isEmpty {
                    Text(notification.title)
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(Color.offWhite)
                }
                if !notification.body.isEmpty {
                    Text(notification.body)
                        .font(.system(size: 13))
                        .foregroundStyle(Color.offWhite.opacity(0.78))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .background(Color.darkGray.opacity(0.24))
    }
}
