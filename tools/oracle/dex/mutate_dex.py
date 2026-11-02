#!/usr/bin/env python3
"""WP 6.4 oracle helper: mutate copies of a fixture APK's first dex entry.

Usage: mutate_dex.py <in.apk> <out.apk> <magic040|superclass-self|superclass-noindex|utf8>
The committed fixtures are never touched; outputs go to the oracle build dir.
"""
import struct
import sys
import zipfile


# utf8: the byte sequences patched over Utf8ProbeTest's placeholders. The ED
# sequences with a second byte in A0..BF are where the JDK's decode differs
# from Rust's from_utf8_lossy (an encoded surrogate, complete or truncated, is
# ONE U+FFFD); the others are cases where the two agree.
UTF8_CASES = [
    "eda0b4edb49e", "eda080", "eda041", "eda0", "edbfbf", "eda0b4", "ed41",
    "eda0b441", "ed9fbf", "f09d849e", "f4", "c080", "e08080", "e0a080",
    "f0808080", "f4908080", "f8888080", "80", "c2", "e282", "e2827a",
    "f09d84", "f09d847a", "c1bf", "ff41", "efbfbd", "e2", "f09d41", "f09d",
    "f5", "c241", "e080", "f490", "ed",
]


def patch(dex: bytearray, placeholder: bytes, replacement: bytes) -> None:
    assert len(placeholder) == len(replacement) and 0 not in replacement
    # The terminating NUL pins the placeholder to the end of its string.
    at = dex.find(placeholder + b"\0")
    assert at >= 0 and dex.find(placeholder + b"\0", at + 1) < 0, placeholder
    dex[at:at + len(placeholder)] = replacement


def main() -> int:
    src, dst, mode = sys.argv[1], sys.argv[2], sys.argv[3]
    zin = zipfile.ZipFile(src)
    names = zin.namelist()
    dex_name = next(n for n in names if n.endswith(".dex"))
    dex = bytearray(zin.read(dex_name))
    if mode == "magic040":
        dex[4:7] = b"040"
    elif mode == "utf8":
        for i, case in enumerate(UTF8_CASES):
            raw = bytes.fromhex(case)
            patch(dex, b"u%02d:" % i + b"_" * len(raw), b"u%02d:" % i + raw)
        patch(dex, b"s:______", b"s:" + bytes.fromhex("eda0b4edb49e"))
        patch(dex, b"utf8Nul__", b"utf8Nul" + bytes.fromhex("c080"))
        patch(dex, b"utf8Pair______", b"utf8Pair" + bytes.fromhex("eda0b4edb49e"))
    else:
        class_defs_size, class_defs_off = struct.unpack_from("<II", dex, 96)
        assert class_defs_size > 0
        # First non-interface class def (interfaces are skipped before the
        # superclass lookup): classIdx @+0, accessFlags @+4, superclassIdx @+8.
        for i in range(class_defs_size):
            off = class_defs_off + 32 * i
            access = struct.unpack_from("<I", dex, off + 4)[0]
            if access & 0x200 == 0:
                break
        else:
            raise AssertionError("no concrete class def")
        class_idx = struct.unpack_from("<I", dex, off)[0]
        value = class_idx if mode == "superclass-self" else 0xFFFFFFFF
        struct.pack_into("<I", dex, off + 8, value)
    with zipfile.ZipFile(dst, "w", zipfile.ZIP_DEFLATED) as zout:
        for name in names:
            # Fixed timestamp: regeneration must be byte-stable.
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            zout.writestr(info, bytes(dex) if name == dex_name else zin.read(name))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
