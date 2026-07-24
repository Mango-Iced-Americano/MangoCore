# another_ext4 DragonOS comparison reference

- **DragonOS source commit:** `45931ee3b3e66892533563f73023021a83f89b2d`
- **Extracted subtree commit:** `571b85084fade21f5c26726a78e71356210c4f86`
- **Local dependency HEAD during capture:** `bdfd711f2116fe48adae293ccdeaa96d61f63aed`
- **Origin remote:** `git@github.com:Mango-Iced-Americano/another_ext4.git`
- **Provenance record:** `dependency/another_ext4/UPSTREAM.md`; extracted history is published as `dragonos` and `sync/dragonos-monorepo`.

## Comparison notes

- The direct-range planning/rejection diagnostics are Mango-side optimization instrumentation: a search of the extracted DragonOS subtree found no `direct_range_plan` or `try_prepare_direct_range` symbol. The baseline therefore measures Mango's eligibility gate rather than an upstream throughput implementation.
- DragonOS-derived `another_ext4` has JBD2 recovery semantics: replay writes home blocks and flushes them before it writes and flushes the clean journal superblock. It also rejects replay to physical journal blocks. The direct path must preserve this persistence ordering.
- The observed `before_eof` rejection followed by `no_journal_range` fallback is consistent with retaining buffered/journalled handling when the planned range is not an append-only range with a suitable journal range. No direct write was accepted in this capture.
