import SwiftUI

/// Maritime color palette — mirrors Android's AnchorTheme.
extension Color {
    // Primary
    static let anchorBlue40 = Color(hex: 0x42A5F5)
    static let deepOcean = Color(hex: 0x0277BD)
    static let seaGreen40 = Color(hex: 0x26A69A)
    static let coralRed40 = Color(hex: 0xEF5350)
    static let seaFoam = Color(hex: 0x4DB6AC)
    static let lighthouse = Color(hex: 0xFF7043)

    // Neutrals
    static let charcoalBlack = Color(hex: 0x212121)
    static let darkGray = Color(hex: 0x424242)
    static let mediumGray = Color(hex: 0x9E9E9E)
    static let offWhite = Color(hex: 0xFAFAFA)
    static let anchorGray = Color(hex: 0x607D8B)

    // Legacy aliases
    static let anchorNavy = Color(red: 0.10, green: 0.15, blue: 0.25)
    static let anchorBlue = Color(red: 0.20, green: 0.40, blue: 0.65)
    static let anchorSea = Color(red: 0.30, green: 0.60, blue: 0.75)
    static let anchorGreen = Color(hex: 0x26A69A)
    static let anchorRed = Color(hex: 0xEF5350)
    static let anchorYellow = Color(red: 0.95, green: 0.75, blue: 0.20)
    static let anchorSurface = Color(hex: 0x1A1F24)
    static let anchorSurfaceLight = Color(hex: 0x2A3A4A)
}

extension Color {
    init(hex: UInt32) {
        self.init(
            red: Double((hex >> 16) & 0xFF) / 255.0,
            green: Double((hex >> 8) & 0xFF) / 255.0,
            blue: Double(hex & 0xFF) / 255.0
        )
    }
}
