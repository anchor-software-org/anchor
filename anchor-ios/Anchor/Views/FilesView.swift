import SwiftUI
import UniformTypeIdentifiers

struct FilesView: View {
    @ObservedObject var filesPlugin: FilesPlugin
    @State private var showingImporter = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("File transfer")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundColor(.offWhite)
                    Text(filesPlugin.isAvailable ? "Connected to desktop" : "Connect to send files")
                        .font(.system(size: 11))
                        .foregroundColor(.anchorGray)
                }
                Spacer()
                if !filesPlugin.transfers.isEmpty {
                    Button("Clear") { filesPlugin.clearCompleted() }
                        .font(.system(size: 12, weight: .medium))
                        .buttonStyle(.plain)
                        .foregroundColor(.anchorGray)
                }
                Button {
                    showingImporter = true
                } label: {
                    Label("Send file", systemImage: "plus")
                        .font(.system(size: 12, weight: .semibold))
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(!filesPlugin.isAvailable)
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 12)

            Divider().overlay(Color.white.opacity(0.08))

            if filesPlugin.transfers.isEmpty {
                Spacer()
                VStack(spacing: 12) {
                    Image("MaterialFolder")
                        .renderingMode(.template)
                        .resizable()
                        .scaledToFit()
                        .frame(width: 34, height: 34)
                        .foregroundColor(.anchorGray)
                    Text("No file transfers")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundColor(.offWhite)
                    Text("Send a file here, or drop one into the desktop's Anchor folder.")
                        .font(.system(size: 12))
                        .foregroundColor(.mediumGray)
                        .multilineTextAlignment(.center)
                }
                Spacer()
            } else {
                List(filesPlugin.transfers) { transfer in
                    FileTransferRow(transfer: transfer)
                        .listRowBackground(Color.clear)
                        .listRowSeparatorTint(Color.white.opacity(0.08))
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
            }
        }
        .background(Color.charcoalBlack)
        .fileImporter(
            isPresented: $showingImporter,
            allowedContentTypes: [.item],
            allowsMultipleSelection: false
        ) { result in
            guard case .success(let urls) = result, let url = urls.first else { return }
            filesPlugin.send(url)
        }
    }
}

private struct FileTransferRow: View {
    let transfer: FileTransferEntry

    private var fraction: Double {
        guard transfer.totalBytes > 0 else { return 0 }
        return min(1, Double(transfer.transferredBytes) / Double(transfer.totalBytes))
    }

    private var statusText: String {
        switch transfer.status {
        case .transferring:
            return transfer.direction == .sent ? "Sending" : "Receiving"
        case .completed:
            return transfer.direction == .sent ? "Sent" : "Received"
        case .failed(let message):
            return message
        }
    }

    var body: some View {
        HStack(spacing: 12) {
            Image("MaterialFolder")
                .renderingMode(.template)
                .resizable()
                .scaledToFit()
                .frame(width: 21, height: 21)
                .foregroundColor(.anchorGray)

            VStack(alignment: .leading, spacing: 5) {
                Text(transfer.name)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(.offWhite)
                    .lineLimit(1)
                HStack(spacing: 6) {
                    Text(statusText)
                    Text("·")
                    Text(ByteCountFormatter.string(fromByteCount: Int64(transfer.totalBytes), countStyle: .file))
                }
                .font(.system(size: 11))
                .foregroundColor(.mediumGray)
                if case .transferring = transfer.status {
                    ProgressView(value: fraction)
                        .tint(.anchorGray)
                }
            }

            Spacer()

            if case .completed = transfer.status, let url = transfer.localURL {
                ShareLink(item: url) {
                    Image(systemName: "square.and.arrow.up")
                        .font(.system(size: 15))
                        .foregroundColor(.anchorGray)
                        .frame(width: 36, height: 36)
                }
            }
        }
        .padding(.vertical, 5)
    }
}
