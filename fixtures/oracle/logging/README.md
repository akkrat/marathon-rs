# Reference logging captures

Generated from Kotlin marathon `e47352f2`, logback-classic 1.5.8, Jansi 2.4.1,
SLF4J 2.0.15, Kotlin logging 3.0.5 and JBR 21.0.10. The pinned distribution
libraries determine the versions; no expected output is handwritten.

```sh
tools/oracle/build.sh reference
python3 tools/oracle/logging-corpus.py
```

- `console.jsonl`: 36 real logger/ConsoleAppender captures: startup, Android,
  Apple normal and Apple compact × terminal, pipe and file × default, force and
  strip mode. Includes every level, multiline text, Unicode, embedded SGR/OSC,
  Timings and Xcodebuild. Terminal output processing is disabled; the captured
  bytes include Jansi's final terminal reset, including in strip mode.
- `state.jsonl`: 16 real logger state observations, covering startup hierarchy,
  repeated lookups, debug changes, explicit levels and all six Apple filters.
  State rows use a separate file because replacing a Jansi console appender can
  close its wrapped stdout stream.
- `details.jsonl`: 56 additional logger abbreviations, 21 SLF4J substitutions and
  32 Jansi parser captures, including private modes, OSC hyperlinks, charset
  selection, unknown/incomplete escapes, true colour and the 100-byte buffer.
  Abbreviation outputs pass through the JVM UTF-8 encoder: an unpaired surrogate
  produced by abbreviating a supplementary character becomes `?`.
  A further throwable row records two nested causes with a shared trailing
  frame and logback's `\t... 1 common frames omitted` at each cause.
- `hierarchy.jsonl`: 36 generated observations for `$`/`.` inheritance, mixed
  case root aliases, canonical handle/emitted names, and mutable root levels.
- `failures.jsonl`: 18 raw Jansi write observations: overflowing SGR/CHA,
  private erase options, and a nonfailing private SGR control. Each sequence
  receives two later writes; output is cumulative and exception classes are
  captured separately. Overflow stays broken; private-option failures recover.
- `failure-console.jsonl`: 27 real compact ConsoleAppender captures (overflowing
  SGR, overflowing CHA, private erase × three targets × three Jansi modes), with
  two following events and the actual process exit code. Overflow parsing also
  runs in force mode. Terminal uninstall after overflow exits 1 without reset
  bytes; pipe/file shutdown exits 0. Private erase recovers on the next event.
- `fallback.jsonl`: 10 real KLoggerDebug calls and three backend-warning captures,
  including suppressed/once-only warning behavior and the missing-spread bug.

`LoggingProbe.kt` uses the actual vendor configurators and logging helpers. It
replaces only the event clock, using the same delegation scheme as
`hooks/Logging.kt`, and leaves messages, logger names, levels, layouts, encoders
and Jansi stream handling intact. The event time is epoch zero; local timezone
is UTC. `TERM=xterm-256color`, UTF-8, and an explicit reference CLI logback.xml
avoid duplicate-resource diagnostics from the oracle's extra test-vendor jar.

The existing 200-row `../jcompat/logback.jsonl` remains the larger logback layout
and abbreviation corpus. Its throwable input schema now records causes; its
200 expected output fields are unchanged.
