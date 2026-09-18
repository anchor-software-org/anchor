import XCTest
@testable import Anchor

final class AnchorTests: XCTestCase {

    func testTrustedStoreFingerprint() {
        // Simple test: SHA-256 of known data should be deterministic
        let data = Data("test certificate data".utf8)
        let fingerprint = TrustedStore.fingerprint(data)
        XCTAssertFalse(fingerprint.isEmpty)
        XCTAssertTrue(fingerprint.contains(":"))

        // Same input should produce same fingerprint
        let fingerprint2 = TrustedStore.fingerprint(data)
        XCTAssertEqual(fingerprint, fingerprint2)
    }

    func testConnectionStateDefaults() {
        let state = ConnectionState()
        XCTAssertEqual(state.status, .disconnected)
        XCTAssertTrue(state.host.isEmpty)
        XCTAssertEqual(state.port, 0)
        XCTAssertNil(state.error)
    }

    func testPairingStateIdle() {
        let state: PairingState = .idle
        if case .idle = state {
            // pass
        } else {
            XCTFail("Expected idle state")
        }
    }
}
