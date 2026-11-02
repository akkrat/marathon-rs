#!/usr/bin/env python3
"""WP 6.4 oracle helper: build minimal binary AndroidManifest.xml APKs that
aapt2 refuses to produce (no package attribute, non-pooled attribute values).

Usage: build_axml.py <out.apk> <mode>
Modes: missing-package  (instrumentation complete, manifest has no package)
       missing-name-and-package
       unpooled-package (package attribute valueString = -1, typed int data)
       surrogate-high-package (package ends in an unpaired high surrogate)
       surrogate-low-runner (runner name holds an unpaired low surrogate)
       surrogate-pair-runner (runner name holds a valid surrogate pair)
Layout matches what com.shazam.axmlparser.AXMLParser streams: file header,
UTF-16LE string pool, resource-id chunk, start-namespace, elements,
end-namespace.
"""
import struct
import sys
import zipfile


def string_pool(strings):
    offsets, data = [], b""
    for s in strings:
        offsets.append(len(data))
        encoded = s.encode("utf-16-le", "surrogatepass")
        data += struct.pack("<H", len(encoded) // 2) + encoded + b"\x00\x00"
    header_size = 28
    strings_offset = header_size + 4 * len(strings)
    chunk_size = strings_offset + len(data)
    out = struct.pack("<HHI", 0x0001, header_size, chunk_size)
    out += struct.pack("<IIIII", len(strings), 0, 0, strings_offset, 0)
    out += b"".join(struct.pack("<I", o) for o in offsets) + data
    return out


def node(chunk_type, line, comment, body):
    return struct.pack("<HHIii", chunk_type, 16, 16 + len(body), line, comment) + body


SURROGATE_STRINGS = {
    "surrogate-high-package": (7, "com.example.test\ud834"),
    "surrogate-low-runner": (9, "com.example.\udd1eRunner"),
    "surrogate-pair-runner": (9, "com.example.\U0001d11eRunner"),
}


def build(mode):
    # indices: 0 uri, 1 "android", 2 "manifest", 3 "package", 4 "instrumentation",
    # 5 "name", 6 "targetPackage", 7 "com.example.test", 8 "com.example",
    # 9 "com.example.Runner", 10 "application"
    strings = ["http://schemas.android.com/apk/res/android", "android", "manifest",
               "package", "instrumentation", "name", "targetPackage",
               "com.example.test", "com.example", "com.example.Runner", "application"]
    if mode in SURROGATE_STRINGS:
        index, text = SURROGATE_STRINGS[mode]
        strings[index] = text
    pool = string_pool(strings)
    resids = struct.pack("<HHI", 0x0180, 8, 8)  # empty resource-id chunk
    ns_start = node(0x0100, 1, -1, struct.pack("<ii", 1, 0))
    ns_end = node(0x0101, 1, -1, struct.pack("<ii", 1, 0))

    def attr(name_idx, value_string_idx, value_type=0x03, data=None):
        return struct.pack("<iii", 0, name_idx, value_string_idx) + \
            struct.pack("<HBB", 8, 0, value_type) + \
            struct.pack("<i", value_string_idx if data is None else data)

    def start(name_idx, attrs):
        body = struct.pack("<i", -1) + struct.pack("<i", name_idx)
        body += struct.pack("<HHII", 20, 20, len(attrs), 0)
        # ResXMLTree_attrExt carries idIndex/classIndex/styleIndex shorts inside
        # the two ints axmlparser skips; the packing above is: attributeStart(2)
        # attributeSize(2) attributeCount(4-as-int-low) -- parser reads 3 ints:
        # flags, attributeCount, ?. Rebuild exactly per AXMLParser.doNext reads:
        body = struct.pack("<i", -1) + struct.pack("<i", name_idx)
        body += struct.pack("<i", 0) + struct.pack("<i", len(attrs)) + struct.pack("<i", 0)
        body += b"".join(attrs)
        return node(0x0102, 1, -1, body)

    def end(name_idx):
        return node(0x0103, 1, -1, struct.pack("<ii", -1, name_idx))

    if mode == "missing-runner":
        manifest_attrs = [attr(3, 7)]
        instr_attrs = [attr(6, 8)]
    elif mode == "missing-package":
        manifest_attrs = []
        instr_attrs = [attr(5, 9), attr(6, 8)]
    elif mode == "missing-name-and-package":
        manifest_attrs = []
        instr_attrs = [attr(6, 8)]
    elif mode in SURROGATE_STRINGS:
        manifest_attrs = [attr(3, 7)]
        instr_attrs = [attr(5, 9), attr(6, 8)]
    elif mode == "unpooled-package":
        manifest_attrs = [attr(3, -1, value_type=0x10, data=7)]
        instr_attrs = [attr(5, 9), attr(6, 8)]
    else:
        raise SystemExit(f"unknown mode {mode}")

    body = pool + resids + ns_start
    body += start(2, manifest_attrs)
    body += start(10, []) + end(10)
    body += start(4, instr_attrs) + end(4)
    body += end(2) + ns_end
    header = struct.pack("<HHI", 0x0003, 8, 8 + len(body))
    return header + body


def main():
    out, mode = sys.argv[1], sys.argv[2]
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        # Fixed timestamp: regeneration must be byte-stable.
        info = zipfile.ZipInfo("AndroidManifest.xml", date_time=(1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        z.writestr(info, build(mode))


if __name__ == "__main__":
    main()
