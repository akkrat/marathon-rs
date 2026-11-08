# Native-string writer expectations

`writer_native_strings.jsonl` contains the D55 cases whose outputs change when lone UTF-16
surrogates decode to U+FFFD. Generated independently by `tools/oracle/writer-probes/run --native-strings`
using the same pinned JVM libraries as `fixtures/oracle/jcompat/writer_edges.jsonl`; retain only rows
which differ from that original table. Raw input units remain in each row. All other rows still
compare byte-for-byte with the unchanged original oracle.
