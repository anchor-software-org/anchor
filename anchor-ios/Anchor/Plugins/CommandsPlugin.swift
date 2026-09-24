import Combine
import Foundation

final class CommandsPlugin: Plugin, ObservableObject {
    let pluginId = "commands"

    @Published private(set) var commands: [RemoteCommandDefinition] = []
    @Published private(set) var lastResult: RemoteCommandResult?
    @Published private(set) var isAvailable = false

    private let broker: MessageBroker
    private var cancellables = Set<AnyCancellable>()

    init(broker: MessageBroker) {
        self.broker = broker
    }

    func start() {
        broker.events
            .receive(on: DispatchQueue.main)
            .filter { $0.target == .service("commands") }
            .sink { [weak self] event in
                guard case .commands(let message) = event.message else { return }
                self?.handle(message)
            }
            .store(in: &cancellables)
    }

    func stop() {
        cancellables.removeAll()
        isAvailable = false
        commands = []
    }

    func run(_ command: RemoteCommandDefinition) {
        guard isAvailable else { return }
        broker.send(AnchorEvent(target: .device, message: .commands(.run(commandID: command.id))))
    }

    func killCurrent() {
        guard let result = lastResult,
              result.status == .started,
              !result.executionID.isEmpty else { return }
        broker.send(AnchorEvent(target: .device, message: .commands(.kill(executionID: result.executionID))))
    }

    func clearResult() {
        lastResult = nil
    }

    private func handle(_ message: CommandWireMessage) {
        switch message {
        case .availability(let available):
            isAvailable = available
            if !available { commands = []; lastResult = nil }
        case .list(let definitions):
            isAvailable = true
            commands = definitions
        case .result(let result):
            lastResult = result
        case .run, .kill:
            break
        }
    }
}
