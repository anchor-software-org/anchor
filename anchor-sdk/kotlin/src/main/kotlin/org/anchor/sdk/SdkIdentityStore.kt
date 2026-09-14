package org.anchor.sdk

import android.content.Context
import java.io.File
import java.math.BigInteger
import java.security.KeyPairGenerator
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.X509Certificate
import java.util.Date
import java.util.UUID
import javax.security.auth.x500.X500Principal
import org.bouncycastle.asn1.x500.X500Name
import org.bouncycastle.cert.X509CertificateHolder
import org.bouncycastle.cert.jcajce.JcaX509CertificateConverter
import org.bouncycastle.cert.jcajce.JcaX509v3CertificateBuilder
import org.bouncycastle.operator.jcajce.JcaContentSignerBuilder
import org.bouncycastle.openssl.jcajce.JcaPEMWriter
import org.bouncycastle.jce.provider.BouncyCastleProvider

data class SdkIdentity(
    val certificatePemPath: String,
    val privateKeyPemPath: String,
    val certificateFingerprint: ByteArray,
)

/** Creates and persists one self-signed SDK identity per app installation. */
class SdkIdentityStore(context: Context) {
    private val directory = File(context.filesDir, "sdk-identity")
    private val certificate = File(directory, "client-cert.pem")
    private val privateKey = File(directory, "client-key.pem")

    @Synchronized
    fun ensure(): SdkIdentity {
        directory.mkdirs()
        if (certificate.isFile && privateKey.isFile) {
            return SdkIdentity(certificate.path, privateKey.path, fingerprint(certificate))
        }

        // Android ships a provider named "BC" of its own.  Passing an explicit
        // provider instance avoids resolving that compatibility provider and,
        // importantly, makes the private-key encoding include the P-256 curve
        // parameters.  MsQuic/OpenSSL cannot load Android's SEC1 key encoding
        // when those parameters are omitted.
        val bc = BouncyCastleProvider()
        val generator = KeyPairGenerator.getInstance("EC", bc)
        generator.initialize(java.security.spec.ECGenParameterSpec("secp256r1"), SecureRandom())
        val keyPair = generator.generateKeyPair()
        val now = System.currentTimeMillis()
        val subject = X500Name(X500Principal("CN=anchor-sdk-${UUID.randomUUID()}").name)
        val holder: X509CertificateHolder = JcaX509v3CertificateBuilder(
            subject,
            BigInteger(160, SecureRandom()).abs(),
            Date(now - 60_000),
            Date(now + 365L * 24 * 60 * 60 * 1_000),
            subject,
            keyPair.public,
        ).build(JcaContentSignerBuilder("SHA256withECDSA").setProvider(bc).build(keyPair.private))
        val x509: X509Certificate = JcaX509CertificateConverter().setProvider(bc).getCertificate(holder)
        x509.verify(keyPair.public)

        val certTemp = File(directory, "client-cert.pem.tmp")
        val keyTemp = File(directory, "client-key.pem.tmp")
        JcaPEMWriter(certTemp.writer()).use { it.writeObject(x509) }
        JcaPEMWriter(keyTemp.writer()).use { it.writeObject(keyPair.private) }
        check(certTemp.renameTo(certificate)) { "could not persist SDK certificate" }
        check(keyTemp.renameTo(privateKey)) { "could not persist SDK private key" }
        return SdkIdentity(certificate.path, privateKey.path, x509.encoded.sha256())
    }

    private fun fingerprint(file: File): ByteArray {
        val cert = file.inputStream().use {
            java.security.cert.CertificateFactory.getInstance("X.509").generateCertificate(it)
        }
        return cert.encoded.sha256()
    }
}

private fun ByteArray.sha256(): ByteArray = MessageDigest.getInstance("SHA-256").digest(this)
