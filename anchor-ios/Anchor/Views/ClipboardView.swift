import SwiftUI

struct ClipboardView: View {
    @ObservedObject var clipboardPlugin: ClipboardPlugin

    var body: some View {
        VStack(spacing: 0) {
            if let notice = clipboardPlugin.notice {
                Text(notice)
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 8)
            }

            // History
            if clipboardPlugin.history.isEmpty {
                Spacer()
                VStack(spacing: 12) {
                    Image("AndroidClipboard")
                        .renderingMode(.template)
                        .resizable()
                        .scaledToFit()
                        .frame(width: 44, height: 44)
                        .foregroundColor(.anchorGray.opacity(0.4))
                    Text("No clipboard history yet")
                        .font(.system(size: 14))
                        .foregroundColor(.anchorGray)
                    Text("Copy text on your desktop or tap Send above")
                        .font(.system(size: 12))
                        .foregroundColor(.anchorGray.opacity(0.6))
                }
                Spacer()
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(spacing: 8) {
                            ForEach(clipboardPlugin.history) { entry in
                                ClipboardHistoryCard(entry: entry, onCopy: clipboardPlugin.copyToLocalClipboard)
                                    .id(entry.id)
                            }
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                    }
                    .onChange(of: clipboardPlugin.history.count) {
                        if let first = clipboardPlugin.history.first {
                            withAnimation {
                                proxy.scrollTo(first.id, anchor: .top)
                            }
                        }
                    }
                }
            }
        }
        .background(Color.charcoalBlack)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            HStack(spacing: 12) {
                if !clipboardPlugin.history.isEmpty {
                    Button("Clear") { clipboardPlugin.clearHistory() }
                        .foregroundColor(.anchorGray)
                }
                Spacer()
                Button(action: { clipboardPlugin.sendCurrentClipboard() }) {
                    HStack(spacing: 8) {
                        Image(systemName: "paperplane.fill")
                        Text("Send to Desktop")
                    }
                    .font(.system(size: 14, weight: .medium))
                    .padding(.horizontal, 20)
                    .frame(height: 44)
                    .background(Color.darkGray)
                    .foregroundColor(.offWhite)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .background(Color.charcoalBlack)
            .overlay(alignment: .top) {
                Rectangle().fill(Color.white.opacity(0.08)).frame(height: 1)
            }
        }
    }
}

struct ClipboardHistoryCard: View {
    let entry: ClipboardHistoryEntry
    let onCopy: (ClipboardHistoryEntry) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Header row: direction and quiet metadata.
            HStack {
                Text(entry.source == "local" ? "Sent" : "Received")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundColor(.anchorGray)

                Spacer()

                // Size · compressed · time
                HStack(spacing: 5) {
                    Text(formatSize(entry.sizeBytes))
                        .font(.system(size: 10))
                        .foregroundColor(.anchorGray)
                    if entry.compressed {
                        Text("·")
                            .font(.system(size: 10))
                            .foregroundColor(.anchorGray.opacity(0.5))
                        Text("compressed")
                            .font(.system(size: 10))
                            .foregroundColor(.anchorGray)
                    }
                    Text("·")
                        .font(.system(size: 10))
                        .foregroundColor(.anchorGray.opacity(0.5))
                    Text(entry.timestamp, style: .time)
                        .font(.system(size: 10))
                        .foregroundColor(.anchorGray)
                }
            }

            // Content: image or text
            if entry.isImage, let imageData = entry.imageData, let uiImage = UIImage(data: imageData) {
                SwiftUI.Image(uiImage: uiImage)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(maxHeight: 200)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
            } else if let text = entry.text {
                Text(text)
                    .font(.system(size: 13))
                    .foregroundColor(.offWhite.opacity(0.9))
                    .lineLimit(4)
                    .lineSpacing(3)
            }

            Text(entry.isImage ? "Tap to copy image" : "Tap to copy")
                .font(.system(size: 10))
                .foregroundColor(.anchorGray.opacity(0.5))
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.darkGray.opacity(0.55))
        .onTapGesture {
            onCopy(entry)
        }
    }

    private func formatSize(_ bytes: Int) -> String {
        if bytes < 1024 { return "\(bytes)B" }
        if bytes < 1024 * 1024 { return "\(bytes / 1024)KB" }
        return String(format: "%.1fMB", Double(bytes) / (1024.0 * 1024.0))
    }
}

#if DEBUG
struct ClipboardView_Previews: PreviewProvider {
    static var previews: some View {
        Group {
            ScreenWithTopBar(title: "Clipboard", onMenu: {}) {
                ClipboardView(clipboardPlugin: .preview(history: ClipboardHistoryEntry.previewSamples))
            }
            .preferredColorScheme(.dark)
            .previewDisplayName("Clipboard Filled")

            ScreenWithTopBar(title: "Clipboard", onMenu: {}) {
                ClipboardView(clipboardPlugin: .preview())
            }
            .preferredColorScheme(.dark)
            .previewDisplayName("Clipboard Empty")
        }
    }
}
#endif
