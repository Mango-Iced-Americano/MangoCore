# T9: retire obsolete boot compatibility paths

## Deletion ledger

| Removed | Why | Replacement | Verification |
|---|---|---|---|
| `preload_payloads`, `preload_app.S`, `flush_preload()` | Duplicate embedded ELFs overwrote CPIO content. | `build.rs` generated `OUT_DIR/initramfs.S` and embedded CPIO. | RV64/LA64 build; RV64 ktest 18/18; RV64 regression 6/6. |
| `block_mem`, `load_img*.S`, `MemBlockWrapper`, high-address copy | No Makefile or CI selects it. | `block_virt` / `block_virt_pci` and PID1 disk policy. | RV64/LA64 build; RV64 ktest; RV64 regression. |
| `legacy_block_root` and ramfs-only block bypass | Empty feature with no build consumer. | Canonical initramfs root. | RV64/LA64 build; RV64 ktest; RV64 regression. |
| static `initramfs-*.S` | Unreferenced hard-coded CPIO stubs. | `MANGO_INITRAMFS_CPIO` → `OUT_DIR/initramfs.S`. | Dual-arch build, source-purity contract, RV64 ktest/regression. |

## Environment

- Docker container: `c238e449081e`; workspace mount: `/home/pxy/projects/MangoCore-cleanup -> /app`.
- Logs: `docs/Work_Log/evidence/2026-07-23/t9-*.log`.
- Normal boot was not run: only compressed `sdcard-{rv,la}.img.xz` inputs are present; no usable x0/x1 runtime pair exists.

## Deferred

`zero_init` remains: it is a dormant memory-initialization experiment, not a boot compatibility path; its selected default remains the safe zeroed-frame behavior.
