#!/usr/bin/env python3
"""Rebuild vendor/russh from the crates.io archive, as vendor/russh/README.marathon.md describes, and compare it with the
committed tree.

    python3 -I scripts/replay_russh_vendor.py <russh-0.64.1.crate> <empty output directory>

The steps, in order: check the archive's SHA-256, unpack it, apply patches 1-4 with `git apply` (no fuzz), then the
adaptations below, each of which must match exactly where it expects to. LICENSE and README.marathon.md are not in the
archive and are copied from the committed tree. Exits 1 with a list of differences if the result is not byte-for-byte the
tracked files of vendor/russh. Not a gate step: run it after changing a patch or an adaptation.
"""
import hashlib
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
VENDOR = ROOT / "vendor/russh"
SHA256 = "ba61e87b9ec9a39a59a6bbed4c0b8ff7fb07073405b24a767a1f2c8560ac8f58"
PREFIX = "russh-0.64.1/"
PATCHES = [
    "spikes/ssh-russh/russh-close-after-local-close.patch",
    "tools/fake-worker/russh-server-hooks.patch",
    "crates/marathon-apple/russh-manual-window.patch",
    "crates/marathon-apple/russh-open-failure-description.patch",
]
REMOVED = [".cargo_vcs_info.json", "Cargo.lock"]
COPIED = ["LICENSE", "README.marathon.md"]
# (source file, fixture, macro): the one string literal in the file whose value is the fixture's text becomes
# `macro!("<path from the file to the fixture>")`.
FIXTURES = [
    ("src/helpers.rs", "helpers-01.txt", "include_str"),
    ("src/keys/known_hosts.rs", "keys-known_hosts-01.txt", "include_bytes"),
] + [("src/keys/mod.rs", f"keys-mod-0{n}.txt", "include_str") for n in range(1, 7)]
NOTE = "see vendor/russh/README.marathon.md for each change."
HEADERS = {
    "Cargo.toml": "# Modified by marathon-rs: see README.marathon.md for each change to this crate.",
    "src/channels/mod.rs": f"// Modified by marathon-rs (open failure description): {NOTE}",
    "src/client/encrypted.rs": "// Modified by marathon-rs (close after local close (R1/U1), manual window adjust, "
    f"open failure description): {NOTE}",
    "src/client/mod.rs": "// Modified by marathon-rs (close after local close (R1/U1), manual window adjust, "
    f"open failure description): {NOTE}",
    "src/client/session.rs": f"// Modified by marathon-rs (close after local close (R1/U1)): {NOTE}",
    "src/client/test.rs": f"// Modified by marathon-rs (tests for the client changes): {NOTE}",
    "src/helpers.rs": f"// Modified by marathon-rs (inline test strings moved to fixtures): {NOTE}",
    "src/keys/known_hosts.rs": f"// Modified by marathon-rs (inline test strings moved to fixtures): {NOTE}",
    "src/keys/mod.rs": f"// Modified by marathon-rs (inline test strings moved to fixtures): {NOTE}",
    "src/lib_inner.rs": f"// Modified by marathon-rs (open failure description): {NOTE}",
    "src/pty.rs": f"// Modified by marathon-rs (variant rename, wire number unchanged): {NOTE}",
    "src/server/encrypted.rs": f"// Modified by marathon-rs (server hooks): {NOTE}",
    "src/server/mod.rs": f"// Modified by marathon-rs (server hooks): {NOTE}",
    "src/server/session.rs": f"// Modified by marathon-rs (server hooks): {NOTE}",
    "src/session.rs": f"// Modified by marathon-rs (server hooks): {NOTE}",
}


def fail(message: str) -> None:
    sys.exit(f"replay_russh_vendor: {message}")


def replace_once(text: str, old: str, new: str, where: str) -> str:
    if text.count(old) != 1:
        fail(f"{where}: expected one {old[:60]!r}, found {text.count(old)}")
    return text.replace(old, new)


def unpack(archive: pathlib.Path, out: pathlib.Path) -> None:
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    if digest != SHA256:
        fail(f"{archive}: SHA-256 {digest}, expected {SHA256}")
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            if not member.name.startswith(PREFIX) or not (member.isfile() or member.isdir()):
                fail(f"unexpected archive member {member.name}")
            target = out / member.name[len(PREFIX):]
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            source = tar.extractfile(member)
            if source is None:
                fail(f"cannot read {member.name}")
            target.write_bytes(source.read())


def rust_literal_bodies(value: str) -> list:
    """The two ways upstream writes a literal with this value: newlines kept, or escaped as \\n."""
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return list(dict.fromkeys([escaped, escaped.replace("\n", "\\n")]))


def move_to_fixture(out: pathlib.Path, source: str, fixture: str, macro: str) -> None:
    path = out / source
    text = path.read_text()
    value = (ROOT / "fixtures/fake-worker/russh" / fixture).read_text()
    prefix = "b" if macro == "include_bytes" else ""
    found = [body for body in rust_literal_bodies(value) if f'{prefix}"{body}"' in text]
    if len(found) != 1:
        fail(f"{source}: the literal for {fixture} matched {len(found)} spellings")
    depth = "../" * (len(pathlib.PurePosixPath("vendor/russh", source).parts) - 1)
    path.write_text(
        replace_once(
            text, f'{prefix}"{found[0]}"', f'{macro}!("{depth}fixtures/fake-worker/russh/{fixture}")', source
        )
    )


def rename_pty_opcode_4(out: pathlib.Path) -> None:
    # The variant for opcode 4 is renamed in its declaration and its decoding; its wire number does not change.
    path = out / "src/pty.rs"
    text = path.read_text()
    names = re.findall(r"^    (\w+) = 4,$", text, re.M)
    if len(names) != 1:
        fail(f"src/pty.rs: expected one variant for opcode 4, found {names}")
    text = replace_once(text, f"    {names[0]} = 4,", "    KillLine = 4,", "src/pty.rs")
    text = replace_once(text, f"4 => Some(Pty::{names[0]}),", "4 => Some(Pty::KillLine),", "src/pty.rs")
    path.write_text(text)


def replay(archive: pathlib.Path, out: pathlib.Path) -> None:
    inside = subprocess.run(["git", "rev-parse", "--is-inside-work-tree"], cwd=out, capture_output=True, text=True)
    if inside.returncode == 0:
        # There `git apply` would take paths from the repository's root.
        fail(f"{out} is inside a git work tree; use a directory outside one")
    unpack(archive, out)
    for patch in PATCHES:
        # No fuzz and no offsets beyond git's own: every hunk applies with its full context, or the replay fails.
        result = subprocess.run(
            ["git", "apply", "-p1", "--whitespace=nowarn", str(ROOT / patch)], cwd=out, capture_output=True, text=True
        )
        if result.returncode != 0:
            fail(f"{patch} does not apply:\n{result.stderr}")
    for name in REMOVED:
        (out / name).unlink()
    cargo = out / "Cargo.toml"
    cargo.write_text(cargo.read_text() + "\n[workspace]\n")
    rename_pty_opcode_4(out)
    for source, fixture, macro in FIXTURES:
        move_to_fixture(out, source, fixture, macro)
    for name, header in HEADERS.items():
        path = out / name
        path.write_text(header + "\n" + path.read_text())
    for name in COPIED:
        shutil.copyfile(VENDOR / name, out / name)


def differences(out: pathlib.Path) -> list:
    built = {p.relative_to(out).as_posix(): p for p in out.rglob("*") if p.is_file()}
    # Tracked files only: the vendored crate's own tests leave an ignored Cargo.lock there.
    tracked = subprocess.run(
        ["git", "ls-files", "-z", "vendor/russh"], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout.split("\0")
    committed = {
        name[len("vendor/russh/"):]: ROOT / name for name in tracked if name
    }
    found = [f"only in the replay: {name}" for name in sorted(built.keys() - committed.keys())]
    found += [f"only in vendor/russh: {name}" for name in sorted(committed.keys() - built.keys())]
    found += [
        f"differs: {name}"
        for name in sorted(built.keys() & committed.keys())
        if built[name].read_bytes() != committed[name].read_bytes()
    ]
    return found


def main() -> None:
    if len(sys.argv) != 3:
        fail("usage: replay_russh_vendor.py <russh-0.64.1.crate> <empty output directory>")
    archive, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    if any(out.iterdir()):
        fail(f"{out} is not empty")
    replay(archive, out)
    found = differences(out)
    if found:
        print("\n".join(found))
        sys.exit(1)
    print(f"{out} matches vendor/russh byte for byte")


if __name__ == "__main__":
    main()
