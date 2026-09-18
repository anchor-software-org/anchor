import SwiftUI

struct ClipboardView: View {
    @ObservedObject var clipboardPlugin: ClipboardPlugin

    var body: some View {
        VStack(spacing: 0) {
            // Send button
            Button(action: { clipboardPlugin.sendCurrentClipboard() }) {
                HStack(spacing: 8) {
                    Image(systemName: "paperplane.fill")
                        .font(.system(size: 14))
                    Text("Send Clipboard to Desktop")
                        .font(.system(size: 14, weight: .medium))
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 14)
                .background(Color.anchorBlue40)
                .foregroundColor(.white)
                .clipShape(RoundedRectangle(cornerRadius: 12))
            }
            .padding(.horizontal, 16)
            .padding(.top, 12)
            .padding(.bottom, 8)

            Divider().padding(.vertical, 4)

            // History
            if clipboardPlugin.history.isEmpty {
                Spacer()
                VStack(spacing: 12) {
                    Image(systemName: "doc.on.clipboard")
                        .font(.system(size: 44))
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
        .navigationTitle("Clipboard")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if !clipboardPlugin.history.isEmpty {
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button("Clear") { clipboardPlugin.clearHistory() }
                        .foregroundColor(.anchorGray)
                }
            }
        }
    }
}

struct ClipboardHistoryCard: View {
    let entry: ClipboardHistoryEntry
    let onCopy: (ClipboardHistoryEntry) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Header row: dot + Sent/Received ... size · compressed · time
            HStack {
                // Source: colored dot + label
                HStack(spacing: 5) {
                    Circle()
                        .fill(entry.source == "local" ? Color.anchorBlue40 : Color.seaGreen40)
                        .frame(width: 6, height: 6)
                    Text(entry.source == "local" ? "Sent" : "Received")
                        .font(.system(size: 11))
                        .foregroundColor(entry.source == "local" ? .anchorBlue40 : .seaGreen40)
                }

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
        .background(Color.darkGray.opacity(0.5))
        .clipShape(RoundedRectangle(cornerRadius: 10))
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
