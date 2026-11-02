"""Normalises the outputs of a fake-adb scenario run (a run-isolated.sh run directory) for the comparison baseline.

    python3 -I scenario_report.py <run dir> <fake-adb port> <output dir>

Writes into <output dir>:
- report-manifest.json: marathon's exit code and every path it wrote under the Marathonfile's outputDir (`in/output`);
- results.json: the semantic snapshot: per test its status, the role of the device that ran it and the failure
  message, plus the JUnit totals;
- marathon.stdout and marathon.stderr: marathon's output.

Everything is normalised the same way: the run directory becomes @RUN@, the fake-adb port @PORT@, each UUID a stable
placeholder <uuid-N> (numbered by the test its file belongs to, then by first appearance), JVM identity strings
`…;@1a2b3c` become `…;@<hash>`, and log times, durations and the JVM properties marathon logs at start-up are masked.
Device serials become roles: device-1 is the device that ran the first test in test_result/raw.json, the others follow
in serial order, so it doesn't matter which emulator marathon borrowed. Prints the exit code and exits with it.
"""

import glob
import json
import os
import re
import sys
import xml.etree.ElementTree as ET

UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


class Normaliser:
    def __init__(self, run, port, serials):
        self.run = os.path.realpath(run)
        self.port = re.compile(rf"(?<![0-9]){re.escape(port)}(?![0-9])")
        self.uuids = {}
        self.roles = {s: f"device-{i + 1}" for i, s in enumerate(serials)}
        self.serial = re.compile("|".join(re.escape(s) for s in sorted(serials, key=len, reverse=True))) if serials \
            else None

    def uuid(self, m):
        return self.uuids.setdefault(m.group(0), f"<uuid-{len(self.uuids) + 1}>")

    def text(self, s):
        s = s.replace(self.run, "@RUN@")
        s = self.port.sub("@PORT@", s)
        s = UUID.sub(self.uuid, s)
        if self.serial:
            s = self.serial.sub(lambda m: self.roles[m.group(0)], s)
        s = re.sub(r";@[0-9a-f]+", ";@<hash>", s)
        return s

    def log(self, s):
        s = self.text(s)
        # The JVM and system properties marathon logs at start-up describe the machine, not the run.
        s = re.sub(r"(<com\.malinskiy\.marathon\.Marathon> [\w.]+): .*", r"\1: <value>", s)
        s = re.sub(r"\b\d{2}:\d{2}:\d{2}\.\d{3}\b", "<time>", s)
        s = re.sub(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z?", "<timestamp>", s)
        s = re.sub(r"\b\d+ ms\b", "<n> ms", s)
        s = re.sub(r"Total time: .*", "Total time: <duration>", s)
        return s


def main():
    run, port, out = sys.argv[1], sys.argv[2], sys.argv[3]
    with open(os.path.join(run, "manifest.json"), encoding="utf-8") as f:
        manifest = json.load(f)
    output = os.path.join(run, "in", "output")

    raw_path = os.path.join(output, "test_result", "raw.json")
    raw = json.load(open(raw_path, encoding="utf-8")) if os.path.exists(raw_path) else []
    serials = []
    for e in raw:
        s = e["deviceSerial"].rsplit(":", 1)[-1]
        if s not in serials:
            serials.append(s)
    for f in sorted(glob.glob(os.path.join(output, "devices", "*", "*.json"))):
        s = os.path.basename(f)[:-len(".json")].rsplit("-", 2)[-2:]
        s = "-".join(s)
        if s not in serials:
            serials.append(s)
    n = Normaliser(run, port, serials)

    # UUIDs in Allure file names are numbered by the test the file describes, so the numbering is stable.
    allure = []
    for f in glob.glob(os.path.join(output, "allure-results", "*-result.json")):
        with open(f, encoding="utf-8") as fh:
            allure.append((json.load(fh).get("fullName", ""), os.path.basename(f)))
    for _, name in sorted(allure):
        UUID.sub(n.uuid, name)

    prefix = "in/output/"
    entries = [e for e in manifest["tree"] if e["path"].startswith(prefix)]
    files = sorted(({"path": n.text(e["path"][len(prefix):]), "kind": e["kind"]} for e in entries),
                   key=lambda e: e["path"])
    report = {"exitCode": manifest["exitCode"], "timedOut": manifest["timedOut"], "output": files}

    failures, totals = {}, {}
    for junit in sorted(glob.glob(os.path.join(output, "tests", "*", "*.xml"))):
        suite = ET.parse(junit).getroot()
        for key in ("tests", "failures", "errors", "skipped"):
            totals[key] = totals.get(key, 0) + int(suite.get(key, "0"))
        for case in suite.iter("testcase"):
            failure = case.find("failure")
            if failure is not None:
                message = (failure.get("message") or failure.text or "").strip().splitlines()
                failures[f"{case.get('classname')}#{case.get('name')}"] = message[0] if message else ""
    tests = []
    for e in raw:
        name = f"{e['package']}.{e['class']}#{e['method']}"
        tests.append({
            "test": name,
            "status": e["status"],
            "device": n.roles.get(e["deviceSerial"].rsplit(":", 1)[-1], "?"),
            "failure": n.text(failures[name]) if name in failures else None,
        })
    results = {"junit": totals, "tests": sorted(tests, key=lambda t: t["test"])}

    os.makedirs(out, exist_ok=True)
    for name, data in (("report-manifest.json", report), ("results.json", results)):
        with open(os.path.join(out, name), "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2)
            f.write("\n")
    for stream in ("stdout", "stderr"):
        with open(os.path.join(run, stream), encoding="utf-8", errors="replace") as f:
            text = f.read()
        with open(os.path.join(out, f"marathon.{stream}"), "w", encoding="utf-8") as f:
            f.write(n.log(text))

    print(f"marathon exit code {manifest['exitCode']}{' (timed out)' if manifest['timedOut'] else ''}, "
          f"{sum(1 for e in entries if e['kind'] == 'file')} output files, "
          + ", ".join(f"{t['test']} {t['status']} on {t['device']}" for t in results["tests"]))
    code = manifest["exitCode"]
    sys.exit(code if 0 <= code < 256 else 1)


main()
