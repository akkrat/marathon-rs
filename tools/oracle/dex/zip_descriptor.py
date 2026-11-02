#!/usr/bin/env python3
"""WP 6.4 oracle helper: rewrap an APK's classes.dex as the single entry of a
zip whose local header defers the sizes to a data descriptor (flag bit 3).

Usage: zip_descriptor.py <in.apk> <out.apk> <stored|deflated>
ZipInputStream reads a DEFLATED entry written this way but rejects a STORED
one ("only DEFLATED entries can have EXT descriptor"); the zip crate's
streaming reader rejects both, so the Rust reader goes through the central
directory for them. Fixed timestamps keep regeneration byte-stable.
"""
import struct
import sys
import zipfile
import zlib


def main() -> int:
    src, dst, mode = sys.argv[1], sys.argv[2], sys.argv[3]
    dex = zipfile.ZipFile(src).read("classes.dex")
    name = b"classes.dex"
    crc = zlib.crc32(dex) & 0xFFFFFFFF
    method = {"stored": 0, "deflated": 8}[mode]
    if method == 8:
        compressor = zlib.compressobj(9, zlib.DEFLATED, -15)
        data = compressor.compress(dex) + compressor.flush()
    else:
        data = dex
    flags, dos_time, dos_date = 0x08, 0, 0x21
    local = struct.pack("<IHHHHHIIIHH", 0x04034B50, 20, flags, method, dos_time, dos_date, 0, 0, 0, len(name), 0)
    descriptor = struct.pack("<IIII", 0x08074B50, crc, len(data), len(dex))
    body = local + name + data + descriptor
    central = struct.pack(
        "<IHHHHHHIIIHHHHHII", 0x02014B50, 20, 20, flags, method, dos_time, dos_date,
        crc, len(data), len(dex), len(name), 0, 0, 0, 0, 0, 0,
    ) + name
    end = struct.pack("<IHHHHIIH", 0x06054B50, 0, 0, 1, 1, len(central), len(body), 0)
    with open(dst, "wb") as out:
        out.write(body + central + end)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
