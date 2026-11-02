# WP 4.5a plist probes

JVM probes against **dd-plist 1.28** and the compiled Kotlin plist models of marathon
`e47352f2`, run on the reference JDK (Android Studio's JBR) against the Kotlin distribution jars
(`$MARATHON_KT/cli/build/install/marathon/lib`). They settle the behaviours WP 4.5a ports:
the byte-exact XML writer, the constructors' unconditional `null` writes, duplicate keys,
the broken nested-model-list read, the missing-key bugs, and every conversion the delegates apply.

```sh
tools/oracle/plist-probes/run.sh                # print the probe output
tools/oracle/plist-probes/run.sh out.txt        # or write it to a file, to diff against expected.txt
```

`expected.txt` is the recorded output the Rust implementation is matched against
(see `docs/wp/4.5a-findings.md` for the per-probe conclusions). Nothing here runs in
`scripts/check.sh`: it needs the Kotlin repository and a JDK.
