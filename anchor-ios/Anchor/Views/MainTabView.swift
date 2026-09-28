import SwiftUI
import AnchorSDK

enum Screen {
    case main, settings, notifications, remoteInput, sideboat, clipboard, media, commands, files
}

private let streamTouchMotionInterval: CFTimeInterval = 1.0 / 30.0
private let streamTouchFlushEpsilon: Float = 0.0001

enum StreamInputMode: String, CaseIterable {
    case pointer = "Pointer"
    case scroll = "Scroll"
    case draw = "Draw"
}

enum DrawerSwipePolicy {
    static let maximumStartX: CGFloat = 32
    static let minimumTravelX: CGFloat = 120
    static let horizontalDominance: CGFloat = 1.5

    static func shouldOpen(start: CGPoint, translation: CGSize) -> Bool {
        guard start.x <= maximumStartX,
              translation.width >= minimumTravelX else { return false }
        return translation.width >= abs(translation.height) * horizontalDominance
    }
}

private struct DrawerSwipeModifier: ViewModifier {
    let enabled: Bool
    let onOpen: () -> Void

    @ViewBuilder
    func body(content: Content) -> some View {
        if enabled {
            content.simultaneousGesture(
                DragGesture(minimumDistance: 24, coordinateSpace: .global)
                    .onEnded { value in
                        guard DrawerSwipePolicy.shouldOpen(
                            start: value.startLocation,
                            translation: value.translation
                        ) else { return }
                        onOpen()
                    }
            )
        } else {
            content
        }
    }
}

struct MainTabView: View {
    @StateObject private var viewModel: MainViewModel
    @State private var currentScreen: Screen
    @State private var isFullscreen = false
    @State private var showDrawer = false
    @State private var streamInputMode: StreamInputMode = .pointer

    @MainActor
    init() {
        _viewModel = StateObject(wrappedValue: MainViewModel())
        _currentScreen = State(initialValue: .main)
    }

    @MainActor
    init(viewModel: MainViewModel, initialScreen: Screen = .main) {
        _viewModel = StateObject(wrappedValue: viewModel)
        _currentScreen = State(initialValue: initialScreen)
    }

    var body: some View {
        ZStack {
            if isFullscreen {
                ZStack {
                    Color.black.ignoresSafeArea()
                    VideoDisplayView(videoPlugin: viewModel.videoPlugin)
                        .ignoresSafeArea()

                    // Touch input overlay — mode-aware
                    if viewModel.touchInputOnStream {
                        FullscreenTouchOverlay(viewModel: viewModel, mode: streamInputMode)
                    }

                    FullscreenStreamControls(
                        videoPlugin: viewModel.videoPlugin,
                        latencyMs: viewModel.latencyMs,
                        touchInputEnabled: viewModel.touchInputOnStream,
                        inputMode: $streamInputMode,
                        onExit: { isFullscreen = false }
                    )
                }
                .ignoresSafeArea()
                .statusBarHidden(true)
            } else {
                // Drawer + content
                ZStack(alignment: .leading) {
                    contentView
                        .disabled(showDrawer)
                        .offset(x: showDrawer ? 280 : 0)

                    if showDrawer {
                        Color.black.opacity(0.4)
                            .ignoresSafeArea()
                            .offset(x: 280)
                            .onTapGesture { withAnimation(.easeOut(duration: 0.2)) { showDrawer = false } }

                        DrawerView(
                            viewModel: viewModel,
                            currentScreen: currentScreen,
                            onNavigate: { screen in
                                currentScreen = screen
                                withAnimation(.easeOut(duration: 0.2)) { showDrawer = false }
                            }
                        )
                        .frame(width: 280)
                        .transition(.move(edge: .leading))
                    }
                }
                .animation(.easeOut(duration: 0.2), value: showDrawer)
            }
        }
        .modifier(DrawerSwipeModifier(enabled: drawerSwipeEnabled) {
            withAnimation(.easeOut(duration: 0.2)) { showDrawer = true }
        })
        .preferredColorScheme(.dark)
        .sheet(isPresented: Binding(
            get: { if case .requested = viewModel.pairingState { return true }; return false },
            set: { if !$0 { viewModel.respondToPairing(accepted: false) } }
        )) {
            PairingDialogView(viewModel: viewModel)
        }
    }

    private var drawerSwipeEnabled: Bool {
        guard !isFullscreen, !showDrawer else { return false }
        switch currentScreen {
        case .remoteInput, .sideboat:
            return false
        case .main, .settings, .notifications, .clipboard, .media, .commands, .files:
            return true
        }
    }

    @ViewBuilder
    private var contentView: some View {
        switch currentScreen {
        case .main:
            MainScreen(
                viewModel: viewModel,
                onMenu: { withAnimation(.easeOut(duration: 0.2)) { showDrawer = true } },
                onFullscreen: { isFullscreen = true },
                onSideboat: { currentScreen = .sideboat }
            )
        case .settings:
            ScreenWithTopBar(title: "Settings", onMenu: { withAnimation(.easeOut(duration: 0.2)) { showDrawer = true } }) {
                SettingsView(viewModel: viewModel)
            }
        case .notifications:
            ScreenWithTopBar(title: "Notifications", onMenu: { withAnimation(.easeOut(duration: 0.2)) { showDrawer = true } }) {
                NotificationsView(notificationPlugin: viewModel.notificationPlugin)
            }
        case .remoteInput:
            ScreenWithTopBar(title: "Remote Input", onMenu: { withAnimation(.easeOut(duration: 0.2)) { showDrawer = true } }) {
                TouchpadView(
                    inputPlugin: viewModel.inputPlugin,
                    videoPlugin: viewModel.videoPlugin,
                    sensitivity: viewModel.touchpadSensitivity
                )
            }
        case .sideboat:
            SideboatScreen(
                viewModel: viewModel,
                onMenu: { withAnimation(.easeOut(duration: 0.2)) { showDrawer = true } },
                onFullscreen: { isFullscreen = true }
            )
        case .clipboard:
            ScreenWithTopBar(title: "Clipboard", onMenu: { withAnimation(.easeOut(duration: 0.2)) { showDrawer = true } }) {
                ClipboardView(clipboardPlugin: viewModel.clipboardPlugin)
            }
        case .media:
            ScreenWithTopBar(title: "Media", onMenu: { withAnimation(.easeOut(duration: 0.2)) { showDrawer = true } }) {
                MediaView(mediaPlugin: viewModel.mediaPlugin)
            }
        case .commands:
            ScreenWithTopBar(title: "Commands", onMenu: { withAnimation(.easeOut(duration: 0.2)) { showDrawer = true } }) {
                CommandsView(commandsPlugin: viewModel.commandsPlugin)
            }
        case .files:
            ScreenWithTopBar(title: "Files", onMenu: { withAnimation(.easeOut(duration: 0.2)) { showDrawer = true } }) {
                FilesView(filesPlugin: viewModel.filesPlugin)
            }
        }
    }
}

private struct FullscreenStreamControls: View {
    @ObservedObject var videoPlugin: VideoPlugin
    let latencyMs: Int
    let touchInputEnabled: Bool
    @Binding var inputMode: StreamInputMode
    let onExit: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 16) {
                Button(action: onExit) {
                    Image(systemName: "xmark")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundColor(.white)
                        .frame(width: 44, height: 44)
                        .background(Color.black.opacity(0.44))
                        .overlay {
                            Rectangle().stroke(Color.white.opacity(0.14), lineWidth: 1)
                        }
                        .contentShape(Rectangle())
                }

                if videoPlugin.availableOutputs.count > 1 {
                    StreamOutputPicker(videoPlugin: videoPlugin, compact: true)
                }

                Spacer(minLength: 12)

                if videoPlugin.isReceiving, videoPlugin.fps > 0 {
                    StreamMetrics(videoPlugin: videoPlugin, latencyMs: latencyMs)
                        .padding(.horizontal, 8)
                        .frame(height: 36)
                        .background(Color.black.opacity(0.44))
                        .overlay {
                            Rectangle().stroke(Color.white.opacity(0.14), lineWidth: 1)
                        }
                }

                if touchInputEnabled {
                    StreamInputModePicker(inputMode: $inputMode)
                }
            }
            .padding(.horizontal, 10)
            .padding(.top, 16)
            .padding(.bottom, 8)

            Spacer()
        }
    }
}

private struct StreamInputModePicker: View {
    @Binding var inputMode: StreamInputMode

    var body: some View {
        Menu {
            ForEach(StreamInputMode.allCases, id: \.self) { mode in
                Button {
                    inputMode = mode
                } label: {
                    if inputMode == mode {
                        Label(mode.rawValue, systemImage: "checkmark")
                    } else {
                        Text(mode.rawValue)
                    }
                }
            }
        } label: {
            HStack(spacing: 4) {
                Text(inputMode.rawValue)
                Image(systemName: "chevron.down")
                    .font(.system(size: 8, weight: .semibold))
            }
            .font(.system(size: 11, weight: .medium))
            .foregroundColor(.white.opacity(0.78))
            .padding(.horizontal, 8)
            .frame(height: 30)
            .background(Color.black.opacity(0.44))
            .overlay {
                Rectangle().stroke(Color.white.opacity(0.14), lineWidth: 1)
            }
            .contentShape(Rectangle())
        }
        .accessibilityLabel("Stream input mode, \(inputMode.rawValue)")
    }
}

// MARK: - Top bar wrapper

struct ScreenWithTopBar<Content: View>: View {
    let title: String
    let onMenu: () -> Void
    @ViewBuilder let content: Content

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button(action: onMenu) {
                    Image(systemName: "line.3.horizontal")
                        .font(.system(size: 20))
                        .foregroundColor(.anchorGray)
                }
                .padding(.leading, 16)
                Text(title)
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundColor(.offWhite)
                    .padding(.leading, 12)
                Spacer()
            }
            .padding(.top, 52)
            .padding(.bottom, 12)
            .background(Color.charcoalBlack)
            .overlay(alignment: .bottom) {
                Rectangle().fill(Color.white.opacity(0.08)).frame(height: 1)
            }

            content
        }
        .background(Color.charcoalBlack)
        .ignoresSafeArea(edges: .top)
    }
}

// MARK: - Main screen (two-column dashboard)

struct MainScreen: View {
    @ObservedObject var viewModel: MainViewModel
    let onMenu: () -> Void
    let onFullscreen: () -> Void
    let onSideboat: () -> Void

    var isConnected: Bool { viewModel.connectionState.status == .connected }

    var body: some View {
        VStack(spacing: 0) {
            // Top bar
            HStack {
                Button(action: onMenu) {
                    Image(systemName: "line.3.horizontal")
                        .font(.system(size: 20))
                        .foregroundColor(.anchorGray)
                }
                .padding(.leading, 16)

                Text("Anchor")
                    .font(.system(size: 20, weight: .bold))
                    .foregroundColor(.offWhite)
                    .padding(.leading, 8)

                Spacer()
            }
            .padding(.top, 52)
            .padding(.bottom, 12)

            // Two-column layout
            HStack(alignment: .top, spacing: 16) {
                // Left: Connection
                VStack(spacing: 16) {
                    // Anchor logo header
                    ZStack(alignment: .topTrailing) {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Connection")
                                .font(.system(size: 16, weight: .semibold))
                                .foregroundColor(.offWhite)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(16)
                        .padding(.bottom, 8)
                        .background(
                            LinearGradient(colors: [.deepOcean, .charcoalBlack], startPoint: .top, endPoint: .bottom)
                        )
                        .clipShape(RoundedRectangle(cornerRadius: 12))

                        // Swinging anchor logo
                        SwingingAnchorLogo()
                            .padding(.trailing, 12)
                            .padding(.top, 8)
                    }

                    if isConnected {
                        // Connected card
                        HStack(spacing: 12) {
                            RoundedRectangle(cornerRadius: 10)
                                .fill(Color.deepOcean)
                                .frame(width: 38, height: 38)
                                .overlay(
                                    Image("MaterialComputer")
                                        .renderingMode(.template)
                                        .resizable()
                                        .scaledToFit()
                                        .frame(width: 20, height: 20)
                                        .foregroundColor(.white)
                                )
                            VStack(alignment: .leading, spacing: 2) {
                                Text(viewModel.connectionState.host.isEmpty ? "Desktop" : viewModel.connectionState.host)
                                    .font(.system(size: 14, weight: .medium))
                                    .foregroundColor(.offWhite)
                                Text("Connected")
                                    .font(.system(size: 11))
                                    .foregroundColor(.anchorGray)
                            }
                            Spacer()
                            Button("Disconnect", role: .destructive) { viewModel.disconnect() }
                                .font(.system(size: 12, weight: .medium))
                                .buttonStyle(.bordered)
                                .controlSize(.small)
                        }
                        .padding(14)
                        .background(Color.darkGray.opacity(0.4))
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                    } else {
                        // Connect form
                        VStack(spacing: 12) {
                            TextField("Desktop IP", text: $viewModel.desktopIp)
                                .textFieldStyle(.roundedBorder)
                                .autocorrectionDisabled()
                                .textInputAutocapitalization(.never)
                            Button {
                                viewModel.connectToIp(viewModel.desktopIp)
                            } label: {
                                Text(viewModel.connectionState.status == .connecting ? "Connecting..." : "Connect")
                                    .font(.system(size: 14, weight: .medium))
                                    .frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.borderedProminent)
                            .disabled(viewModel.connectionState.status == .connecting)

                            Text(viewModel.usbTetherActive
                                 ? "Wired link detected"
                                 : "Plug in USB and enable Personal Hotspot for a wired link")
                                .font(.system(size: 11))
                                .foregroundColor(.mediumGray)
                        }
                        .padding(14)
                        .background(Color.darkGray.opacity(0.4))
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                    }

                    Spacer()
                }
                .frame(maxWidth: .infinity)

                // Right: Dashboard
                VStack(spacing: 16) {
                    Button(action: onSideboat) {
                        HStack(spacing: 14) {
                            RoundedRectangle(cornerRadius: 12)
                                .fill(Color.anchorBlue40.opacity(0.15))
                                .frame(width: 42, height: 42)
                                .overlay(
                                    Image("AndroidSailboat")
                                        .renderingMode(.template)
                                        .resizable()
                                        .scaledToFit()
                                        .frame(width: 20, height: 20)
                                        .foregroundColor(.anchorBlue40)
                                )

                            VStack(alignment: .leading, spacing: 3) {
                                Text("Sideboat")
                                    .font(.system(size: 14, weight: .semibold))
                                    .foregroundColor(.offWhite)
                                Text(viewModel.videoPlugin.isReceiving ? "Open the live stream" : "Open the stream view")
                                    .font(.system(size: 11))
                                    .foregroundColor(.mediumGray)
                            }

                            Spacer()

                            StreamMetrics(
                                videoPlugin: viewModel.videoPlugin,
                                latencyMs: viewModel.latencyMs
                            )

                            Image(systemName: "chevron.right")
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundColor(.anchorGray)
                        }
                        .padding(14)
                        .background(Color.darkGray.opacity(0.4))
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                    }

                    if !viewModel.discoveredDesktops.isEmpty {
                        VStack(alignment: .leading, spacing: 10) {
                            Text("Nearby")
                                .font(.system(size: 13, weight: .semibold))
                                .foregroundColor(.anchorGray)

                            ForEach(viewModel.discoveredDesktops) { device in
                                HStack(spacing: 10) {
                                    VStack(alignment: .leading, spacing: 1) {
                                        Text(device.deviceName)
                                            .font(.system(size: 13))
                                            .foregroundColor(.offWhite)
                                        Text(device.wired ? "wired · \(device.ip)" : device.ip)
                                            .font(.system(size: 11))
                                            .foregroundColor(.mediumGray)
                                    }
                                    Spacer()
                                    Button("Connect") {
                                        viewModel.connectToDiscovered(device)
                                    }
                                    .font(.system(size: 11))
                                    .buttonStyle(.bordered)
                                    .controlSize(.mini)
                                }
                            }
                        }
                        .padding(14)
                        .background(Color.darkGray.opacity(0.4))
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                    }

                    // Recent devices
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Paired Devices")
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundColor(.anchorGray)

                        if viewModel.pairedDevices.isEmpty {
                            Text("No devices paired yet")
                                .font(.system(size: 12))
                                .foregroundColor(.mediumGray)
                                .padding(.vertical, 8)
                        } else {
                            ForEach(viewModel.pairedDevices) { device in
                                HStack(spacing: 10) {
                                    VStack(alignment: .leading, spacing: 1) {
                                        Text(device.deviceName)
                                            .font(.system(size: 13))
                                            .foregroundColor(.offWhite)
                                        if let ip = device.lastKnownIp, !ip.isEmpty {
                                            Text(ip)
                                                .font(.system(size: 11))
                                                .foregroundColor(.mediumGray)
                                        }
                                    }
                                    Spacer()
                                    if !device.isOnline, let ip = device.lastKnownIp, !ip.isEmpty {
                                        Button("Connect") {
                                            viewModel.connectToIp(ip)
                                        }
                                        .font(.system(size: 11))
                                        .buttonStyle(.bordered)
                                        .controlSize(.mini)
                                    }
                                }
                            }
                        }
                    }
                    .padding(14)
                    .background(Color.darkGray.opacity(0.4))
                    .clipShape(RoundedRectangle(cornerRadius: 12))

                    // Stream status and quick setting share one compact row.
                    HStack(spacing: 12) {
                        Text("Touch on stream")
                            .font(.system(size: 13))
                            .foregroundColor(.offWhite)

                        Spacer()

                        Toggle("", isOn: $viewModel.touchInputOnStream)
                            .labelsHidden()
                            .scaleEffect(0.8)
                    }
                    .padding(14)
                    .background(Color.darkGray.opacity(0.4))
                    .clipShape(RoundedRectangle(cornerRadius: 12))

                    Spacer()
                }
                .frame(maxWidth: .infinity)
            }
            .padding(.horizontal, 16)
        }
        .background(Color.charcoalBlack)
        .ignoresSafeArea(edges: .top)
    }
}

// MARK: - Sideboat screen (video stream)

struct SideboatScreen: View {
    @ObservedObject var viewModel: MainViewModel
    let onMenu: () -> Void
    let onFullscreen: () -> Void

    var body: some View {
        SideboatStreamSurface(
            videoPlugin: viewModel.videoPlugin,
            connectionStatus: viewModel.connectionState.status,
            latencyMs: viewModel.latencyMs,
            onMenu: onMenu,
            onFullscreen: onFullscreen
        )
        .ignoresSafeArea(edges: .top)
        .onAppear {
            viewModel.ensureAutoConnect()
        }
    }
}

/// Owns observation of the video plugin so frame state and FPS updates redraw the
/// stream UI independently of unrelated MainViewModel changes.
private struct SideboatStreamSurface: View {
    @ObservedObject var videoPlugin: VideoPlugin
    let connectionStatus: ConnectionStatus
    let latencyMs: Int
    let onMenu: () -> Void
    let onFullscreen: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            header

            Rectangle()
                .fill(Color.white.opacity(0.09))
                .frame(height: 1)

            ZStack {
                Color.black

                VideoDisplayView(videoPlugin: videoPlugin)

                if !videoPlugin.isReceiving {
                    streamPlaceholder
                }
            }
            .contentShape(Rectangle())
            .onTapGesture {
                if connectionStatus == .connected, videoPlugin.isReceiving {
                    onFullscreen()
                }
            }
        }
        .background(Color.charcoalBlack)
    }

    private var header: some View {
        HStack(spacing: 12) {
            Button(action: onMenu) {
                Image(systemName: "line.3.horizontal")
                    .font(.system(size: 18, weight: .medium))
                    .foregroundColor(.white)
                    .frame(width: 44, height: 44)
                    .contentShape(Rectangle())
            }

            VStack(alignment: .leading, spacing: 1) {
                Text("Sideboat")
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundColor(.white)
                if videoPlugin.availableOutputs.count > 1 {
                    StreamOutputPicker(videoPlugin: videoPlugin, compact: false)
                } else {
                    Text(videoPlugin.availableOutputs.first?.name ?? "Desktop display")
                        .font(.system(size: 10))
                        .foregroundColor(.white.opacity(0.46))
                }
            }

            Spacer(minLength: 8)

            if videoPlugin.isReceiving {
                StreamMetrics(videoPlugin: videoPlugin, latencyMs: latencyMs)
            }

            Button(action: onFullscreen) {
                Image(systemName: "arrow.up.left.and.arrow.down.right")
                    .font(.system(size: 16, weight: .medium))
                    .foregroundColor(videoPlugin.isReceiving ? .white : .white.opacity(0.35))
                    .frame(width: 44, height: 44)
                    .contentShape(Rectangle())
            }
            .disabled(!videoPlugin.isReceiving)
        }
        .padding(.horizontal, 8)
        .padding(.top, 48)
        .padding(.bottom, 8)
        .background(Color.charcoalBlack)
    }

    @ViewBuilder
    private var streamPlaceholder: some View {
        if connectionStatus == .connected {
            VStack(spacing: 10) {
                ProgressView()
                    .tint(.white.opacity(0.7))
                Text("Waiting for desktop video")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(.white.opacity(0.7))
            }
        } else {
            VStack(spacing: 10) {
                Image(systemName: "display")
                    .font(.system(size: 28, weight: .light))
                    .foregroundColor(.white.opacity(0.38))
                Text("Connect to a desktop to start Sideboat")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(.white.opacity(0.58))
            }
        }
    }
}

private struct StreamOutputPicker: View {
    @ObservedObject var videoPlugin: VideoPlugin
    let compact: Bool
    @State private var isPresentingOutputPicker = false

    private var selectedName: String {
        videoPlugin.availableOutputs.first(where: { $0.id == videoPlugin.selectedOutputID })?.name
            ?? "Choose screen"
    }

    var body: some View {
        Button {
            isPresentingOutputPicker = true
        } label: {
            HStack(spacing: 4) {
                Image(systemName: "display")
                if !compact {
                    Text(selectedName)
                        .lineLimit(1)
                }
                Image(systemName: "chevron.down")
                    .font(.system(size: 8, weight: .semibold))
            }
            .font(.system(size: compact ? 11 : 10, weight: .medium))
            .foregroundColor(.white.opacity(compact ? 0.86 : 0.72))
            .frame(minWidth: 44, minHeight: 36, alignment: .leading)
            .padding(.horizontal, compact ? 8 : 6)
            .background(Color.black.opacity(compact ? 0.44 : 0.18))
            .overlay {
                Rectangle().stroke(Color.white.opacity(compact ? 0.16 : 0.10), lineWidth: 1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Stream screen, \(selectedName)")
        .confirmationDialog(
            "Choose screen",
            isPresented: $isPresentingOutputPicker,
            titleVisibility: .visible
        ) {
            ForEach(videoPlugin.availableOutputs) { output in
                Button {
                    videoPlugin.selectOutput(output.id)
                } label: {
                    if output.id == videoPlugin.selectedOutputID {
                        Label(output.name, systemImage: "checkmark")
                    } else {
                        Text(output.name)
                    }
                }
            }
        }
    }
}

private struct StreamMetrics: View {
    @ObservedObject var videoPlugin: VideoPlugin
    let latencyMs: Int

    var body: some View {
        Group {
            if videoPlugin.isReceiving, videoPlugin.fps > 0 {
                HStack(spacing: 6) {
                    Text("\(videoPlugin.fps) FPS")
                    if latencyMs > 0 {
                        Text("·")
                            .foregroundColor(.white.opacity(0.3))
                        Text("\(latencyMs) ms")
                    }
                }
            }
        }
        .font(.system(size: 11, weight: .medium, design: .monospaced))
        .foregroundColor(.white.opacity(0.68))
    }
}

// MARK: - Components

struct SwingingAnchorLogo: View {
    let animateOnAppear: Bool
    @State private var rotation = 0.0
    @State private var isSwinging = false
    @State private var swingTask: Task<Void, Never>?

    init(animateOnAppear: Bool = false) {
        self.animateOnAppear = animateOnAppear
    }

    var body: some View {
        Image("AnchorLogo")
            .resizable()
            .aspectRatio(contentMode: .fit)
            .frame(width: 50, height: 50)
            .opacity(0.15)
            .rotationEffect(.degrees(rotation), anchor: UnitPoint(x: 0.25, y: 0.13))
            .contentShape(Rectangle())
            .onTapGesture { swing() }
            .onAppear { if animateOnAppear { swing() } }
            .onDisappear { swingTask?.cancel() }
    }

    private func swing() {
        guard !isSwinging else { return }
        isSwinging = true
        swingTask = Task { @MainActor in
            withAnimation(.easeInOut(duration: 0.16)) { rotation = 16 }
            try? await Task.sleep(nanoseconds: 160_000_000)
            guard !Task.isCancelled else { return }
            withAnimation(.easeInOut(duration: 0.22)) { rotation = -10 }
            try? await Task.sleep(nanoseconds: 220_000_000)
            guard !Task.isCancelled else { return }
            withAnimation(.easeOut(duration: 0.2)) { rotation = 0 }
            try? await Task.sleep(nanoseconds: 200_000_000)
            isSwinging = false
        }
    }
}

// MARK: - Fullscreen touch overlay

struct FullscreenTouchOverlay: UIViewRepresentable {
    let viewModel: MainViewModel
    let mode: StreamInputMode

    func makeUIView(context: Context) -> FullscreenTouchUIView {
        let view = FullscreenTouchUIView()
        view.inputPlugin = viewModel.inputPlugin
        view.videoPlugin = viewModel.videoPlugin
        view.selectedOutputName = viewModel.videoPlugin.selectedOutputName
        view.mode = mode
        view.backgroundColor = .clear
        return view
    }

    func updateUIView(_ uiView: FullscreenTouchUIView, context: Context) {
        uiView.inputPlugin = viewModel.inputPlugin
        uiView.videoPlugin = viewModel.videoPlugin
        uiView.selectedOutputName = viewModel.videoPlugin.selectedOutputName
        uiView.updateMode(mode)
    }
}

class FullscreenTouchUIView: UIView {
    var inputPlugin: InputPlugin!
    var videoPlugin: VideoPlugin!
    var selectedOutputName: String?
    var mode: StreamInputMode = .pointer
    private var downTime: TimeInterval = 0
    private var dragging = false
    private var prevTouch: CGPoint?
    private var lastMotionSendTime: CFTimeInterval = 0
    private var lastSentPoint: (Float, Float)?
    private var latestPoint: (Float, Float)?
    private var touchInProgress = false
    private var ignoreCurrentTouch = false
    private var buttonIsDown = false
    private var ignoreNextHoverEvent = false
    private var hoverSuppressedUntil: CFTimeInterval = 0

    override init(frame: CGRect) {
        super.init(frame: frame)
        installPencilHoverRecognizer()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        installPencilHoverRecognizer()
    }

    func updateMode(_ newMode: StreamInputMode) {
        if mode != newMode {
            releasePressedButton(reason: "mode_change", force: true)
            resetTouchState()
            mode = newMode
        }
    }

    override func willMove(toWindow newWindow: UIWindow?) {
        if newWindow == nil {
            releasePressedButton(reason: "overlay_removed", force: true)
            resetTouchState()
        }
        super.willMove(toWindow: newWindow)
    }

    private func releasePressedButton(reason: String, force: Bool = false) {
        guard inputPlugin != nil, force || buttonIsDown else { return }
        NSLog("[anchor] [input] button_release reason=%@ forced=%d", reason, force ? 1 : 0)
        inputPlugin.sendButton(pressed: false)
        buttonIsDown = false
    }

    private func installPencilHoverRecognizer() {
        let hover = UIHoverGestureRecognizer(target: self, action: #selector(handlePencilHover(_:)))
        hover.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.pencil.rawValue)]
        addGestureRecognizer(hover)
    }

    @objc private func handlePencilHover(_ recognizer: UIHoverGestureRecognizer) {
        guard inputPlugin != nil, mode != .scroll, !touchInProgress else { return }
        if CACurrentMediaTime() < hoverSuppressedUntil {
            return
        }
        if ignoreNextHoverEvent {
            ignoreNextHoverEvent = false
            return
        }

        switch recognizer.state {
        case .began, .changed:
            let pos = recognizer.location(in: self)
            guard let (nx, ny) = mapToStream(pos) else { return }
            sendMappedMotion(x: nx, y: ny)
        default:
            break
        }
    }

    private func mapToStream(_ pos: CGPoint) -> (Float, Float)? {
        guard let point = AnchorAspectFitCoordinates.mapIfInside(
            x: Double(pos.x),
            y: Double(pos.y),
            viewWidth: Double(bounds.width),
            viewHeight: Double(bounds.height),
            streamWidth: Double(videoPlugin.streamWidth),
            streamHeight: Double(videoPlugin.streamHeight)
        ) else { return nil }
        return (Float(point.x), Float(point.y))
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = touches.first else { return }
        let pos = touch.location(in: self)
        downTime = CACurrentMediaTime()
        dragging = false
        prevTouch = pos
        touchInProgress = true
        ignoreCurrentTouch = false
        buttonIsDown = false
        lastMotionSendTime = downTime

        switch mode {
        case .pointer:
            guard let (nx, ny) = mapToStream(pos) else {
                ignoreCurrentTouch = true
                touchInProgress = false
                return
            }
            lastSentPoint = (nx, ny)
            latestPoint = (nx, ny)
            sendMappedMotion(x: nx, y: ny)
        case .scroll:
            lastSentPoint = nil
            latestPoint = nil
            break // just record start position
        case .draw:
            guard let (nx, ny) = mapToStream(pos) else {
                ignoreCurrentTouch = true
                touchInProgress = false
                return
            }
            lastSentPoint = (nx, ny)
            latestPoint = (nx, ny)
            sendMappedMotion(x: nx, y: ny)
            inputPlugin.sendButton(pressed: true)
            buttonIsDown = true
            // TODO: forward pressure via tablet events when desktop supports it
        }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = touches.first, let prev = prevTouch else { return }
        guard !ignoreCurrentTouch else { return }
        let pos = touch.location(in: self)

        switch mode {
        case .pointer:
            guard let (nx, ny) = mapToStream(pos) else {
                releasePressedButton(reason: "left_video")
                ignoreCurrentTouch = true
                return
            }
            latestPoint = (nx, ny)
            let now = CACurrentMediaTime()
            if now - lastMotionSendTime >= streamTouchMotionInterval {
                sendMappedMotion(x: nx, y: ny)
                lastSentPoint = (nx, ny)
                lastMotionSendTime = now
            }
            // Long press → drag
            if !dragging && CACurrentMediaTime() - downTime > 0.3 {
                dragging = true
                NSLog("[anchor] [input] drag_start after_ms=%.0f", (CACurrentMediaTime() - downTime) * 1000)
                inputPlugin.sendButton(pressed: true)
                buttonIsDown = true
            }
        case .scroll:
            let dy = Float(pos.y - prev.y)
            let dx = Float(pos.x - prev.x)
            if abs(dy) > abs(dx) {
                inputPlugin.sendAxis(axis: InputPlugin.AXIS_VERTICAL, value: dy * 0.5)
            } else if abs(dx) > 2 {
                inputPlugin.sendAxis(axis: InputPlugin.AXIS_HORIZONTAL, value: dx * 0.5)
            }
        case .draw:
            guard let (nx, ny) = mapToStream(pos) else {
                releasePressedButton(reason: "left_video")
                ignoreCurrentTouch = true
                return
            }
            sendMappedMotion(x: nx, y: ny)
        }
        prevTouch = pos
    }

    private func flushLatestPointIfNeeded() {
        guard let latest = latestPoint else { return }
        guard let lastSent = lastSentPoint else {
            sendMappedMotion(x: latest.0, y: latest.1)
            lastSentPoint = latest
            return
        }
        if abs(latest.0 - lastSent.0) > streamTouchFlushEpsilon || abs(latest.1 - lastSent.1) > streamTouchFlushEpsilon {
            NSLog("[anchor] [input] drag_end_flush x=%.3f y=%.3f", latest.0, latest.1)
            sendMappedMotion(x: latest.0, y: latest.1)
            lastSentPoint = latest
        }
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        let elapsed = CACurrentMediaTime() - downTime

        if ignoreCurrentTouch {
            resetTouchState()
            return
        }

        switch mode {
        case .pointer:
            if dragging {
                flushLatestPointIfNeeded()
                NSLog("[anchor] [input] drag_end duration_ms=%.0f", elapsed * 1000)
                releasePressedButton(reason: "drag_end")
            } else if elapsed < 0.2 {
                inputPlugin.sendButton(pressed: true)
                inputPlugin.sendButton(pressed: false)
            }
        case .scroll:
            break
        case .draw:
            releasePressedButton(reason: "draw_end")
        }
        resetTouchState()
    }

    private func resetTouchState() {
        touchInProgress = false
        ignoreNextHoverEvent = true
        hoverSuppressedUntil = CACurrentMediaTime() + 0.01
        dragging = false
        ignoreCurrentTouch = false
        prevTouch = nil
        lastSentPoint = nil
        latestPoint = nil
    }

    private func sendMappedMotion(x: Float, y: Float) {
        guard let selectedOutputName else {
            NSLog("[anchor] [input] dropping mapped motion without selected output")
            return
        }
        inputPlugin.sendMotionAbsolute(x: x, y: y, outputName: selectedOutputName)
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        releasePressedButton(reason: "touch_cancelled", force: true)
        resetTouchState()
    }
}


// MARK: - Drawer

struct DrawerView: View {
    @ObservedObject var viewModel: MainViewModel
    let currentScreen: Screen
    let onNavigate: (Screen) -> Void

    var isConnected: Bool { viewModel.connectionState.status == .connected }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Header with swinging anchor
            ZStack(alignment: .topTrailing) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Anchor")
                        .font(.system(size: 24, weight: .bold))
                        .foregroundColor(.offWhite)
                    Text(UIDevice.current.name)
                        .font(.system(size: 12))
                        .foregroundColor(.anchorGray)
                }
                .padding(.horizontal, 20)
                .padding(.top, 52)
                .padding(.bottom, 20)
                .frame(maxWidth: .infinity, alignment: .leading)

                SwingingAnchorLogo(animateOnAppear: true)
                    .padding(.trailing, 16)
                    .padding(.top, 44)
            }
            .background(Color.charcoalBlack)

            // Device card
            DeviceCard(isConnected: isConnected, host: viewModel.connectionState.host)
                .onTapGesture { onNavigate(.main) }
                .padding(12)

            SectionLabel("SYNC")
            DrawerItem(icon: "MaterialNotifications", label: "Notifications", tint: .anchorGray) { onNavigate(.notifications) }
            DrawerItem(icon: "AndroidClipboard", label: "Clipboard", tint: .anchorGray) { onNavigate(.clipboard) }
            DrawerItem(icon: "MaterialMusicNote", label: "Media", tint: .anchorGray) { onNavigate(.media) }
            DrawerItem(icon: "MaterialFolder", label: "Files", tint: .anchorGray) { onNavigate(.files) }

            Divider().padding(.horizontal, 20).padding(.vertical, 4)

            SectionLabel("CONTROL")
            DrawerItem(icon: "AndroidMousePointer", label: "Remote Input", tint: .anchorGray) { onNavigate(.remoteInput) }
            DrawerItem(icon: "MaterialTerminal", label: "Commands", tint: .anchorGray) { onNavigate(.commands) }
            DrawerItem(icon: "AndroidSailboat", label: "Sideboat", tint: .anchorBlue40) { onNavigate(.sideboat) }

            Divider().padding(.horizontal, 20).padding(.vertical, 4)

            DrawerItem(icon: "MaterialSettings", label: "Settings", tint: .anchorGray) { onNavigate(.settings) }

            Spacer()
        }
        .background(Color.charcoalBlack)
    }
}

struct DeviceCard: View {
    let isConnected: Bool
    let host: String

    var body: some View {
        HStack(spacing: 12) {
            RoundedRectangle(cornerRadius: 10)
                .fill(isConnected ? Color.deepOcean : Color.coralRed40.opacity(0.3))
                .frame(width: 38, height: 38)
                .overlay(
                    Group {
                        if isConnected {
                            Image("MaterialComputer")
                                .renderingMode(.template)
                                .resizable()
                                .scaledToFit()
                        } else {
                            Image(systemName: "questionmark.circle")
                                .resizable()
                                .scaledToFit()
                        }
                    }
                        .frame(width: 20, height: 20)
                        .foregroundColor(isConnected ? .white : .coralRed40)
                )
            VStack(alignment: .leading, spacing: 2) {
                Text(isConnected ? (host.isEmpty ? "Desktop" : host) : "No device")
                    .font(.system(size: 14, weight: .medium))
                    .foregroundColor(.offWhite)
                Text(isConnected ? "Connected" : "Tap to connect")
                    .font(.system(size: 11))
                    .foregroundColor(isConnected ? .seaGreen40 : .coralRed40)
            }
            Spacer()
        }
        .padding(14)
        .background(Color.darkGray.opacity(0.5))
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }
}

struct SectionLabel: View {
    let text: String
    init(_ text: String) { self.text = text }
    var body: some View {
        Text(text)
            .font(.system(size: 10, weight: .semibold))
            .foregroundColor(.anchorGray)
            .tracking(1.5)
            .padding(.horizontal, 20)
            .padding(.top, 16)
            .padding(.bottom, 6)
    }
}

struct DrawerItem: View {
    let icon: String
    let label: String
    let tint: Color
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 14) {
                RoundedRectangle(cornerRadius: 8)
                    .fill(tint.opacity(0.15))
                    .frame(width: 30, height: 30)
                    .overlay(
                        Image(icon)
                            .renderingMode(.template)
                            .resizable()
                            .scaledToFit()
                            .frame(width: 16, height: 16)
                            .foregroundColor(tint)
                    )
                Text(label).font(.system(size: 14)).foregroundColor(.offWhite)
                Spacer()
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
        }
    }
}

// MARK: - Placeholder

struct PlaceholderContent: View {
    let title: String
    let icon: String

    var body: some View {
        VStack(spacing: 16) {
            Spacer()
            Image(systemName: icon)
                .font(.system(size: 48))
                .foregroundColor(.anchorGray.opacity(0.5))
            Text("Coming Soon")
                .font(.title3)
                .foregroundColor(.anchorGray)
            Text("This feature is under development.")
                .font(.subheadline)
                .foregroundColor(.mediumGray)
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }
}

#if DEBUG
struct MainTabView_Previews: PreviewProvider {
    static var previews: some View {
        Group {
            MainScreen(
                viewModel: .previewConnected(),
                onMenu: {},
                onFullscreen: {},
                onSideboat: {}
            )
            .preferredColorScheme(.dark)
            .previewDisplayName("Main Connected")

            MainScreen(
                viewModel: .previewDisconnected(),
                onMenu: {},
                onFullscreen: {},
                onSideboat: {}
            )
            .preferredColorScheme(.dark)
            .previewDisplayName("Main Disconnected")

            DrawerView(
                viewModel: .previewConnected(),
                currentScreen: .clipboard,
                onNavigate: { _ in }
            )
            .preferredColorScheme(.dark)
            .previewDisplayName("Drawer")

            SideboatScreen(
                viewModel: .previewConnected(),
                onMenu: {},
                onFullscreen: {}
            )
            .preferredColorScheme(.dark)
            .previewDisplayName("Sideboat Streaming")
        }
    }
}
#endif
