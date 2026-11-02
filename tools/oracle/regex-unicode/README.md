# JDK 21 regex Unicode tables

Run `tools/oracle/regex-unicode/generate.sh [output.tsv.gz]` using the reference JDK
(`JAVA_HOME`, default Android Studio's JBR). It refuses any major version other than 21.
The checked-in gzip file is deterministic and contains Unicode 15.0 data from that JDK.
No network or host Rust Unicode tables are used by the translator.

The generator calls JDK `CharPredicates` for every general category, binary/POSIX
property and `java*` property, with both CASE_INSENSITIVE states. Script enum values
and the JDK's complete ISO alias map, and the block map (including its exact accepted
space/underscore grammar and renamed aliases), come from `Character`. The remaining
records hold `Character.toUpperCase` / `toLowerCase` simple mappings and character
names for distinguishing valid `\N{...}` from syntax errors. Access to these internal
APIs requires the two `--add-opens` options in the script; this is an oracle build tool,
never a runtime dependency.

The tab-separated records use `P0/P1` (general/Java properties), `U0/U1` (binary),
`X0/X1` (Unicode POSIX), `S` and `B` (script/block ranges), lower-case `s` and `b`
(aliases), `C` (simple mappings) and `N` (names). Ranges are inclusive hexadecimal
pairs. `spikes/java-regex/build.rs` expands the embedded file at build time; runtime
property expansion always emits explicit scalar ranges, omitting the surrogate interval.
