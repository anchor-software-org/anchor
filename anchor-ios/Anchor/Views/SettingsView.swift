import SwiftUI

struct SettingsView: View {
    @ObservedObject var viewModel: MainViewModel
    @State private var showShareSheet = false
    @State private var exportURL: URL?

    private var statusColor: Color {
        switch viewModel.connectionState.status {
        case .connected: return .seaGreen40
        case .connecting: return .anchorBlue40
        case .disconnected: return .coralRed40
        }
    }

    private var statusText: String {
        switch viewModel.connectionState.status {
        case .connected: return "Connected"
        case .connecting: return "Connecting..."
        case .disconnected: return "Disconnected"
        }
    }

    var body: some View {
        List {
            connectionStatusSection
            quickActionsSection
            pairedDevicesSection
            clipboardSection
            inputSection
            aboutSection
            exportLogsSection
        }
        .scrollContentBackground(.hidden)
        .background(Color.charcoalBlack)
        .sheet(isPresented: $showShareSheet) {
            if let url = exportURL {
                ShareSheet(items: [url])
            }
        }
    }

    private var connectionStatusSection: some View {
        Section("Connection Status") {
            HStack {
                Circle().fill(statusColor).frame(width: 10, height: 10)
                Text(statusText)
            }
            SettingsRow(label: "Host", value: viewModel.connectionState.host.isEmpty ? "--" : viewModel.connectionState.host)
            SettingsRow(label: "Port", value: viewModel.connectionState.port > 0 ? "\(viewModel.connectionState.port)" : "--")
            if let error = viewModel.connectionState.error {
                Text(error).font(.caption).foregroundColor(.red)
            }
        }
    }

    private var quickActionsSection: some View {
        Section("Quick Actions") {
            if viewModel.connectionState.status == .disconnected {
                Button("Connect") { viewModel.connectToDesktop() }
            } else if viewModel.connectionState.status == .connected {
                Button("Disconnect", role: .destructive) { viewModel.disconnect() }
            }
        }
    }

    private var pairedDevicesSection: some View {
        Section("Paired Desktops") {
            if viewModel.pairedDevices.isEmpty {
                Text("No paired desktops.")
                    .foregroundColor(.secondary)
            } else {
                ForEach(viewModel.pairedDevices) { device in
                    HStack {
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(spacing: 6) {
                                Circle()
                                    .fill(device.isOnline ? Color.seaGreen40 : Color.gray)
                                    .frame(width: 8, height: 8)
                                Text(device.deviceName)
                            }
                            Text("ID: \(String(device.deviceId.prefix(8)))...")
                                .font(.caption)
                                .foregroundColor(.secondary)
                        }
                        Spacer()
                        Button("Unpair", role: .destructive) {
                            viewModel.unpairDevice(device.deviceId)
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                }
            }
        }
    }

    private var clipboardSection: some View {
        Section("Clipboard") {
            Toggle("Auto-sync to desktop", isOn: $viewModel.autoSyncClipboard)
            Text("Automatically send copied text and images to the desktop. Desktop → iPad always works.")
                .font(.caption)
                .foregroundColor(.secondary)
        }
    }

    private var inputSection: some View {
        Section("Input") {
            Toggle("Touch input on stream", isOn: $viewModel.touchInputOnStream)
            Text("Tap the fullscreen video to control the desktop pointer.")
                .font(.caption)
                .foregroundColor(.secondary)

            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text("Touchpad sensitivity")
                    Spacer()
                    Text(String(format: "%.1fx", viewModel.touchpadSensitivity))
                        .foregroundColor(.secondary)
                }
                Slider(value: $viewModel.touchpadSensitivity, in: 0.5...4.0)
            }
        }
    }

    private var aboutSection: some View {
        Section("About") {
            SettingsRow(label: "Version", value: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "1.0")
            SettingsRow(label: "Build", value: Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "1")
            SettingsRow(label: "Protocol", value: "1")
        }
    }

    private var exportLogsSection: some View {
        Section {
            Button("Export Logs") {
                if let url = viewModel.exportLogsURL() {
                    exportURL = url
                    showShareSheet = true
                }
            }
        }
    }
}

private struct SettingsRow: View {
    let label: String
    let value: String

    var body: some View {
        HStack {
            Text(label).foregroundColor(.secondary)
            Spacer()
            Text(value)
        }
    }
}

struct ShareSheet: UIViewControllerRepresentable {
    let items: [Any]

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    func updateUIViewController(_ uiViewController: UIActivityViewController, context: Context) {}
}

#if DEBUG
struct SettingsView_Previews: PreviewProvider {
    static var previews: some View {
        ScreenWithTopBar(title: "Settings", onMenu: {}) {
            SettingsView(
                viewModel: .previewConnected(
                    clipboardHistory: ClipboardHistoryEntry.previewSamples
                )
            )
        }
        .preferredColorScheme(.dark)
    }
}
#endif
