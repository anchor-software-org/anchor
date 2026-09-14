import Foundation

/// Sends pointer/touch/keyboard events from the phone to the desktop.
/// Desktop InputPlugin receives these and injects via wlr-virtual-pointer + virtual-keyboard.
class InputPlugin: Plugin {
    let pluginId = "input"
    private let broker: MessageBroker

    static let BTN_LEFT: Int = 272
    static let BTN_RIGHT: Int = 273
    static let BTN_MIDDLE: Int = 274
    static let AXIS_VERTICAL: Int = 0
    static let AXIS_HORIZONTAL: Int = 1

    init(broker: MessageBroker) {
        self.broker = broker
    }

    func start() {}
    func stop() {}

    func sendMotion(dx: Float, dy: Float) {
        send([
            "plugin_id": "input",
            "type": "anchor.input.motion",
            "dx": dx,
            "dy": dy,
            "time": timestamp()
        ])
    }

    func sendMotionAbsolute(x: Float, y: Float) {
        send([
            "plugin_id": "input",
            "type": "anchor.input.motion_absolute",
            "x": x,
            "y": y,
            "time": timestamp()
        ])
    }

    func sendButton(button: Int = BTN_LEFT, pressed: Bool) {
        send([
            "plugin_id": "input",
            "type": "anchor.input.button",
            "button": button,
            "state": pressed ? 1 : 0,
            "time": timestamp()
        ])
    }

    func sendAxis(axis: Int = AXIS_VERTICAL, value: Float) {
        send([
            "plugin_id": "input",
            "type": "anchor.input.axis",
            "axis": axis,
            "value": value,
            "time": timestamp()
        ])
    }

    func sendText(_ text: String) {
        send([
            "plugin_id": "input",
            "type": "anchor.input.text",
            "text": text
        ])
    }

    func sendKey(_ key: String) {
        send([
            "plugin_id": "input",
            "type": "anchor.input.key",
            "key": key
        ])
    }

    func sendKeyCombo(modifiers: [String], key: String) {
        send([
            "plugin_id": "input",
            "type": "anchor.input.key_combo",
            "modifiers": modifiers,
            "key": key
        ] as [String: Any])
    }

    private func timestamp() -> UInt64 {
        UInt64(Date().timeIntervalSince1970 * 1000) & 0xFFFFFFFF
    }

    private func send(_ dict: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: dict),
              let json = String(data: data, encoding: .utf8) else { return }
        broker.send(AnchorEvent(target: .device, message: .json(json)))
    }
}
