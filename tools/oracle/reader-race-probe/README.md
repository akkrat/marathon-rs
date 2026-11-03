# Reader race probe (U145)

`tools/oracle/reader-race-probe/run.sh` runs Kotlin's `produceLinesManually`
(`vendor/vendor-apple/base/.../extensions/Reader.kt`) from the read-only source
checkout's already-built CLI distribution (`MARATHON_KT` overrides its path)
with the reference JDK. It does not build or modify the source checkout.

The probe's stream stands in for sshj's `ChannelInputStream`. In the second
case the command's whole stdout and then EOF arrive after the poll that saw
`available() == 0` and before `canRead()` (`cmd.isOpen && !cmd.isEOF`) is
evaluated, which is when sshj's reader thread can deliver them. Expected
output on the frozen source:

```
stdout and EOF before the first poll: [HOME=/Users/worker, USER=worker]
stdout and EOF between available() and canRead(): []
```

The Rust counterpart is
`crates/marathon-apple/tests/reader.rs::output_arriving_with_eof_between_an_empty_read_and_can_read_is_never_read`.
Outcome and discussion: U145 in `docs/found-bugs.md`. Not a CI fixture.
