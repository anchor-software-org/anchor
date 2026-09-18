package com.anchor.data

/**
 * Pure logic that merges saved (trusted) devices with live mDNS discovery
 * results into the single ordered list shown on the disconnected screen.
 *
 * Android-free so it can be unit-tested directly.
 *
 * Rules:
 *  - Every saved device appears, marked online if a discovered entry matches it
 *    by device_id. Online saved devices use their fresh mDNS IP; offline ones
 *    fall back to their stored last IP (or empty if never recorded).
 *  - Discovered devices that don't match any saved device appear as unpaired
 *    ("new") entries — always online (they only exist because mDNS saw them).
 *  - Ordering: online first, then paired, then by name (case-insensitive).
 */
object DeviceListMerger {

    fun merge(
        saved: List<TrustedDeviceEntry>,
        discovered: List<DiscoveredDevice>
    ): List<DeviceListEntry> {
        val matchedIds = mutableSetOf<String>()

        val savedEntries = saved.map { entry ->
            val online = discovered.firstOrNull { it.deviceId == entry.deviceId }
            if (online != null) matchedIds.add(entry.deviceId)
            DeviceListEntry(
                deviceId = entry.deviceId,
                // Replace the legacy generic label with the hostname mDNS
                // advertises, but do not override a saved user-facing name.
                name = online?.name?.takeIf {
                    entry.deviceName == "Anchor Desktop" && it.isNotBlank()
                } ?: entry.deviceName,
                ip = online?.ip ?: entry.lastIp ?: "",
                isPaired = true,
                isOnline = online != null,
                port = online?.port ?: 5027,
                certificateDer = online?.certificateDer,
            )
        }

        val unpaired = discovered
            .filter { it.deviceId == null || it.deviceId !in matchedIds }
            .map { d ->
                DeviceListEntry(
                    deviceId = d.deviceId,
                    name = d.name,
                    ip = d.ip,
                    isPaired = false,
                    isOnline = true,
                    port = d.port,
                    certificateDer = d.certificateDer,
                )
            }

        return (savedEntries + unpaired).sortedWith(
            compareByDescending<DeviceListEntry> { it.isOnline }
                .thenByDescending { it.isPaired }
                .thenBy { it.name.lowercase() }
        )
    }
}
