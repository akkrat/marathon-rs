# WP x128 — Recorder stop command diagnostics (U290)

D102's 1,250 ms graceful stop deadline can cancel an in-flight shell command. The shell catch
logged that cancellation at ERROR even when the reserved KILL phase confirmed exit and the
video was delivered. Recorder stop command failures now keep their cause at DEBUG. The final
WARN and ERROR outcomes, cancellation handling, budgets, fallback and D92 cleanup are unchanged.

The fake ADB witness gates a PID probe until the virtual graceful deadline fires, then releases
the KILL phase's confirmation probe. It checks the published movie bytes and the DEBUG timeout
record, with no stop ERROR. Separate cases still require WARN when a known PID stays live after
KILL and ERROR when the process listing is malformed, with no pull in either case. On the
unmodified base, the new witness delivered the movie and then failed on the 1,250 ms ERROR.
With the fix it passed 50 repeated runs and one more under CPU load.
