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
    dependencies: [
        .package(url: "https://github.com/apple/swift-protobuf.git", from: "1.38.0"),
    ],
    targets: [
        .target(
            name: "AnchorSDK",
            dependencies: [
                .product(name: "SwiftProtobuf", package: "swift-protobuf"),
            ]
        ),
        .testTarget(name: "AnchorSDKTests", dependencies: ["AnchorSDK"]),
    ]
)
