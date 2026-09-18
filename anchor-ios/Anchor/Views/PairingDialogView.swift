import SwiftUI

/// Pairing request sheet — mirrors Android's PairingDialog.
struct PairingDialogView: View {
    @ObservedObject var viewModel: MainViewModel

    private var request: (deviceId: String, deviceName: String, deviceType: String, fingerprint: String)? {
        if case .requested(let id, let name, let type, let fp, _) = viewModel.pairingState {
            return (id, name, type, fp)
        }
        return nil
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) {
                    Image(systemName: "lock.shield")
                        .font(.system(size: 40))
                        .foregroundColor(.anchorSea)

                    Text("Pairing Request")
                        .font(.title3.bold())

                    Text("A new desktop wants to pair with this device.")
                        .font(.subheadline)
                        .multilineTextAlignment(.center)
                        .foregroundColor(.secondary)

                    if let req = request {
                        VStack(alignment: .leading, spacing: 10) {
                            InfoRow(label: "Name", value: req.deviceName)
                            InfoRow(label: "Type", value: req.deviceType)

                            VStack(alignment: .leading, spacing: 4) {
                                Text("Fingerprint")
                                    .font(.caption)
                                    .foregroundColor(.secondary)
                                Text(req.fingerprint)
                                    .font(.system(.caption2, design: .monospaced))
                                    .lineLimit(nil)
                                    .fixedSize(horizontal: false, vertical: true)
                                    .padding(8)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .background(Color(.secondarySystemBackground))
                                    .cornerRadius(6)
                            }
                        }
                        .padding(12)
                        .background(Color(.tertiarySystemBackground))
                        .cornerRadius(12)
                    }

                    Text("Verify this fingerprint matches what is shown on the desktop.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .multilineTextAlignment(.center)

                    HStack(spacing: 16) {
                        Button {
                            viewModel.respondToPairing(accepted: false)
                        } label: {
                            Text("Reject")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.bordered)

                        Button {
                            viewModel.respondToPairing(accepted: true)
                        } label: {
                            Text("Accept")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.borderedProminent)
                    }
                    .padding(.top, 4)
                }
                .padding()
            }
        }
        .presentationDetents([.medium, .large])
    }
}

private struct InfoRow: View {
    let label: String
    let value: String

    var body: some View {
        HStack {
            Text(label)
                .foregroundColor(.secondary)
            Spacer()
            Text(value)
        }
    }
}

#if DEBUG
struct PairingDialogView_Previews: PreviewProvider {
    static var previews: some View {
        PairingDialogView(viewModel: .previewPairingRequest())
            .preferredColorScheme(.dark)
    }
}
#endif
