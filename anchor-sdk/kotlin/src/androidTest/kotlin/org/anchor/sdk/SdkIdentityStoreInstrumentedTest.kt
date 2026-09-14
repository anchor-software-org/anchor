package org.anchor.sdk

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.security.cert.CertificateFactory
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.bouncycastle.asn1.sec.ECPrivateKey
import org.bouncycastle.openssl.PEMParser

@RunWith(AndroidJUnit4::class)
class SdkIdentityStoreInstrumentedTest {
    @Test
    fun provisionsAndReusesAnAppPrivateIdentity() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        File(context.filesDir, "sdk-identity").deleteRecursively()
        val store = SdkIdentityStore(context)

        val first = store.ensure()
        val certificate = File(first.certificatePemPath)
        val privateKey = File(first.privateKeyPemPath)
        assertTrue(certificate.isFile)
        assertTrue(privateKey.isFile)
        assertTrue(certificate.readText().contains("BEGIN CERTIFICATE"))
        assertTrue(privateKey.readText().contains("PRIVATE KEY"))
        CertificateFactory.getInstance("X.509").generateCertificate(certificate.inputStream())

        // MsQuic/OpenSSL needs the named P-256 curve parameters in the SEC1
        // private-key object. Android's older provider emitted a key without
        // them, which Android accepted but MsQuic rejected at configuration
        // load time. Parse the actual PEM/ASN.1 here so that regression is
        // caught before a device connection test.
        val pem = requireNotNull(PEMParser(privateKey.reader()).use { it.readPemObject() })
        assertEquals("EC PRIVATE KEY", pem.type)
        assertNotNull(ECPrivateKey.getInstance(pem.content).parameters)

        val second = store.ensure()
        assertArrayEquals(first.certificateFingerprint, second.certificateFingerprint)
        assertTrue(first.certificatePemPath == second.certificatePemPath)
    }
}
