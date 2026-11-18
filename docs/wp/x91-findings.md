# x91 — Android device cleanup

## Ownership and deadline

D84 starts each admitted Android device's stop before cancelling its actor. The device records one absolute
deadline on the vendor clock and seals its ordinary ADB request group. The scheduler runs the device cleanup
beside actor cancellation and joins both within its existing five-second margin. The provider starts cleanup for
its remaining devices at termination. It reuses the retained task for any device already stopping.

The owner admits work before spawning it and keeps a completion count through the body and its finalizers.
Instrumentation, logcat, screenshot capture, video and perfetto all participate, as do batch execution, setup,
restoration, installs and pulls. Sealing closes the admission gate and keeps D70's instrumentation submission
boundary. Existing D22 and D41 gates remain active during normal work. D81 uses the same pull deadline and
destination paths during a healthy batch.

The monitor publishes a disconnect and continues processing updates while the device retires. D41 can then open
the gate used by a batch whose grace period ended in that disconnect. A replacement waits for cleanup of earlier
instances of its serial before it sends device commands. Setup publication and retirement share the readiness lock.

With default timeouts the allowance is 60 seconds. If work remains at 55 seconds, force cancellation closes the
device's established sockets and pending connects. Listener registration is sealed and the listener lists are
cleared. Producer finalizers and remote stop commands then share the time left. Cleanup can enter this phase
earlier when the work has already finished. The cleanup commands use a separate request group for the same
device, which closes at the common deadline. The provider's root factory closes at provider termination. Other
devices' request groups remain usable when one device stops.

Disabled socket idle timeouts contribute zero. Saturation leaves room for the scheduler's
margin without producing the clock's infinite-delay sentinel. D21 still applies to healthy operations, including
silent requests with their own timeout, file-sync directories and video chunks. Pulls stage bytes beside the
destination and publish only after the sync protocol finishes and the writer drains. Requests from multiple
devices can target the same final path without a cancelled pull deleting a completed sibling's output.

## Remote state after forced closure

TCP EOF proves local connection closure. Android can retain an instrumentation process, screenrecord, perfetto,
their remote artifacts, an unacknowledged install or sync side effect, a reverse forward, or a D85 readiness marker
if adb cannot confirm the relevant command. Pushed files can remain in device storage. The device tries to stop its known recorders and remove its known
forwards and marker within the remaining allowance. The device object stays sealed. If local completion cannot be
confirmed before the deadline, it reports a cleanup timeout and the run keeps its first failure. A stalled local
filesystem operation can still outlive the wait. Reaping a provider-owned ADB restart process can still delay
provider termination. D52 governs signal exit. Queue and pool barrier timeouts are a separate work package.

## Evidence

The fake-adb tests in `crates/marathon-android/tests/support/bounded_cleanup.rs` commit a batch, hold the next
batch in a trickling pull, and fail the provider channel. An injected artifact listener holds the real sync
transfer with a protected cancellation signal. The tests drive the vendor clock at the 55-second force point
and hold marker removal inside the five-second reserve. The assertions cover EOF, a joined pull, listener removal,
refusal of a new request, disappearance of staged data and the original cause with the committed report entry by
65 virtual seconds. The witnesses use default and disabled idle timeouts. A separate device-owner case verifies
another serial's requests before and after the forced close on the same factory. The fake TCP transport uses real
time. Admission and timeout assertions use the run's virtual clock.

On the scratch checkout at `f7a02f59`, both whole-run witnesses reached 65 virtual seconds with the run still
pending, and the sibling witness found the stopped device's factory still open. The existing D41 test for a
batch ending after a disconnect also verifies that retirement lets the monitor acknowledge the update.
Each of the three concurrency witnesses passed 50 repetitions and an additional run with 18 CPU load workers.
Real-device behavior and the Linux integration gate belong to the integration run.

### Golden diffs and incidental bugs

No golden changes. The fake server scripts for cancelled recorder tests accept the added best-effort shutdown
probe. The source-inventory and parity-matrix validators report the same drift on the frozen Kotlin checkout
at the branch base. Neither generated inventory was changed.
