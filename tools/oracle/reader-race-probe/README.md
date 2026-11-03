# Reader race probe (U145)

`tools/oracle/reader-race-probe/run.sh` runs Kotlin's `produceLinesManually`
(`vendor/vendor-apple/base/.../extensions/Reader.kt`) from the read-only source
checkout's already-built CLI distribution (`MARATHON_KT` overrides its path)
with the reference JDK. It does not build or modify the source checkout.

The probe's stream stands in for sshj's `ChannelInputStream`. In the second
case the command's whole stdout and then EOF arrive after the poll that saw
`available() == 0` and before `canRead()` (`cmd.isOpen && !cmd.isEOF`) is
evaluated, which is when sshj's reader thread can deliver them. Kotlin drops
both lines; the Rust port reads them under D19 (PLAN §3.11).

Recorded output: `expected.txt`, on JBR 21.0.10 (see `tools/oracle/README.md`)
with the reference distribution's jars, built from source commit `d511c021`.
`Reader.kt`, `LineBuffer.kt` and `PollBackoff.kt` are byte-identical at that
commit, at the frozen base `e47352f2` and at the release source `c95b2128`.

The Rust regression test for D19 is
`crates/marathon-apple/tests/reader.rs::output_arriving_with_eof_between_an_empty_read_and_can_read_is_still_delivered`.
Discussion: U145 in `docs/found-bugs.md` and `docs/wp/x9-findings.md`. Not a CI fixture.
