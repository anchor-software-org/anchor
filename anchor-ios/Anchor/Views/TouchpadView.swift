import SwiftUI

struct TouchpadView: View {
    let inputPlugin: InputPlugin
    let sensitivity: Float
    @State private var showKeyboard = false
    @State private var textBuffer = ""
    @State private var ctrlActive = false
    @State private var altActive = false
    @State private var superActive = false
    @FocusState private var textFieldFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            // Touchpad surface
            TouchpadGestureArea(inputPlugin: inputPlugin, sensitivity: sensitivity)
                .frame(maxWidth: .infinity, maxHeight: .infinity)

            // Modifier bar
            if showKeyboard {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ModKey(label: "Ctrl", active: ctrlActive) { ctrlActive.toggle() }
                        ModKey(label: "Alt", active: altActive) { altActive.toggle() }
                        ModKey(label: "Super", active: superActive) { superActive.toggle() }

                        Divider().frame(height: 20).background(Color.anchorGray)

                        QuickKey(label: "Esc") { sendWithModifiers("Escape") }
                        QuickKey(label: "Tab") { sendWithModifiers("Tab") }
                        QuickKey(label: "Del") { sendWithModifiers("Delete") }
                        QuickKey(label: "Home") { sendWithModifiers("Home") }
                        QuickKey(label: "End") { sendWithModifiers("End") }
                        QuickKey(label: "PgUp") { sendWithModifiers("PageUp") }
                        QuickKey(label: "PgDn") { sendWithModifiers("PageDown") }
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                }
                .background(Color(hex: 0x1E2128))
            }

            // Bottom bar
            HStack(spacing: 12) {
                Button(action: {
                    showKeyboard.toggle()
                    if showKeyboard { textFieldFocused = true }
                }) {
                    Text("Keyboard")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundColor(showKeyboard ? .anchorBlue40 : .offWhite)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(showKeyboard ? Color.anchorBlue40.opacity(0.2) : Color(hex: 0x3A3D45))
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                }

                if showKeyboard {
                    TextField("Type here...", text: $textBuffer)
                        .textFieldStyle(.plain)
                        .font(.system(size: 14))
                        .foregroundColor(.offWhite)
                        .focused($textFieldFocused)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .background(Color(hex: 0x1A1D21))
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                        .onSubmit {
                            sendWithModifiers("Return")
                            textBuffer = ""
                        }
                        .onChange(of: textBuffer) { oldValue, newValue in
                            if newValue.count > oldValue.count {
                                let added = String(newValue.suffix(newValue.count - oldValue.count))
                                let mods = activeModifiers()
                                if !mods.isEmpty {
                                    inputPlugin.sendKeyCombo(modifiers: mods, key: added)
                                    clearModifiers()
                                } else {
                                    inputPlugin.sendText(added)
                                }
                            } else if newValue.count < oldValue.count {
                                let deleted = oldValue.count - newValue.count
                                for _ in 0..<deleted {
                                    inputPlugin.sendKey("BackSpace")
                                }
                            }
                            if textBuffer.count > 500 {
                                textBuffer = String(textBuffer.suffix(500))
                            }
                        }
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Color(hex: 0x252830))
        }
    }

    private func activeModifiers() -> [String] {
        var mods: [String] = []
        if ctrlActive { mods.append("ctrl") }
        if altActive { mods.append("alt") }
        if superActive { mods.append("super") }
        return mods
    }

    private func clearModifiers() {
        ctrlActive = false
        altActive = false
        superActive = false
    }

    private func sendWithModifiers(_ key: String) {
        let mods = activeModifiers()
        if !mods.isEmpty {
            inputPlugin.sendKeyCombo(modifiers: mods, key: key)
            clearModifiers()
        } else {
            inputPlugin.sendKey(key)
        }
    }
}

// MARK: - Gesture area (UIKit for multi-touch)

struct TouchpadGestureArea: UIViewRepresentable {
    let inputPlugin: InputPlugin
    let sensitivity: Float

    func makeUIView(context: Context) -> TouchpadUIView {
        let view = TouchpadUIView()
        view.inputPlugin = inputPlugin
        view.sensitivity = sensitivity
        view.isMultipleTouchEnabled = true
        view.backgroundColor = UIColor(red: 0.1, green: 0.11, blue: 0.13, alpha: 1)
        return view
    }

    func updateUIView(_ uiView: TouchpadUIView, context: Context) {
        uiView.sensitivity = sensitivity
    }
}

class TouchpadUIView: UIView {
    var inputPlugin: InputPlugin!
    var sensitivity: Float = 1.5
    private var prevTouch: CGPoint?
    private var startTouch: CGPoint?
    private var touchDownTime: TimeInterval = 0
    private var touchCount = 0
    private var moved = false
    private var isPencil = false

    // Pencil needs bigger dead zones due to hand tremor on glass
    private var moveDeadZone: CGFloat { isPencil ? 6 : 3 }
    private var tapMaxTravel: CGFloat { isPencil ? 10 : 18 }
    private let tapMaxDuration: TimeInterval = 0.25 // must lift within 250ms for tap

    // Prevent system gesture recognizers from stealing two-finger touches
    override func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
        return false
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        let allTouches = event?.allTouches ?? touches
        touchCount = allTouches.count
        moved = false
        wasScrolling = false

        if let touch = touches.first {
            isPencil = touch.type == .pencil
            let pos = touch.location(in: self)
            prevTouch = pos
            startTouch = pos
            touchDownTime = CACurrentMediaTime()
        }
    }

    private var wasScrolling = false

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = touches.first, let prev = prevTouch else { return }
        let pos = touch.location(in: self)

        let allTouches = event?.allTouches ?? touches
        let currentFingers = allTouches.filter { $0.phase != .ended && $0.phase != .cancelled }.count

        // When transitioning from scroll (2 fingers) to pointer (1 finger),
        // reset prevTouch to avoid a huge delta from the finger position jump.
        if wasScrolling && currentFingers < 2 {
            wasScrolling = false
            prevTouch = pos
            return
        }
        wasScrolling = currentFingers >= 2

        let rawDx = pos.x - prev.x
        let rawDy = pos.y - prev.y
        let dist = hypot(rawDx, rawDy)

        guard dist > moveDeadZone else { return }

        moved = true

        if currentFingers >= 2 {
            inputPlugin.sendAxis(axis: InputPlugin.AXIS_VERTICAL, value: Float(rawDy) * 0.3)
        } else {
            let accel = Float(pow(dist, 0.3))
            let dx = Float(rawDx) * sensitivity * accel / Float(dist)
            let dy = Float(rawDy) * sensitivity * accel / Float(dist)
            inputPlugin.sendMotion(dx: dx * Float(dist), dy: dy * Float(dist))
        }
        prevTouch = pos
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        let elapsed = CACurrentMediaTime() - touchDownTime

        // Tap: must be quick AND not moved far
        if elapsed < tapMaxDuration {
            let end = touches.first?.location(in: self) ?? (startTouch ?? .zero)
            let start = startTouch ?? end
            let travel = hypot(end.x - start.x, end.y - start.y)

            if !moved || travel < tapMaxTravel {
                if touchCount >= 2 {
                    inputPlugin.sendButton(button: InputPlugin.BTN_RIGHT, pressed: true)
                    inputPlugin.sendButton(button: InputPlugin.BTN_RIGHT, pressed: false)
                } else {
                    inputPlugin.sendButton(button: InputPlugin.BTN_LEFT, pressed: true)
                    inputPlugin.sendButton(button: InputPlugin.BTN_LEFT, pressed: false)
                }
            }
        }

        prevTouch = nil
        startTouch = nil
        touchCount = 0
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        prevTouch = nil
        startTouch = nil
        touchCount = 0
    }
}

#if DEBUG
struct TouchpadView_Previews: PreviewProvider {
    static var previews: some View {
        ScreenWithTopBar(title: "Remote Input", onMenu: {}) {
            TouchpadView(
                inputPlugin: InputPlugin(broker: MessageBroker()),
                sensitivity: 1.6
            )
        }
        .preferredColorScheme(.dark)
    }
}
#endif

// MARK: - Key buttons

struct ModKey: View {
    let label: String
    let active: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(label)
                .font(.system(size: 12, weight: active ? .bold : .medium, design: .monospaced))
                .foregroundColor(active ? .anchorBlue40 : .mediumGray)
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .background(active ? Color.anchorBlue40.opacity(0.35) : Color(hex: 0x2E3138))
                .clipShape(RoundedRectangle(cornerRadius: 6))
        }
    }
}

struct QuickKey: View {
    let label: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(label)
                .font(.system(size: 12, weight: .medium, design: .monospaced))
                .foregroundColor(.offWhite)
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .background(Color(hex: 0x2E3138))
                .clipShape(RoundedRectangle(cornerRadius: 6))
        }
    }
}
