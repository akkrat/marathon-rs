# Differential evidence and vendor suites

The Phase 8 contract is in [PLAN.md §3.12](../../PLAN.md#312-phase-8-contract).
`0.12.0-rc.1` is the JVM-parity baseline. JVM captures and their verification
records are historical; do not generate new JVM records. The gate compares fresh
native captures against the selected Rust-owned `native-v1` views for all 17
adapters, including 15 bounded vendor cases.

## The external-storage wipe (D27)

Kotlin's prepare runs `rm -r /sdcard` and `mkdir /sdcard` on the fake's storage; the native program does not.
`vendor_transport.adb` leaves out exactly those two commands of a JVM record and of a native record pinned by digest
from before D27, the same scope as D20's (`evidence.may_withhold`). A native run of today's code that wiped again would
keep them and differ. `test_evidence.py` (`test_d27_*`) proves that no other `rm` or `mkdir` is affected, and the
scoped owner contract `scenarios_test.rs` applies the same rule to its recordings.

## The third-party parser library (D20)

The macOS cases are the only ones that push `libxctest-parser.dylib`. A JVM run pushes its own
copy, which this repository must not hold, so `evidence.py` records
`worker/shared/libxctest-parser.dylib` as `withheld`: the path, mode, size and digest stay, the
payload does not, and `validate` accepts this for that path, digest and size only. A native
run gets a fixed stand-in from a directory the harness supplies, and `normalized` gives the
withheld entry and that exact stand-in the same content. Other bytes in a native run, the
stand-in in a JVM record, or a different mode still differ. A live native capture of the real
digest is refused outright. Only the exact older native records may keep a withheld entry, because
those runs still embedded the library: `validate` looks up the `evidence.json` digest in the
verification manifests, so changing a record, or naming an old commit in its provenance, does not qualify. So the comparison proves that a
supplied file reaches the worker's path with the right mode, and no longer that its bytes
equal Kotlin's, which the D20 stand-in rules out by construction.

The rsync stdin of that transfer is zeroed in JVM transcripts (`redact_parser_transfer`),
keeping rows, sizes and order. `ssh()` drops rsync streams from the comparison, so this changes
no comparison. `validate_record` rejects a recorded transcript with an unmarked chunk in that
transfer, a marker other than `D20`, a marker elsewhere, or a marked chunk that is not all zeros.
A new native transcript may hold no marked row and at most 16 KiB in that transfer, counting every
retained stream in it, far below any of the libraries; a recorded one has that bound on all but its
zeroed `stdin`. `write_record` applies these rules, and a JVM capture's redaction, before storing a payload.

## Checking and re-blessing

`tools/differential/run check` validates historical evidence and runs the native
contracts and comparator self-tests. `scripts/check.sh` runs the offline checks
and the native targets through the workspace tests. `native-vendors --output
NEW_DIRECTORY` runs the 15 whole-vendor cases. Ruby with Psych is required for
YAML comparison. No JVM or JDK is needed for these checks.

`tools/differential/run bless` explicitly captures all registered cases and
publishes normalized, compressed views and commit/host provenance under
`fixtures/contract/native-v1`. It refuses to overwrite an existing generation;
remove the old generation explicitly before a deliberate re-bless. Review every
golden change, and add a D-row for a contract change. The gate never blesses.

The configuration corpus runs as `cargo test -p marathon-cli --test
config_contract`. Set `MARATHON_BLESS_CONFIG=1` only when deliberately
rewriting `fixtures/contract/config/corpus.json`; review that diff. The corpus
pins accepted configuration dumps and rejection outcomes with a fixed environment
and clock. Marathondevices are bound directly because config-dump handles
Marathonfiles; their full bound value is recorded separately.

`progress_oracle::progress_line_contract_for_each_status` and
`reporters::stdout_summary_contract` pin progress and summary text. The existing
`started_ci_passes_fraction_and_prints_after_ci` also pins the started line.

## Scenario scope

| Adapter set | Observations |
|---|---|
| `ios-smoke`, `macos-smoke` | Passing, failed and ignored tests, real SSH commands/rsync argv, complete reports, xctestrun/xcresult bytes, permissions and worker cleanup |
| `ios-media`, `ios-screenshot` | Video/transcode/device logs and A19 leftover; exact decoded GIF frames and timing; all report references |
| `android-smoke`, `android-two-devices`, `android-remote` | Passing local discovery; two-device setup/install plus pass/fail/ignored batch; remote discovery plus execution; complete reports, shell output and final ADB state |
| Each vendor's `-ignore` and `-parse` | Failure suppression stays on stderr with CLI exit 0; named `.yml`, seeded output and parse/provider side effects |
| `ios-pass`, `macos-pass` | Passing Apple CLI exit 0; Android passing is `android-smoke` |

Every case seeds an output sentinel containing a literal UUID-shaped message.
Runs must wipe it; parse must preserve it. The Android fake SDK contains a
rejecting adb tripwire. Only a ready fake on an allocated non-5037 loopback port
is passed to the CLI. No real ADB server/device/emulator is contacted.

P1 integrates the owners' installer, recovery/boot/crash, timeout/loss,
coverage/file-sync, video/GIF and profiling JVM contracts as explicitly scoped
registry rows. Those rows retain their component/request comparison claims;
they do not become additional whole-CLI full-manifest scenarios. This avoids
an exhaustive feature/transport cross-product. The 15 CLI scenarios provide the
representative end-to-end manifests.

P3 retains actual `KotlinProcessCommandExecutor`/`JvmFileBridge` command and copy
pairs, plus the labelled shared Apple world/loopback-SSH contracts from 5.1.
There is no new JVM injection framework for a whole fake local Apple CLI.
Absolute real local Apple tools, real hosts/devices and certification remain 7.2.

The new native probe uses the real CLI parser, production factories, core,
vendors, transports, listeners and reporters. `ProductionRunner::construct_with_hooks`
is an additive test seam; the public binary supplies live hooks. The probe
freezes wall/monotonic reads and core batch/Allure/DEX-array IDs through existing
`RuntimeHooks`, retaining real scheduler/socket time. Android connection,
prepare and execute barriers fix the two-device assignment. Finite draws and
barriers must be consumed before the probe writes its hook report.

## Evidence and publication

The original 7.1a portable generations remain evidence format 1, unchanged.
Vendor generations use format 2 with `storage: gzip@1`: a binary, lossless
content-addressed envelope. Manifest sizes and SHA-256 always describe the
**decoded original bytes**. Gzip is storage only; it does not alter wire bytes,
reports or comparison. This keeps opaque base64 rsync traffic out of text diffs and searches. The initial uncompressed recordings remain
unchanged in ignored staging; no opaque content was redacted.

Both formats enumerate every relative path, including tree roots, empty
directories, files and links. Unix modes and literal link targets are compared;
files have raw length/hash and payload bytes. `lstat` never follows a link.
Directory inode sizes, ownership and timestamps are not manifest fields. The
validator rejects unknown formats/entries, missing parents, duplicate paths,
collisions, corrupt/missing/extra payloads and incomplete outcomes.

The whole Apple worker filesystem is an additional manifest tree, including
scripted/consumed xctestrun files, dylib/product bytes, control files and leftover
media. Raw final world state and transport streams are retained. Fake ADB
exports every device's files (bytes/modes/mtimes), reverse forwards, properties,
features and connection state only after joined shutdown. Optional output
capture records successful stdout/stderr writes; strict warnings, failed jobs,
transcript writes or fake shutdown fail the capture. Streamed install stdin is
addressed by its server-measured digest and the unchanged pinned APK payload.

CLI and fake exit statuses are observed separately; harness completion follows
successful capture/validation. Expected failed-test CLI=1, fake=0, harness=0 is
complete evidence. A watchdog, unused tape, unmatched command, missing process
observation or failed fake is never success. Standalone validation requires the
JVM parse driver transcript and recomputes local/parse/vendor outcomes. Unknown
scenario adapters are rejected. The artifact-only adapter is for synthetic
adversarial tests, not an active suite.

Historical JVM capture generations were published only after validation and
include source, JDK and process provenance. They are retained as historical
records, not as native references.

The driver and all probe processes use umask `0022`; source NIO temporaries remain 0600/0700.
File/directory permissions remain exact. The single `ios-media` runner symlink
validates its raw mode against the recorded host: macOS `0755` under umask 0022,
Linux `0777` (kernel-defined). Only that mode projects to the same explicit
kernel-default role; its path, link kind and complete target remain exact. Other
symlink modes remain exact. Raw manifests retain the physical lstat values. Rust tests do
blocking process/filesystem work outside core and await cleanup. No production
runtime/trait, serializer, regex or queue implementation is replaced.

## Typed comparison

`evidence.py` remains the sole comparator entrypoint. Vendor projections in
`vendor_reports.py`, `vendor_console.py`, `vendor_transport.py` and
`gif_frames.py` are used by both committed and fresh comparisons. Their source
hashes and scenario inputs are pinned in the ledger.

Each run binds explicit scenario device/worker, endpoint, sandbox and temporary
file roles. Device assignment is not inferred from a passing test. Bindings are
bijective by identity type and shared across paths and references. Product JSON is edited at declared token locations before comparison; structured
results compare as parsed data with key and array order, null/omission, statuses,
messages and metadata preserved. Numeric spellings and JSON escapes are not significant. D3 removes only the
root/nested DeviceInfo delegate; similarly named user metadata is preserved.
Literal UUID-shaped text is never globally replaced. Taped clocks/batch/Allure
and DEX identity values remain exact. D12/F29 configuration-object identities
validate the JVM tape value and native class-name hash before substituting the
same typed object role. Temporary source/receive file roles come from declared
rsync operands; transferred/consumed bytes remain exact.

JSON, XML and YAML result files compare as parsed data. JSON object key order,
XML element order and attribute sets, and YAML mapping order remain observable;
equivalent JSON numeric and escaped-string forms compare equal. Other text and
binary data retain their existing comparison. Vendor console comparison retains
only progress and summary lines. Allure history IDs and fragmentation hashes
remain exact. Historical JVM comparisons retain their original rules.

GIF comparison decodes every composited RGBA frame and compares frame order,
rectangle/count, delay, disposal, transparency, user-input flag, dimensions,
aspect, version, loop and unknown extensions. There is no pixel tolerance.
Different lossless palette/LZW encodings can therefore match. Video/scripted
assets remain byte exact. The old gradient-only 3.9c tolerance is not widened.

SSH streams aggregate chunks separately per channel; connection and channel
roles remain distinct. Normal control commands retain order. Source-parallel
Swift/ObjC discovery and transfer preparation have named lanes, with order and
multiplicity retained within each lane. iOS preparation also records a fork/join
boundary: the installer's SDK/root-generation commands and simulator keyboard/
boot checks keep their own ordered lanes until the next post-prepare command.
This mirrors `AppleSimulatorDevice.prepare`'s two joined async branches. Moving
a command across that join or changing either branch's order still fails. Host processes and server streams have
no invented shared total order. Rsync's generated server protocol flags/traffic
are diagnostics for the openrsync/Samba profiles; the caller's exact rsync argv,
including `--links`, `--rsh`, `--rsync-path`, partial and delay flags, remains a
contract, as do all transfer bytes, remote paths and outcomes. Opaque WriteFile
shell/base64 commands remain exact. ADB compares ordered per-device operations
and independent logcat lanes, aggregating each stream without sorting requests.
Host negotiation/transport selections remain raw protocol diagnostics.

Stdout and stderr remain separate. Only exact D1 telemetry lines, D12's provider
line/system facts/thread fields and narrowly identified SSH entropy/cipher
handshake diagnostics are projected. In historical JVM comparisons, other log
text and warnings were also significant; the native contract retains progress
and summary lines.
The first explicit unordered stdout block is the adjacent artifact-admission and
worker-queue-wait pair: submit launches an independent worker before logging
its admission, so either order is source-observed. Both complete lines and their
multiplicity/values are retained. The other block is the caller's provider-initialized
timing line and the independent iOS update-consumer's adjacent two-line event;
either can run first. Other stdout order is unchanged.

Apple inputs add 30 ms line spacing (the named settled video variant uses
1000 ms) to let event consumers settle before the next scripted line. Historical
JVM comparisons used complete finite variants. Native comparisons use the
selected normalized view.

The vendor test retains its run directory after a mismatch. Adversarial checks cover missing/extra
files/directories, status/failure/reference/array changes, mode, identity
collision/inconsistency, GIF timing, literal messages, stream/exit changes,
removed cleanup, truncation, unused tapes, malformed transcripts, forged outcome
stamps, storage corruption and the real U30 host/wire mutations with all hashes
recomputed. Comparisons never update a golden.
