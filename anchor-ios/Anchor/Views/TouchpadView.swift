import SwiftUI

enum TouchpadPointerMode: String, CaseIterable {
    case relative
    case absolute

    var label: String { rawValue.capitalized }
}

enum TouchpadCoordinateMapper {
    static func absolute(x: CGFloat, y: CGFloat, width: CGFloat, height: CGFloat) -> (x: Float, y: Float)? {
        guard width > 0, height > 0 else { return nil }
        return (
            Float(min(1, max(0, x / width))),
            Float(min(1, max(0, y / height)))
        )
    }

    static func relative(from previous: CGPoint, to current: CGPoint, sensitivity: Float) -> (dx: Float, dy: Float) {
        (
            Float(current.x - previous.x) * sensitivity,
            Float(current.y - previous.y) * sensitivity
        )
    }
}

struct TouchpadView: View {
    let inputPlugin: InputPlugin
    @ObservedObject var videoPlugin: VideoPlugin
    let sensitivity: Float

    @AppStorage("remoteInput.pointerMode") private var pointerModeValue = TouchpadPointerMode.relative.rawValue
    @State private var showKeyboard = true
    @State private var textBuffer = ""
    @State private var ctrlActive = false
    @State private var altActive = false
    @State private var superActive = false
    @FocusState private var textFieldFocused: Bool

    private var pointerMode: TouchpadPointerMode {
        TouchpadPointerMode(rawValue: pointerModeValue) ?? .relative
    }

    var body: some View {
        VStack(spacing: 0) {
            configurationBar

            TouchpadGestureArea(inputPlugin: inputPlugin, sensitivity: sensitivity, mode: pointerMode)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .overlay {
                    ZStack {
                        Rectangle().frame(width: 24, height: 1)
                        Rectangle().frame(width: 1, height: 24)
                    }
                    .foregroundStyle(Color.anchorGray.opacity(0.45))
                    .allowsHitTesting(false)
                }

            controlStrip

            if showKeyboard {
                keyboardPanel
            }
        }
        .background(Color.charcoalBlack)
    }

    private var configurationBar: some View {
        HStack(spacing: 8) {
            modeButton(.relative)
            modeButton(.absolute)

            if pointerMode == .absolute {
                Rectangle().fill(Color.white.opacity(0.10)).frame(width: 1, height: 24)
                    .padding(.horizontal, 4)
                outputPicker
            }
            Spacer()
        }
        .padding(.horizontal, 12)
        .frame(height: 52)
        .background(Color.charcoalBlack)
        .overlay(alignment: .bottom) {
            Rectangle().fill(Color.white.opacity(0.08)).frame(height: 1)
        }
    }

    private func modeButton(_ mode: TouchpadPointerMode) -> some View {
        Button { pointerModeValue = mode.rawValue } label: {
            Text(mode.label)
                .font(.system(size: 12, weight: pointerMode == mode ? .semibold : .regular))
                .foregroundStyle(pointerMode == mode ? Color.offWhite : Color.anchorGray)
                .frame(width: 78, height: 34)
                .background(pointerMode == mode ? Color.darkGray : Color.clear)
                .clipShape(RoundedRectangle(cornerRadius: 4))
        }
    }

    @ViewBuilder private var outputPicker: some View {
        if videoPlugin.availableOutputs.isEmpty {
            Label("Current display", systemImage: "display")
                .font(.system(size: 12))
                .foregroundStyle(Color.anchorGray)
        } else {
            Menu {
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
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "display")
                    Text(selectedOutputName).lineLimit(1)
                    Image(systemName: "chevron.down").font(.system(size: 9, weight: .semibold))
                }
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Color.offWhite)
                .padding(.horizontal, 10)
                .frame(height: 34)
                .background(Color.darkGray.opacity(0.8))
                .clipShape(RoundedRectangle(cornerRadius: 4))
            }
        }
    }

    private var selectedOutputName: String {
        videoPlugin.availableOutputs.first(where: { $0.id == videoPlugin.selectedOutputID })?.name
            ?? videoPlugin.availableOutputs.first?.name
            ?? "Current display"
    }

    private var controlStrip: some View {
        HStack(spacing: 8) {
            ModifierKey(label: "Ctrl", symbol: "⌃", active: ctrlActive) { ctrlActive.toggle() }
            ModifierKey(label: "Alt", symbol: "⌥", active: altActive) { altActive.toggle() }
            ModifierKey(label: "Super", symbol: "⌘", active: superActive) { superActive.toggle() }

            Spacer()

            Button {
                showKeyboard.toggle()
                textFieldFocused = showKeyboard
            } label: {
                Label(showKeyboard ? "Hide keyboard" : "Show keyboard", systemImage: "keyboard")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Color.offWhite)
                    .padding(.horizontal, 12)
                    .frame(height: 40)
                    .background(Color.darkGray.opacity(0.75))
                    .clipShape(RoundedRectangle(cornerRadius: 4))
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Color(hex: 0x292929))
        .overlay(alignment: .top) {
            Rectangle().fill(Color.white.opacity(0.08)).frame(height: 1)
        }
    }

    private var keyboardPanel: some View {
        VStack(spacing: 8) {
            HStack(spacing: 6) {
                ForEach(["Esc", "Tab", "Del", "Home", "End", "PgUp", "PgDn"], id: \.self) { key in
                    QuickKey(label: key) { sendWithModifiers(keyName(for: key)) }
                }
                Spacer(minLength: 0)
            }

            HStack(spacing: 10) {
                TextField("Type on the desktop", text: $textBuffer)
                    .textFieldStyle(.plain)
                    .font(.system(size: 15))
                    .foregroundStyle(Color.offWhite)
                    .focused($textFieldFocused)
                    .padding(.horizontal, 12)
                    .frame(height: 42)
                    .background(Color(hex: 0x1B1B1B))
                    .overlay {
                        RoundedRectangle(cornerRadius: 4).stroke(Color.white.opacity(0.10), lineWidth: 1)
                    }
                    .onSubmit {
                        sendWithModifiers("Return")
                        textBuffer = ""
                    }
                    .onChange(of: textBuffer) { oldValue, newValue in
                        handleTextChange(oldValue: oldValue, newValue: newValue)
                    }

                QuickKey(label: "Enter", width: 72) {
                    sendWithModifiers("Return")
                    textBuffer = ""
                }
            }
        }
        .padding(12)
        .background(Color(hex: 0x242424))
        .overlay(alignment: .top) {
            Rectangle().fill(Color.white.opacity(0.08)).frame(height: 1)
        }
    }

    private func handleTextChange(oldValue: String, newValue: String) {
        if newValue.count > oldValue.count {
            let added = String(newValue.suffix(newValue.count - oldValue.count))
            let modifiers = activeModifiers()
            if modifiers.isEmpty {
                inputPlugin.sendText(added)
            } else if let character = added.last {
                inputPlugin.sendKeyCombo(modifiers: modifiers, key: String(character))
                clearModifiers()
            }
        } else if newValue.count < oldValue.count {
            for _ in 0..<(oldValue.count - newValue.count) {
                inputPlugin.sendKey("BackSpace")
            }
        }
        if textBuffer.count > 500 {
            textBuffer = String(textBuffer.suffix(500))
        }
    }

    private func keyName(for label: String) -> String {
        switch label {
        case "Esc": return "Escape"
        case "Del": return "Delete"
        case "PgUp": return "PageUp"
        case "PgDn": return "PageDown"
        default: return label
        }
    }

    private func activeModifiers() -> [String] {
        var modifiers: [String] = []
        if ctrlActive { modifiers.append("ctrl") }
        if altActive { modifiers.append("alt") }
        if superActive { modifiers.append("super") }
        return modifiers
    }

    private func clearModifiers() {
        ctrlActive = false
        altActive = false
        superActive = false
    }

    private func sendWithModifiers(_ key: String) {
        let modifiers = activeModifiers()
        if modifiers.isEmpty {
            inputPlugin.sendKey(key)
        } else {
            inputPlugin.sendKeyCombo(modifiers: modifiers, key: key)
            clearModifiers()
        }
    }
}

struct TouchpadGestureArea: UIViewRepresentable {
    let inputPlugin: InputPlugin
    let sensitivity: Float
    let mode: TouchpadPointerMode

    func makeUIView(context: Context) -> TouchpadUIView {
        let view = TouchpadUIView()
        view.inputPlugin = inputPlugin
        view.sensitivity = sensitivity
        view.mode = mode
        view.isMultipleTouchEnabled = true
        view.backgroundColor = UIColor(red: 0.10, green: 0.10, blue: 0.10, alpha: 1)
        return view
    }

    func updateUIView(_ uiView: TouchpadUIView, context: Context) {
        uiView.sensitivity = sensitivity
        uiView.updateMode(mode)
    }
}

final class TouchpadUIView: UIView {
    var inputPlugin: InputPlugin!
    var sensitivity: Float = 1.5
    var mode: TouchpadPointerMode = .relative
    private var prevTouch: CGPoint?
    private var startTouch: CGPoint?
    private var touchDownTime: TimeInterval = 0
    private var touchCount = 0
    private var moved = false
    private var isPencil = false
    private var wasScrolling = false
    private var touchInProgress = false
    private var previousPencilHover: CGPoint?
    private var lastPencilHoverSendTime: CFTimeInterval = 0
    private let pencilHoverInterval: CFTimeInterval = 1.0 / 60.0

    func updateMode(_ newMode: TouchpadPointerMode) {
        guard mode != newMode else { return }
        mode = newMode
        previousPencilHover = nil
    }

    private var moveDeadZone: CGFloat { isPencil ? 6 : 3 }
    private var tapMaxTravel: CGFloat { isPencil ? 10 : 18 }
    private let tapMaxDuration: TimeInterval = 0.25

    override init(frame: CGRect) {
        super.init(frame: frame)
        installPencilHoverRecognizer()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        installPencilHoverRecognizer()
    }

    override func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
        gestureRecognizer is UIHoverGestureRecognizer
    }

    private func installPencilHoverRecognizer() {
        let hover = UIHoverGestureRecognizer(target: self, action: #selector(handlePencilHover(_:)))
        hover.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.pencil.rawValue)]
        addGestureRecognizer(hover)
    }

    @objc private func handlePencilHover(_ recognizer: UIHoverGestureRecognizer) {
        guard inputPlugin != nil, !touchInProgress else { return }
        let position = recognizer.location(in: self)

        switch recognizer.state {
        case .began:
            previousPencilHover = position
            lastPencilHoverSendTime = CACurrentMediaTime()
            if mode == .absolute { sendAbsolute(position) }
        case .changed:
            let now = CACurrentMediaTime()
            guard now - lastPencilHoverSendTime >= pencilHoverInterval else { return }
            if mode == .absolute {
                sendAbsolute(position)
            } else if let previousPencilHover {
                let delta = TouchpadCoordinateMapper.relative(
                    from: previousPencilHover,
                    to: position,
                    sensitivity: sensitivity
                )
                inputPlugin.sendMotion(dx: delta.dx, dy: delta.dy)
            }
            previousPencilHover = position
            lastPencilHoverSendTime = now
        case .ended, .cancelled, .failed:
            previousPencilHover = nil
        default:
            break
        }
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        let allTouches = event?.allTouches ?? touches
        touchCount = allTouches.count
        moved = false
        wasScrolling = false
        touchInProgress = true
        previousPencilHover = nil
        guard let touch = touches.first else { return }
        isPencil = touch.type == .pencil
        let position = touch.location(in: self)
        prevTouch = position
        startTouch = position
        touchDownTime = CACurrentMediaTime()
        if mode == .absolute, touchCount == 1 { sendAbsolute(position) }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = touches.first, let previous = prevTouch else { return }
        let position = touch.location(in: self)
        let allTouches = event?.allTouches ?? touches
        let fingers = allTouches.filter { $0.phase != .ended && $0.phase != .cancelled }.count

        if wasScrolling && fingers < 2 {
            wasScrolling = false
            prevTouch = position
            return
        }
        wasScrolling = fingers >= 2
        let rawDx = position.x - previous.x
        let rawDy = position.y - previous.y
        let distance = hypot(rawDx, rawDy)
        guard distance > moveDeadZone else { return }
        moved = true

        if fingers >= 2 {
            inputPlugin.sendAxis(axis: InputPlugin.AXIS_VERTICAL, value: Float(rawDy) * 0.3)
        } else if mode == .absolute {
            sendAbsolute(position)
        } else {
            let acceleration = Float(pow(distance, 0.3))
            inputPlugin.sendMotion(
                dx: Float(rawDx) * sensitivity * acceleration,
                dy: Float(rawDy) * sensitivity * acceleration
            )
        }
        prevTouch = position
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        let elapsed = CACurrentMediaTime() - touchDownTime
        if elapsed < tapMaxDuration {
            let end = touches.first?.location(in: self) ?? (startTouch ?? .zero)
            let start = startTouch ?? end
            let travel = hypot(end.x - start.x, end.y - start.y)
            if !moved || travel < tapMaxTravel {
                let button = touchCount >= 2 ? InputPlugin.BTN_RIGHT : InputPlugin.BTN_LEFT
                inputPlugin.sendButton(button: button, pressed: true)
                inputPlugin.sendButton(button: button, pressed: false)
            }
        }
        resetTouch()
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        inputPlugin.sendButton(button: InputPlugin.BTN_LEFT, pressed: false)
        inputPlugin.sendButton(button: InputPlugin.BTN_RIGHT, pressed: false)
        resetTouch()
    }

    private func sendAbsolute(_ position: CGPoint) {
        guard let point = TouchpadCoordinateMapper.absolute(
            x: position.x, y: position.y, width: bounds.width, height: bounds.height
        ) else { return }
        inputPlugin.sendMotionAbsolute(x: point.x, y: point.y)
    }

    private func resetTouch() {
        prevTouch = nil
        startTouch = nil
        touchCount = 0
        moved = false
        wasScrolling = false
        touchInProgress = false
        previousPencilHover = nil
    }
}

struct ModifierKey: View {
    let label: String
    let symbol: String
    let active: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Text(symbol).font(.system(size: 14))
                Text(label).font(.system(size: 12, weight: .medium))
            }
            .foregroundStyle(active ? Color.offWhite : Color.mediumGray)
            .padding(.horizontal, 11)
            .frame(height: 40)
            .background(active ? Color.white.opacity(0.14) : Color.darkGray.opacity(0.55))
            .clipShape(RoundedRectangle(cornerRadius: 4))
            .overlay {
                RoundedRectangle(cornerRadius: 4)
                    .stroke(active ? Color.white.opacity(0.45) : Color.clear, lineWidth: 1)
            }
        }
    }
}

struct QuickKey: View {
    let label: String
    var width: CGFloat = 54
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(label)
                .font(.system(size: 11, weight: .medium, design: .monospaced))
                .foregroundStyle(Color.offWhite)
                .frame(width: width, height: 36)
                .background(Color.darkGray.opacity(0.75))
                .clipShape(RoundedRectangle(cornerRadius: 4))
        }
    }
}

#if DEBUG
struct TouchpadView_Previews: PreviewProvider {
    static var previews: some View {
        ScreenWithTopBar(title: "Remote Input", onMenu: {}) {
            TouchpadView(
                inputPlugin: InputPlugin(broker: MessageBroker()),
                videoPlugin: VideoPlugin(broker: MessageBroker()),
                sensitivity: 1.6
            )
        }
        .preferredColorScheme(.dark)
    }
}
#endif
