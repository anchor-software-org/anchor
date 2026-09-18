import SwiftUI

enum Screen {
    case main, settings, notifications, remoteInput, sideboat, clipboard
}

private let streamTouchMotionInterval: CFTimeInterval = 1.0 / 30.0
private let streamTouchFlushEpsilon: Float = 0.0001

enum StreamInputMode: String, CaseIterable {
    case pointer = "Pointer"
    case scroll = "Scroll"
    case draw = "Draw"
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

                    // Top bar: exit + mode picker + FPS — single row, no overlap
                    VStack {
                        HStack(spacing: 12) {
                            // Exit button
                            Button { isFullscreen = false } label: {
                                Image(systemName: "xmark")
                                    .font(.system(size: 12, weight: .bold))
                                    .foregroundColor(.white)
                                    .frame(width: 32, height: 32)
                                    .background(Color.black.opacity(0.5))
                                    .clipShape(Circle())
                            }

                            // Mode selector (only when touch input enabled)
                            if viewModel.touchInputOnStream {
                                HStack(spacing: 0) {
                                    ForEach(StreamInputMode.allCases, id: \.self) { mode in
                                        Button {
                                            streamInputMode = mode
                                        } label: {
                                            Text(mode.rawValue)
                                                .font(.system(size: 11, weight: streamInputMode == mode ? .semibold : .regular))
                                                .foregroundColor(streamInputMode == mode ? .white : .white.opacity(0.5))
                                                .padding(.horizontal, 12)
                                                .padding(.vertical, 6)
                                                .background(streamInputMode == mode ? Color.anchorBlue40.opacity(0.5) : Color.clear)
                                        }
                                    }
                                }
                                .background(Color.black.opacity(0.4))
                                .clipShape(RoundedRectangle(cornerRadius: 8))
                            }

                            Spacer()

                            // FPS + latency
                            if viewModel.videoPlugin.isReceiving {
                                HStack(spacing: 8) {
                                    Text("\(viewModel.videoPlugin.fps) fps")
                                        .font(.system(size: 11, design: .monospaced))
                                    if viewModel.latencyMs > 0 {
                                        Text("\(viewModel.latencyMs)ms")
                                            .font(.system(size: 11, design: .monospaced))
                                            .foregroundColor(.seaGreen40)
                                    }
                                }
                                .foregroundColor(.white.opacity(0.7))
                                .padding(.horizontal, 10)
                                .padding(.vertical, 5)
                                .background(Color.black.opacity(0.4))
                                .clipShape(RoundedRectangle(cornerRadius: 6))
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.top, 48)
                        Spacer()
                    }
                }
                .ignoresSafeArea()
                .statusBarHidden(true)
                .onTapGesture {
                    if !viewModel.touchInputOnStream {
                        isFullscreen = false
                    }
                }
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
        .preferredColorScheme(.dark)
        .sheet(isPresented: Binding(
            get: { if case .requested = viewModel.pairingState { return true }; return false },
            set: { if !$0 { viewModel.respondToPairing(accepted: false) } }
        )) {
            PairingDialogView(viewModel: viewModel)
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
                PlaceholderContent(title: "Notifications", icon: "bell.fill")
            }
        case .remoteInput:
            ScreenWithTopBar(title: "Remote Input", onMenu: { withAnimation(.easeOut(duration: 0.2)) { showDrawer = true } }) {
                TouchpadView(inputPlugin: viewModel.inputPlugin, sensitivity: viewModel.touchpadSensitivity)
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
        }
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
            .background(Color.deepOcean)

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

    var statusColor: Color {
        switch viewModel.connectionState.status {
        case .connected: return .seaGreen40
        case .connecting: return .anchorBlue40
        case .disconnected: return .coralRed40
        }
    }

    var statusText: String {
        switch viewModel.connectionState.status {
        case .connected: return "Connected"
        case .connecting: return "Connecting..."
        case .disconnected: return "Disconnected"
        }
    }

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

                HStack(spacing: 5) {
                    Circle().fill(statusColor).frame(width: 6, height: 6)
                    Text(statusText).font(.system(size: 12)).foregroundColor(statusColor)
                }
                .padding(.trailing, 16)
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
                            Text(UIDevice.current.name)
                                .font(.system(size: 12))
                                .foregroundColor(.anchorGray)
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
                        VStack(spacing: 12) {
                            HStack(spacing: 12) {
                                RoundedRectangle(cornerRadius: 10)
                                    .fill(Color.deepOcean)
                                    .frame(width: 38, height: 38)
                                    .overlay(Text("PC").font(.system(size: 11, weight: .bold)).foregroundColor(.white))
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(viewModel.connectionState.host.isEmpty ? "Desktop" : viewModel.connectionState.host)
                                        .font(.system(size: 14, weight: .medium))
                                        .foregroundColor(.offWhite)
                                    Text("Connected")
                                        .font(.system(size: 11))
                                        .foregroundColor(.seaGreen40)
                                }
                                Spacer()
                                Circle().fill(Color.seaGreen40).frame(width: 8, height: 8)
                            }

                            Button(role: .destructive) { viewModel.disconnect() } label: {
                                Text("Disconnect")
                                    .font(.system(size: 13))
                                    .frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.bordered)
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
                    // Stats
                    HStack(spacing: 12) {
                        StatCard(value: viewModel.videoPlugin.isReceiving ? "\(viewModel.videoPlugin.fps)" : "--", label: "FPS", color: .anchorBlue40)
                        StatCard(value: viewModel.latencyMs > 0 ? "\(viewModel.latencyMs)ms" : "--", label: "Latency", color: .seaGreen40)
                    }

                    Button(action: onSideboat) {
                        HStack(spacing: 14) {
                            RoundedRectangle(cornerRadius: 12)
                                .fill(Color.anchorBlue40.opacity(0.15))
                                .frame(width: 42, height: 42)
                                .overlay(
                                    Image(systemName: "ferry.fill")
                                        .font(.system(size: 18, weight: .semibold))
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

                            Image(systemName: "chevron.right")
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundColor(.anchorGray)
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
                                    Circle()
                                        .fill(device.isOnline ? Color.seaGreen40 : Color.mediumGray)
                                        .frame(width: 6, height: 6)
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

                    // Quick settings
                    VStack(spacing: 12) {
                        HStack {
                            Text("Touch on stream")
                                .font(.system(size: 13))
                                .foregroundColor(.offWhite)
                            Spacer()
                            Toggle("", isOn: $viewModel.touchInputOnStream)
                                .labelsHidden()
                                .scaleEffect(0.8)
                        }
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
        VStack(spacing: 0) {
            HStack {
                Button(action: onMenu) {
                    Image(systemName: "line.3.horizontal")
                        .font(.system(size: 20))
                        .foregroundColor(.anchorGray)
                }
                .padding(.leading, 16)
                Text("Sideboat")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundColor(.offWhite)
                    .padding(.leading, 12)
                Spacer()
            }
            .padding(.top, 52)
            .padding(.bottom, 8)
            .background(Color.deepOcean)

            ZStack {
                Color.anchorSurface

                VideoDisplayView(videoPlugin: viewModel.videoPlugin)

                if viewModel.videoPlugin.isReceiving {
                    VStack {
                        Spacer()
                        Text("Tap for fullscreen")
                            .font(.system(size: 12))
                            .foregroundColor(.anchorBlue40)
                            .padding(.horizontal, 16)
                            .padding(.vertical, 6)
                            .background(Color.anchorBlue40.opacity(0.15))
                            .clipShape(RoundedRectangle(cornerRadius: 20))
                            .padding(.bottom, 12)
                    }
                } else if viewModel.connectionState.status == .connected {
                    VStack(spacing: 12) {
                        Image(systemName: "tv.slash")
                            .font(.system(size: 36))
                            .foregroundColor(.anchorGray.opacity(0.5))
                        Text("Waiting for stream...")
                            .font(.system(size: 14))
                            .foregroundColor(.anchorGray)
                    }
                } else {
                    VStack(spacing: 12) {
                        Image(systemName: "ferry.fill")
                            .font(.system(size: 36))
                            .foregroundColor(.anchorGray.opacity(0.5))
                        Text("Connect a desktop to view stream")
                            .font(.system(size: 14))
                            .foregroundColor(.anchorGray)
                    }
                }
            }
            .contentShape(Rectangle())
            .onTapGesture { if viewModel.connectionState.status == .connected { onFullscreen() } }
            .frame(maxHeight: .infinity)

            // Stats bar
            HStack {
                StatCard(value: viewModel.videoPlugin.isReceiving ? "\(viewModel.videoPlugin.fps)" : "--", label: "FPS", color: .anchorBlue40)
                StatCard(value: viewModel.latencyMs > 0 ? "\(viewModel.latencyMs)ms" : "--", label: "Latency", color: .seaGreen40)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
        .background(Color.charcoalBlack)
        .ignoresSafeArea(edges: .top)
        .onAppear {
            viewModel.ensureAutoConnect()
        }
    }
}

// MARK: - Components

struct SwingingAnchorLogo: View {
    @State private var swing = false

    var body: some View {
        Image("AnchorLogo")
            .resizable()
            .aspectRatio(contentMode: .fit)
            .frame(width: 50, height: 50)
            .opacity(0.15)
            .rotationEffect(.degrees(swing ? 0 : 20), anchor: UnitPoint(x: 0.25, y: 0.13))
            .animation(
                .interpolatingSpring(stiffness: 15, damping: 3)
                .repeatForever(autoreverses: true),
                value: swing
            )
            .onAppear { swing = true }
    }
}

struct StatCard: View {
    let value: String
    let label: String
    let color: Color

    var body: some View {
        VStack(spacing: 4) {
            Text(value).font(.system(size: 20, weight: .semibold)).foregroundColor(color)
            Text(label).font(.system(size: 10)).foregroundColor(.mediumGray)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 10)
        .background(Color.darkGray.opacity(0.4))
        .clipShape(RoundedRectangle(cornerRadius: 10))
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
        view.mode = mode
        view.backgroundColor = .clear
        return view
    }

    func updateUIView(_ uiView: FullscreenTouchUIView, context: Context) {
        uiView.inputPlugin = viewModel.inputPlugin
        uiView.videoPlugin = viewModel.videoPlugin
        uiView.mode = mode
    }
}

class FullscreenTouchUIView: UIView {
    var inputPlugin: InputPlugin!
    var videoPlugin: VideoPlugin!
    var mode: StreamInputMode = .pointer
    private var downTime: TimeInterval = 0
    private var dragging = false
    private var prevTouch: CGPoint?
    private var lastMotionSendTime: CFTimeInterval = 0
    private var lastSentPoint: (Float, Float)?
    private var latestPoint: (Float, Float)?
    private var touchInProgress = false
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
            let (nx, ny) = mapToStream(pos)
            inputPlugin.sendMotionAbsolute(x: nx, y: ny)
        default:
            break
        }
    }

    private func mapToStream(_ pos: CGPoint) -> (Float, Float) {
        let viewW = bounds.width
        let viewH = bounds.height
        guard viewW > 0, viewH > 0 else { return (0, 0) }

        let streamW = CGFloat(videoPlugin.streamWidth)
        let streamH = CGFloat(videoPlugin.streamHeight)
        guard streamW > 0, streamH > 0 else {
            return (Float(pos.x / viewW), Float(pos.y / viewH))
        }

        let streamAspect = streamW / streamH
        let viewAspect = viewW / viewH
        let contentW: CGFloat
        let contentH: CGFloat
        let offsetX: CGFloat
        let offsetY: CGFloat

        if viewAspect > streamAspect {
            contentH = viewH
            contentW = viewH * streamAspect
            offsetX = (viewW - contentW) / 2
            offsetY = 0
        } else {
            contentW = viewW
            contentH = viewW / streamAspect
            offsetX = 0
            offsetY = (viewH - contentH) / 2
        }

        let rawX = (pos.x - offsetX) / contentW
        let rawY = (pos.y - offsetY) / contentH
        let nx = Float(rawX < 0 ? 0 : rawX > 1 ? 1 : rawX)
        let ny = Float(rawY < 0 ? 0 : rawY > 1 ? 1 : rawY)
        return (nx, ny)
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = touches.first else { return }
        let pos = touch.location(in: self)
        downTime = CACurrentMediaTime()
        dragging = false
        prevTouch = pos
        touchInProgress = true
        lastMotionSendTime = downTime

        switch mode {
        case .pointer:
            let (nx, ny) = mapToStream(pos)
            lastSentPoint = (nx, ny)
            latestPoint = (nx, ny)
            inputPlugin.sendMotionAbsolute(x: nx, y: ny)
        case .scroll:
            lastSentPoint = nil
            latestPoint = nil
            break // just record start position
        case .draw:
            let (nx, ny) = mapToStream(pos)
            lastSentPoint = (nx, ny)
            latestPoint = (nx, ny)
            inputPlugin.sendMotionAbsolute(x: nx, y: ny)
            inputPlugin.sendButton(pressed: true)
            // TODO: forward pressure via tablet events when desktop supports it
        }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = touches.first, let prev = prevTouch else { return }
        let pos = touch.location(in: self)

        switch mode {
        case .pointer:
            let (nx, ny) = mapToStream(pos)
            latestPoint = (nx, ny)
            let now = CACurrentMediaTime()
            if now - lastMotionSendTime >= streamTouchMotionInterval {
                inputPlugin.sendMotionAbsolute(x: nx, y: ny)
                lastSentPoint = (nx, ny)
                lastMotionSendTime = now
            }
            // Long press → drag
            if !dragging && CACurrentMediaTime() - downTime > 0.3 {
                dragging = true
                NSLog("[anchor] [input] drag_start after_ms=%.0f", (CACurrentMediaTime() - downTime) * 1000)
                inputPlugin.sendButton(pressed: true)
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
            let (nx, ny) = mapToStream(pos)
            inputPlugin.sendMotionAbsolute(x: nx, y: ny)
        }
        prevTouch = pos
    }

    private func flushLatestPointIfNeeded() {
        guard let latest = latestPoint else { return }
        guard let lastSent = lastSentPoint else {
            inputPlugin.sendMotionAbsolute(x: latest.0, y: latest.1)
            lastSentPoint = latest
            return
        }
        if abs(latest.0 - lastSent.0) > streamTouchFlushEpsilon || abs(latest.1 - lastSent.1) > streamTouchFlushEpsilon {
            NSLog("[anchor] [input] drag_end_flush x=%.3f y=%.3f", latest.0, latest.1)
            inputPlugin.sendMotionAbsolute(x: latest.0, y: latest.1)
            lastSentPoint = latest
        }
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        let elapsed = CACurrentMediaTime() - downTime

        switch mode {
        case .pointer:
            if dragging {
                flushLatestPointIfNeeded()
                NSLog("[anchor] [input] drag_end duration_ms=%.0f", elapsed * 1000)
                inputPlugin.sendButton(pressed: false)
            } else if elapsed < 0.2 {
                inputPlugin.sendButton(pressed: true)
                inputPlugin.sendButton(pressed: false)
            }
        case .scroll:
            break
        case .draw:
            inputPlugin.sendButton(pressed: false)
        }
        touchInProgress = false
        ignoreNextHoverEvent = true
        hoverSuppressedUntil = CACurrentMediaTime() + 0.01
        dragging = false
        prevTouch = nil
        lastSentPoint = nil
        latestPoint = nil
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        if dragging || mode == .draw {
            inputPlugin.sendButton(pressed: false)
        }
        touchInProgress = false
        ignoreNextHoverEvent = true
        hoverSuppressedUntil = CACurrentMediaTime() + 0.01
        dragging = false
        prevTouch = nil
        lastSentPoint = nil
        latestPoint = nil
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
                    Text("\(UIDevice.current.name) · This device")
                        .font(.system(size: 12))
                        .foregroundColor(.anchorGray)
                }
                .padding(.horizontal, 20)
                .padding(.top, 52)
                .padding(.bottom, 20)
                .frame(maxWidth: .infinity, alignment: .leading)

                SwingingAnchorLogo()
                    .padding(.trailing, 16)
                    .padding(.top, 44)
            }
            .background(LinearGradient(colors: [.deepOcean, .charcoalBlack], startPoint: .top, endPoint: .bottom))

            // Device card
            DeviceCard(isConnected: isConnected, host: viewModel.connectionState.host)
                .onTapGesture { onNavigate(.main) }
                .padding(12)

            SectionLabel("SYNC")
            DrawerItem(icon: "bell.fill", label: "Notifications", tint: .coralRed40) { onNavigate(.notifications) }
            DrawerItem(icon: "doc.on.clipboard.fill", label: "Clipboard", tint: .seaGreen40) { onNavigate(.clipboard) }

            Divider().padding(.horizontal, 20).padding(.vertical, 4)

            SectionLabel("CONTROL")
            DrawerItem(icon: "hand.tap.fill", label: "Remote Input", tint: .anchorGray) { onNavigate(.remoteInput) }
            DrawerItem(icon: "ferry.fill", label: "Sideboat", tint: .anchorBlue40) { onNavigate(.sideboat) }

            Divider().padding(.horizontal, 20).padding(.vertical, 4)

            DrawerItem(icon: "gear", label: "Settings", tint: .anchorGray) { onNavigate(.settings) }

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
                    Text(isConnected ? "PC" : "?")
                        .font(.system(size: 11, weight: .bold))
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
            Circle().fill(isConnected ? Color.seaGreen40 : Color.coralRed40).frame(width: 8, height: 8)
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
                    .overlay(Image(systemName: icon).font(.system(size: 14)).foregroundColor(tint))
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
