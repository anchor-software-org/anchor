# BouncyCastleProvider discovers algorithm mappings and implementations by
# class name. SdkIdentityStore needs EC key generation/signing and X.509
# conversion even when the consuming app is minified (including F-Droid).
-keep class org.bouncycastle.jcajce.provider.asymmetric.EC$Mappings { *; }
-keep class org.bouncycastle.jcajce.provider.asymmetric.ec.** { *; }
-keep class org.bouncycastle.jcajce.provider.asymmetric.X509$Mappings { *; }
-keep class org.bouncycastle.jcajce.provider.asymmetric.x509.** { *; }
