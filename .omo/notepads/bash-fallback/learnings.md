# bash-fallback learnings

## Change made
- Added `/bash` fallback in `run_group_once()` child process in `user/src/bin/initproc.rs`
- Used same closure pattern as `run_bash_cmd()` (lines 93-100) for constructing `argv`
- The `exec()` function is from `user_lib::usr_call`: `pub fn exec(path: &str, args: &[*const u8], envp: &[*const u8]) -> isize`

## Build notes
- User-space code can be compiled with `cd user && make build BOARD=rvqemu MODE=release`
- rv64 user programs use `riscv64gc-unknown-none-elf` target with LLVM linker — compiles without external toolchain
- la64 user programs use `loongarch64-unknown-linux-gnu` which requires `loongarch64-linux-gnu-gcc` — fails when missing, but this is pre-existing
- `kernel-build-only` targets only build kernel (`os/` crate), not user programs
