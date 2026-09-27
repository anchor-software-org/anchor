import Combine
import Foundation

/// Discovers Anchor desktops by probing broadcast-capable interfaces —
/// mirrors Android's `WiredDiscovery`. Sends `ANCHOR_PROBE_V1` to each
/// interface's subnet broadcast on UDP 5028; the desktop answers
/// `ANCHOR_HERE_V1\n` + identity JSON (anchor-desktop `device/probe.rs`).
/// Since this app has no mDNS discovery, every broadcast-capable interface is
/// probed and replies from tethered interfaces are marked `wired`.
@MainActor
final class WiredDiscovery: ObservableObject {

    /// A desktop that answered a probe on some local interface.
    struct DiscoveredDesktop: Identifiable, Equatable, Sendable {
        let deviceId: String
        let deviceName: String
        /// Reply source address — never an address claimed inside the JSON.
        let ip: String
        let port: Int
        let certificateDer: Data?
        /// True when the reply arrived over a tethered interface.
        let wired: Bool

        var id: String { deviceId }
    }

    @Published private(set) var devices: [DiscoveredDesktop] = []

    /// True while a tethered-style interface exists, whether or not a desktop
    /// has answered yet.
    @Published private(set) var tetherActive = false

    private var task: Task<Void, Never>?
    private var seen: [String: (device: DiscoveredDesktop, lastSeen: Date)] = [:]

    func start() {
        guard task == nil else { return }
        devices = []
        seen.removeAll()
        task = Task.detached(priority: .utility) { [weak self] in
            while !Task.isCancelled {
                let interfaces = WiredProbe.probeInterfaces()
                let found = interfaces.isEmpty ? [] : WiredProbe.probe(interfaces)
                guard let self else { return }
                await self.ingest(interfaces: interfaces, found: found)
                try? await Task.sleep(nanoseconds: WiredProbe.probeIntervalNs)
            }
        }
    }

    func stop() {
        task?.cancel()
        task = nil
        seen.removeAll()
        devices = []
        tetherActive = false
    }

    private func ingest(interfaces: [WiredProbe.ProbeInterface], found: [DiscoveredDesktop]) {
        tetherActive = interfaces.contains { WiredProbe.isTetheredInterfaceName($0.name) }
        let now = Date()
        for device in found {
            // A device answering on both wired and Wi-Fi collapses to the
            // wired path for new connections.
            if let existing = seen[device.deviceId], existing.device.wired, !device.wired {
                seen[device.deviceId] = (existing.device, now)
            } else {
                seen[device.deviceId] = (device, now)
            }
        }
        seen = seen.filter { now.timeIntervalSince($0.value.lastSeen) < Self.deviceExpirySeconds }
        devices = seen.values.map(\.device).sorted { $0.deviceName < $1.deviceName }
    }

    private static let deviceExpirySeconds: TimeInterval = 9
}

/// Pure helpers, split from the observable class for unit tests —
/// same split as Android's `WiredProbe`.
enum WiredProbe {

    static let probePort: UInt16 = 5028
    static let probeRequest = Data("ANCHOR_PROBE_V1".utf8)
    static let probeResponsePrefix = Data("ANCHOR_HERE_V1\n".utf8)
    static let replyWindowSeconds: TimeInterval = 0.7
    static let probeIntervalNs: UInt64 = 2_000_000_000

    /// An interface worth probing: up, broadcast-capable, with an IPv4 address.
    struct ProbeInterface: Sendable {
        let name: String
        let address: in_addr
        let broadcast: in_addr
    }

    /// Interfaces that never carry a desktop: cellular, tunnels, loopback,
    /// and Apple's peer-to-peer side channels.
    private static let excludedPrefixes = ["lo", "pdp_ip", "utun", "ipsec", "awdl", "llw", "gif", "stf", "anpi", "ap"]

    /// Personal Hotspot shows up as `bridgeN`; USB ethernet and Mac internet
    /// sharing appear as `en2` and up.
    static func isTetheredInterfaceName(_ name: String) -> Bool {
        let lower = name.lowercased()
        for prefix in ["bridge", "usb", "eth", "ncm", "rndis"] where lower.hasPrefix(prefix) {
            return true
        }
        if lower.hasPrefix("en"), let index = Int(lower.dropFirst(2)), index >= 2 {
            return true
        }
        return false
    }

    /// Up, non-loopback, IPv4 interfaces not on the exclusion list. The subnet
    /// broadcast comes from `ifa_dstaddr` when flagged, else from the mask.
    static func probeInterfaces() -> [ProbeInterface] {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let head = list else { return [] }
        defer { freeifaddrs(list) }

        var interfaces: [ProbeInterface] = []
        for entry in sequence(first: head, next: { $0.pointee.ifa_next }) {
            let name = String(cString: entry.pointee.ifa_name)
            let flags = entry.pointee.ifa_flags
            guard flags & UInt32(IFF_UP) != 0,
                  flags & UInt32(IFF_LOOPBACK) == 0,
                  flags & UInt32(IFF_RUNNING) != 0,
                  !excludedPrefixes.contains(where: { name.hasPrefix($0) }),
                  let addr = entry.pointee.ifa_addr,
                  addr.pointee.sa_family == sa_family_t(AF_INET)
            else { continue }

            let ipv4 = addr.withMemoryRebound(to: sockaddr_in.self, capacity: 1) {
                $0.pointee.sin_addr
            }
            let broadcast: in_addr
            if flags & UInt32(IFF_BROADCAST) != 0, let dst = entry.pointee.ifa_dstaddr,
               dst.pointee.sa_family == sa_family_t(AF_INET) {
                broadcast = dst.withMemoryRebound(to: sockaddr_in.self, capacity: 1) {
                    $0.pointee.sin_addr
                }
            } else if let mask = entry.pointee.ifa_netmask,
                      mask.pointee.sa_family == sa_family_t(AF_INET) {
                let maskAddr = mask.withMemoryRebound(to: sockaddr_in.self, capacity: 1) {
                    $0.pointee.sin_addr
                }
                guard let computed = computeBroadcast(address: ipv4, netmask: maskAddr) else { continue }
                broadcast = computed
            } else {
                continue
            }
            interfaces.append(ProbeInterface(name: name, address: ipv4, broadcast: broadcast))
        }
        return interfaces
    }

    /// `address | ~mask`. `in_addr` is already network-ordered, which bitwise
    /// OR does not care about.
    static func computeBroadcast(address: in_addr, netmask: in_addr) -> in_addr? {
        let broadcast = in_addr(s_addr: address.s_addr | ~netmask.s_addr)
        return broadcast.s_addr == address.s_addr ? nil : broadcast
    }

    /// Parse a probe reply, or nil if malformed. The IP always comes from the
    /// packet's source — a reply cannot claim to live at another address.
    static func parseResponse(_ data: Data, sourceIp: String, wired: Bool)
        -> WiredDiscovery.DiscoveredDesktop?
    {
        guard data.starts(with: probeResponsePrefix),
              let json = try? JSONSerialization.jsonObject(
                  with: data.dropFirst(probeResponsePrefix.count)
              ) as? [String: Any]
        else { return nil }

        guard let deviceId = (json["device_id"] as? String),
              !deviceId.isEmpty, deviceId.count <= 128,
              let port = (json["port"] as? Int), (1...65535).contains(port)
        else { return nil }

        let name = (json["device_name"] as? String).flatMap {
            $0.isEmpty || $0.count > 128 ? nil : $0
        } ?? sourceIp

        let certificateDer = (json["certificate"] as? String).flatMap(base64UrlDecode)
            .flatMap { $0.isEmpty || $0.count > 4096 ? nil : $0 }

        return WiredDiscovery.DiscoveredDesktop(
            deviceId: deviceId,
            deviceName: name,
            ip: sourceIp,
            port: port,
            certificateDer: certificateDer,
            wired: wired
        )
    }

    /// Desktop encodes the certificate as base64url without padding.
    static func base64UrlDecode(_ encoded: String) -> Data? {
        var std = encoded.replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let remainder = std.count % 4
        if remainder != 0 {
            std.append(String(repeating: "=", count: 4 - remainder))
        }
        return Data(base64Encoded: std)
    }

    /// Send one probe per interface to its subnet broadcast, then collect
    /// replies for a bounded window. Each socket binds to the interface's own
    /// IPv4 address so replies are attributed to the right link.
    static func probe(_ interfaces: [ProbeInterface]) -> [WiredDiscovery.DiscoveredDesktop] {
        var sockets: [(fd: Int32, interface: ProbeInterface)] = []
        defer { sockets.forEach { close($0.fd) } }

        for interface in interfaces {
            let fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
            guard fd >= 0 else { continue }
            var yes: Int32 = 1
            setsockopt(fd, SOL_SOCKET, SO_BROADCAST, &yes, socklen_t(MemoryLayout<Int32>.size))

            var src = sockaddr_in()
            src.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
            src.sin_family = sa_family_t(AF_INET)
            src.sin_port = 0
            src.sin_addr = interface.address
            let bound = withUnsafePointer(to: &src) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
            guard bound == 0 else {
                close(fd)
                continue
            }

            var dst = sockaddr_in()
            dst.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
            dst.sin_family = sa_family_t(AF_INET)
            dst.sin_port = probePort.bigEndian
            dst.sin_addr = interface.broadcast
            let sent = probeRequest.withUnsafeBytes { bytes in
                withUnsafePointer(to: &dst) {
                    $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                        sendto(fd, bytes.baseAddress, bytes.count, 0, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                    }
                }
            }
            if sent >= 0 {
                sockets.append((fd: fd, interface: interface))
            } else {
                close(fd)
            }
        }
        guard !sockets.isEmpty else { return [] }

        var found: [String: WiredDiscovery.DiscoveredDesktop] = [:]
        var pollfds = sockets.map { pollfd(fd: $0.fd, events: Int16(POLLIN), revents: 0) }
        let deadline = Date().addingTimeInterval(replyWindowSeconds)
        while true {
            let remaining = deadline.timeIntervalSinceNow
            guard remaining > 0 else { break }
            let ready = poll(&pollfds, nfds_t(pollfds.count), Int32((remaining * 1000).rounded(.up)))
            guard ready > 0 else { break }
            for (index, descriptor) in pollfds.enumerated() where descriptor.revents & Int16(POLLIN) != 0 {
                var buffer = [UInt8](repeating: 0, count: 8192)
                var source = sockaddr_in()
                var sourceLength = socklen_t(MemoryLayout<sockaddr_in>.size)
                let received = withUnsafeMutablePointer(to: &source) {
                    $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                        recvfrom(descriptor.fd, &buffer, buffer.count, 0, $0, &sourceLength)
                    }
                }
                guard received > 0 else { continue }
                let sourceIp = String(cString: inet_ntoa(source.sin_addr))
                let wired = WiredProbe.isTetheredInterfaceName(sockets[index].interface.name)
                guard let device = parseResponse(
                    Data(buffer[..<received]), sourceIp: sourceIp, wired: wired
                ) else { continue }
                NSLog("[anchor] probe reply from %@:%ld on %@ (id=%@)", sourceIp, device.port,
                      sockets[index].interface.name, device.deviceId)
                if let existing = found[device.deviceId], existing.wired, !device.wired {
                    continue
                }
                found[device.deviceId] = device
            }
        }
        return Array(found.values)
    }
}
