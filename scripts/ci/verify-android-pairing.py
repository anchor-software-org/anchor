#!/usr/bin/env python3
"""Check the packaged DEX classes that BC loads reflectively for pairing.

Debug unit tests do not run R8. Check the minified release APK itself so a
missing SDK consumer rule cannot silently break fresh-install enrollment.
"""

import struct
import sys
import zipfile


def defined_classes(dex):
    if not dex.startswith(b"dex\n"):
        raise ValueError("unsupported DEX format")
    string_count, string_offset = struct.unpack_from("<II", dex, 56)
    strings = []
    for index in range(string_count):
        offset = struct.unpack_from("<I", dex, string_offset + index * 4)[0]
        # Skip the ULEB128 UTF-16 length; descriptors below contain only ASCII.
        while dex[offset] & 0x80:
            offset += 1
        offset += 1
        strings.append(dex[offset:dex.index(b"\0", offset)].decode("utf-8", errors="replace"))
    type_count, type_offset = struct.unpack_from("<II", dex, 64)
    types = [strings[struct.unpack_from("<I", dex, type_offset + index * 4)[0]]
             for index in range(type_count)]
    class_count, class_offset = struct.unpack_from("<II", dex, 96)
    return {types[struct.unpack_from("<I", dex, class_offset + index * 32)[0]]
            for index in range(class_count)}


def main():
    if len(sys.argv) != 2:
        sys.exit("Usage: verify-android-pairing.py <apk>")
    apk = sys.argv[1]
    classes = set()
    with zipfile.ZipFile(apk) as archive:
        for name in archive.namelist():
            if name.startswith("classes") and name.endswith(".dex"):
                classes.update(defined_classes(archive.read(name)))
    required = {
        "org.bouncycastle.jcajce.provider.asymmetric.EC$Mappings",
        "org.bouncycastle.jcajce.provider.asymmetric.ec.KeyPairGeneratorSpi$EC",
        "org.bouncycastle.jcajce.provider.asymmetric.ec.SignatureSpi$ecDSA256",
        "org.bouncycastle.jcajce.provider.asymmetric.X509$Mappings",
        "org.bouncycastle.jcajce.provider.asymmetric.x509.CertificateFactory",
    }
    missing = sorted(name for name in required if "L" + name.replace(".", "/") + ";" not in classes)
    if missing:
        sys.exit("Pairing identity algorithms missing or renamed in " + apk + ":\n  "
                 + "\n  ".join(missing) + "\nCheck anchorSdk's consumer ProGuard rules.")
    print(f"Pairing identity provider classes verified in {apk}")


if __name__ == "__main__":
    main()
