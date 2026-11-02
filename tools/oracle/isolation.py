"""Filesystem manifest walker shared by the oracle isolation drivers."""
import hashlib
import os


def tree(root, base):
    out = []
    for d, dirs, files in os.walk(root):
        dirs.sort()
        for name in sorted(dirs + files):
            p = os.path.join(d, name)
            rel = os.path.relpath(p, base)
            if os.path.islink(p):
                out.append({"path": rel, "kind": "symlink", "target": os.readlink(p)})
            elif os.path.isdir(p):
                out.append({"path": rel, "kind": "dir"})
            else:
                h = hashlib.sha256()
                with open(p, "rb") as f:
                    for chunk in iter(lambda: f.read(1 << 20), b""):
                        h.update(chunk)
                out.append({"path": rel, "kind": "file", "size": os.path.getsize(p), "sha256": h.hexdigest()})
    return sorted(out, key=lambda e: e["path"])

