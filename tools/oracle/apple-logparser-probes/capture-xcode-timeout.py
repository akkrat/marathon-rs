#!/usr/bin/env python3
"""Capture a real macOS XCTest timeout without simulators/devices or changes to the source checkout.
All build products and result bundles stay in a temporary directory. Expected xcodebuild exit: 65.
"""
import pathlib
import shutil
import subprocess
import tempfile

here = pathlib.Path(__file__).resolve().parent
out = here.parents[2] / 'fixtures/apple-logparser/xcode27-timeout.log.input'
with tempfile.TemporaryDirectory(prefix='mrs47-xcode-') as tmp:
    work = pathlib.Path(tmp)
    shutil.copytree(here / 'xcode-timeout', work, dirs_exist_ok=True)
    command = ['/usr/bin/xcodebuild', '-project', str(work / 'Timeout.xcodeproj'), '-scheme', 'TimeoutTests',
               '-destination', 'platform=macOS', '-derivedDataPath', str(work / 'build'),
               '-resultBundlePath', str(work / 'result.xcresult'), '-test-timeouts-enabled', 'YES',
               '-default-test-execution-time-allowance', '60', '-maximum-test-execution-time-allowance', '60', 'test']
    result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=300)
    lines = [line for line in result.stdout.decode().split('\n') if line.startswith("Test Case '-[TimeoutTests.")]
    if result.returncode != 65 or not any('exceeded execution time allowance' in line for line in lines):
        raise SystemExit(f'No expected timeout (rc={result.returncode}); fixture not changed')
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text('\n'.join(lines) + '\n')
    print(out)
