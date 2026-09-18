package org.anchor.sdk

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class FilesProtocolTest {
    @Test
    fun offerAndDecisionPreserveTransferIdentity() {
        val transferId = ByteArray(16) { 7 }
        val hash = ByteArray(32) { 9 }
        val offer = FilesProtocol.decodeOffer(
            FilesProtocol.encodeOffer(transferId, "photo.jpg", "image/jpeg", 1234, hash),
        )
        assertArrayEquals(transferId, offer.transferId)
        assertEquals(1234, offer.byteLength)
        assertArrayEquals(hash, offer.sha256)

        val decision = FilesProtocol.decodeDecision(FilesProtocol.encodeDecision(transferId, accepted = true))
        assertArrayEquals(transferId, decision.transferId)
        assertTrue(decision.accepted)
    }
}
