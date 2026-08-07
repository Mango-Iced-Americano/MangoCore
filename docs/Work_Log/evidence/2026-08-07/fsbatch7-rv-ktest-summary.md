# FS batch 7 RV64 ktest

- Command: `make -C os ktest-run ARCH=rv64 PROFILE=normal`
- Result: `66 passed, 10 skipped, 0 failed, 76 total`
- Status: `[KTEST RESULT: PASS]`
- Relevant tests: `ext4_another_lifetime::partial_reclaim_runs_final_barrier`, `page_cache::global_flush_releases_registry_before_writeback`, and `page_cache::writeback_retries_transient_eagain` all passed.
