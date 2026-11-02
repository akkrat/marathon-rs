# Differential evidence and vendor suites

The reviewed [registry](../../tools/differential/registry.json) contains 52 active
case groups and 17 capture adapters: the original actual-local/injected-parse
contracts and 15 whole-vendor CLI scenarios. The vendor scenarios compare a
fresh private fake-worker or scripted-ADB world against independent JVM evidence
from frozen `e47352f2`. There are no source/parity rows assigned to 7.1a/7.1b.

## Commands and activation

Run from the checkout with the documented Rust PATH:

```sh
tools/differential/run validate
tools/differential/run check
tools/differential/run native-vendors --output tools/differential/runs/new-native
tools/differential/run smoke --profile macos-arm64 --output tools/differential/runs/new-pairs
tools/differential/run all --profile linux-x86_64 --output tools/differential/runs/new-all
tools/differential/run smoke --profile harness-first --output tools/differential/runs/reduced
tools/differential/run capture ios-smoke --output fixtures/differential/vendors/ios-smoke/jvm-v3
tools/differential/run compare EXPECTED ACTUAL --contract CONTRACT --mismatch NEW_DIRECTORY
```

`validate` checks the generated ledger, pinned legacy evidence, all golden
manifests and both verification manifests. It compares every repeatability
record against its independent JVM golden. `check` adds adversarial Python tests
and all registered native targets. `scripts/check.sh` runs offline validation
and Python tests; its ordinary workspace test pass executes the native targets,
including `marathon-cli/tests/differential_vendors.rs`, once. No JDK, Gradle,
Xcode, real ADB executable or download is needed for that gate. Native executable
builds use the existing Cargo toolchain/cache outside runtime sandboxes.

`native-vendors` runs all 15 whole-vendor adapters against committed goldens.
`smoke` builds/verifies the reference and patched JVM distributions, then runs two
fresh JVM captures and one native capture per selected adapter. `all` also runs
the offline/native gate. `macos-arm64` and `linux-x86_64` require the matching
host; selecting another host cannot claim success. `harness-first` explicitly
reduces selection to the two portable adapters. Every attempt has a fresh
sandbox, HOME/TMPDIR, process logs and bounded watchdog. There is no retry loop.

The registry selects each adapter's contract and immutable golden generation.
Neither capture nor comparison changes that selection. Refresh intentionally
with `python3 -I tools/differential/make_registry.py`; the gate only uses
`--check`. Factory tripwires cover the CLI factory expression and vendor
registration function, so unrelated CLI functions do not force activation.
Missing adapters, factories, evidence or tools fail instead of reducing coverage.

The existing manual/tag workflow remains separate from ordinary CI. It requires
provisioned runners with the read-only frozen source, JBR and cached build
inputs. Release tags select the full host profile. Actual local, Linux and
Actions results must be reported separately; recording a macOS profile does not
claim a Linux or Actions execution.

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

Capture starts in a new ignored sibling directory. Complete JVM evidence is
validated before a same-filesystem rename publishes a **new** generation.
Existing generations cannot be overwritten. Registry hashes pin each selected
golden and the verification manifest; the latter pins every fresh JVM/native
record. Provenance includes source base, jars, patches, hook sources, JDK and
waited processes. It describes origin, not cryptographic attestation: fabricated
but shaped metadata is not proof without the reviewed evidence pins.

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
bijective by identity type and shared across paths and references. Product JSON
is edited at declared token locations, preserving other bytes, numeric spelling,
null/omission, array order, statuses, messages and metadata. D3 removes only the
root/nested DeviceInfo delegate; similarly named user metadata is preserved.
Literal UUID-shaped text is never globally replaced. Taped clocks/batch/Allure
and DEX identity values remain exact. D12/F29 configuration-object identities
validate the JVM tape value and native class-name hash before substituting the
same typed object role. Temporary source/receive file roles come from declared
rsync operands; transferred/consumed bytes remain exact.

Allure JSON and environment XML retain 3.9c's approved structural comparison:
exact scalar values, numeric lexemes, ordered arrays/environment entries and
references. Allure trees always use the explicit harness alias
`reports/allure-results/`, independent of the physical `outputDir`; other roots
cannot opt in accidentally. Other Gson/JUnit/HTML bytes remain exact outside
the designated token substitutions. Opaque globs use path components: `*` does
not cross `/`, while `**` explicitly does.

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
handshake diagnostics are projected. Other log text, external tool warnings,
progress, summaries, final messages and their stream/exit remain significant.
The first explicit unordered stdout block is the adjacent artifact-admission and
worker-queue-wait pair: submit launches an independent worker before logging
its admission, so either order is source-observed. Both complete lines and their
multiplicity/values are retained. The other block is the caller's provider-initialized
timing line and the independent iOS update-consumer's adjacent two-line event;
either can run first. Other stdout order is unchanged.

Apple inputs add 30 ms line spacing (the named settled video variant uses 1000 ms) to let event consumers settle before the
next scripted line. Both fresh JVM repetitions and native captures compare every
log/attachment exactly. Unpaced A20 mismatches remain diagnostics; no log family
is omitted and no optional-log variant was promoted. P4's finite complete JVM
variant comparator remains available for a future explicitly witnessed case.

A mismatch retains the original records, normalized views and path-level
failure list (or validation error). Adversarial checks cover missing/extra
files/directories, status/failure/reference/array changes, mode, identity
collision/inconsistency, GIF timing, literal messages, stream/exit changes,
removed cleanup, truncation, unused tapes, malformed transcripts, forged outcome
stamps, storage corruption and the real U30 host/wire mutations with all hashes
recomputed. Comparisons never update a golden.
