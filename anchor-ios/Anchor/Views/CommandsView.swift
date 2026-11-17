import SwiftUI

struct CommandsView: View {
    @ObservedObject var commandsPlugin: CommandsPlugin

    private let columns = [GridItem(.adaptive(minimum: 190), spacing: 10)]

    var body: some View {
        VStack(spacing: 0) {
            if let result = commandsPlugin.lastResult {
                resultBar(result)
            }

            if !commandsPlugin.isAvailable {
                emptyState("Commands are unavailable on this desktop.")
            } else if commandsPlugin.commands.isEmpty {
                emptyState("No commands are configured on the desktop.")
            } else {
                ScrollView {
                    LazyVGrid(columns: columns, spacing: 10) {
                        ForEach(commandsPlugin.commands) { command in
                            commandButton(command)
                        }
                    }
                    .padding(12)
                }
            }
        }
        .background(Color.charcoalBlack)
    }

    private func commandButton(_ command: RemoteCommandDefinition) -> some View {
        Button { commandsPlugin.run(command) } label: {
            HStack(alignment: .top, spacing: 12) {
                Image("MaterialPlayArrow")
                    .renderingMode(.template)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 18, height: 18)
                    .foregroundStyle(Color.offWhite)
                    .frame(width: 28, height: 28)
                    .background(Color.darkGray)

                VStack(alignment: .leading, spacing: 5) {
                    Text(command.name.isEmpty ? command.id : command.name)
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(Color.offWhite)
                        .multilineTextAlignment(.leading)
                    if !command.description.isEmpty {
                        Text(command.description)
                            .font(.system(size: 12))
                            .foregroundStyle(Color.anchorGray)
                            .lineLimit(3)
                            .multilineTextAlignment(.leading)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(12)
            .frame(maxWidth: .infinity, minHeight: 72, alignment: .topLeading)
            .background(Color.darkGray.opacity(0.32))
            .overlay { Rectangle().stroke(Color.white.opacity(0.08), lineWidth: 1) }
        }
        .buttonStyle(.plain)
    }

    private func resultBar(_ result: RemoteCommandResult) -> some View {
        HStack(spacing: 10) {
            Text(resultText(result))
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Color.offWhite)
                .lineLimit(2)
            Spacer()
            if result.status == .started && !result.executionID.isEmpty {
                Button("Stop") { commandsPlugin.killCurrent() }
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Color.coralRed40)
            } else {
                Button { commandsPlugin.clearResult() } label: {
                    Image(systemName: "xmark").font(.system(size: 11, weight: .semibold))
                }
                .foregroundStyle(Color.anchorGray)
            }
        }
        .padding(.horizontal, 14)
        .frame(minHeight: 46)
        .background(Color.darkGray.opacity(0.45))
        .overlay(alignment: .bottom) {
            Rectangle().fill(Color.white.opacity(0.08)).frame(height: 1)
        }
    }

    private func resultText(_ result: RemoteCommandResult) -> String {
        switch result.status {
        case .started: return "Command started"
        case .done: return "Command completed"
        case .failed:
            return result.error.isEmpty ? "Command failed (exit \(result.exitCode))" : "Command failed: \(result.error)"
        }
    }

    private func emptyState(_ text: String) -> some View {
        VStack(spacing: 10) {
            Spacer()
            Image("MaterialTerminal")
                .renderingMode(.template)
                .resizable()
                .scaledToFit()
                .frame(width: 40, height: 40)
                .foregroundStyle(Color.anchorGray.opacity(0.55))
            Text(text)
                .font(.system(size: 13))
                .foregroundStyle(Color.anchorGray)
                .multilineTextAlignment(.center)
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }
}
