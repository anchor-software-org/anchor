package com.anchor.data

/**
 * Pure decision logic for choosing a single auto-connect/reconnect target.
 *
 * Kept free of Android dependencies so every branch can be unit-tested. The
 * ViewModel feeds it the current mDNS results, the set of saved device IDs, and
 * the last-connected device id/IP, and connects to whatever IP it returns.
 *
 * Policy (highest priority first):
 *  1. The last-connected device, if it's visible on mDNS now → its fresh IP.
 *  2. Otherwise any *other* saved device currently visible on mDNS → its IP.
 *  3. Otherwise the last-connected device's stored IP (a blind attempt).
 *  4. Otherwise nothing to do.
 */
object ReconnectPolicy {

    /**
     * Select the trusted desktop identity for an address entered manually.
     *
     * A host/IP alone does not carry the desktop identity required for
     * certificate-pinned QUIC. Prefer an exact known address, then the last
     * connected desktop (so a user can enter its alternate LAN or Tailscale
     * address), and finally the sole saved desktop. Returning null means this
     * is a new desktop and must go through the pairing-only flow.
     */
    fun manualAddressDeviceId(
        address: String,
        discovered: List<DiscoveredDevice>,
        savedDeviceLastIps: Map<String, String?>,
        lastConnectedDeviceId: String?,
    ): String? {
        discovered.firstOrNull {
            it.ip == address && it.deviceId != null && it.deviceId in savedDeviceLastIps
        }?.deviceId?.let { return it }

        savedDeviceLastIps.entries.firstOrNull { (_, lastIp) -> lastIp == address }
            ?.key?.let { return it }

        if (lastConnectedDeviceId in savedDeviceLastIps) return lastConnectedDeviceId
        return savedDeviceLastIps.keys.singleOrNull()
    }

    fun pickTarget(
        discovered: List<DiscoveredDevice>,
        savedDeviceIds: Set<String>,
        lastConnectedDeviceId: String?,
        lastConnectedIp: String?
    ): String? {
        // A remembered address is only meaningful while its corresponding
        // desktop is still trusted. Without this guard, unpairing leaves an
        // old address eligible for background retries, which can race a new
        // pairing flow.
        val trustedLastDeviceId = lastConnectedDeviceId?.takeIf { it in savedDeviceIds }

        // 1. Last trusted device, seen on mDNS now → use its fresh (possibly changed) IP.
        if (trustedLastDeviceId != null) {
            discovered.firstOrNull { it.deviceId == trustedLastDeviceId && it.ip.isNotEmpty() }
                ?.let { return it.ip }
        }

        // 2. Any other saved device currently on mDNS. mDNS presence means we
        //    know it's reachable, so this beats a blind stored-IP attempt.
        discovered.firstOrNull {
            it.deviceId != null && it.deviceId in savedDeviceIds && it.ip.isNotEmpty()
        }?.let { return it.ip }

        // 3. Blind attempt at the last device's stored IP.
        return lastConnectedIp?.takeIf { trustedLastDeviceId != null && it.isNotEmpty() }
    }
}
