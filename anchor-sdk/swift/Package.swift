// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "AnchorSDK",
    platforms: [
        .iOS(.v17),
        .macOS(.v14),
    ],
    products: [
        .library(name: "AnchorSDK", targets: ["AnchorSDK"]),
    ],
    targets: [
        .target(name: "AnchorSDK"),
        .testTarget(name: "AnchorSDKTests", dependencies: ["AnchorSDK"]),
    ]
)
