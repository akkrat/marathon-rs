"""Small reviewed factory tripwires, excluding unrelated CLI implementation text."""
import hashlib
import re


def factory_fingerprints(root):
    entry = (root / 'crates/marathon-cli/src/main.rs').read_text()
    slots = (root / 'crates/marathon-cli/src/runner.rs').read_text()
    registration = re.search(r'^pub fn missing_vendors\(\) -> VendorFactories \{\n.*?^\}', slots, re.M | re.S)
    selected = re.findall(r'^\s*factories:.*$', entry, re.M)
    if registration is None or len(selected) != 1:
        raise ValueError('CLI factory registration shape changed; review registry extractor')
    digest = lambda value: hashlib.sha256(value.encode()).hexdigest()
    return dict(factory_entry_sha256=digest(selected[0].strip()),
                factory_slots_sha256=digest(registration[0]))
