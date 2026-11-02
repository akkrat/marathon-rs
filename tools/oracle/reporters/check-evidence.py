#!/usr/bin/env python3
"""Offline evidence hash and manifest checks; never modifies evidence."""
import hashlib
import json
from pathlib import Path

root = Path(__file__).resolve().parents[3] / 'fixtures/oracle/html-allure-gif'
for line in (root / 'evidence.sha256').read_text().splitlines():
    digest, name = line.split('  ', 1)
    assert hashlib.sha256((root / name).read_bytes()).hexdigest() == digest, name
manifest = json.loads((root / 'manifest.json').read_text())
by_path = {entry['path']: entry for entry in manifest}
# Check committed files against their original manifest; empty directories are
# represented in JSON because Git cannot store an empty directory itself.
for path, entry in by_path.items():
    if entry['kind'] == 'file':
        assert hashlib.sha256((root/'manifest'/path).read_bytes()).hexdigest() == entry['sha256'], path
assert by_path['logs/pool/serial-#1']['kind'] == 'directory'

# The five negative witnesses (missing artifact, changed status, wrong profiling
# link, changed GIF delay, omitted capture cleanup) run through the Rust
# comparators themselves: html_reporter::raw_pool_names_overwrites_artifact_gates_and_log_urls,
# allure_reporter::profiling_links_use_form_encoding_and_replace_every_placeholder,
# screenshot::gif_metadata_zero_frames_and_invalid_delays_match_jvm and
# screenshot::cancellation_during_capture_write_and_delay_waits_for_cleanup.
print('Reporter evidence hashes and manifest passed')
