# develop Batch 1 融合验证摘要

- 日期：2026-08-03 10:15 +08:00
- 工作树：`/home/lzm/projects/MangoCore-smp-integration-20260725`
- 分支：`codex/smp-develop-integration`
- 基线：`f922aab6d8b44e7aadfabe7419c759d131fd7899-dirty`
- develop 来源：`f514cc3f659264c23efc076cf1c9511f422c6744`
- 容器：`a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`
- 镜像：`zhouzhouyi/os-contest:20260510`
- 挂载：`/home/lzm/projects/MangoCore-smp-integration-20260725 -> /app`

## 验证结果

| 命令/检查 | 结果 |
|---|---|
| `python3 -m py_compile scripts/score_test.py` | PASS |
| 评分器零样本列表、空结果和 `--table` focused 检查 | PASS |
| `bash scripts/test-make-layering-contract.sh` | PASS |
| 双架构 `mke2fs` ELF 类型与 program interpreter | PASS |
| `make toolchain-preflight` | PASS，`nightly-2026-05-10` |
| `make kernel ARCH=rv64 PROFILE=normal` | PASS |
| `make kernel ARCH=la64 PROFILE=normal` | PASS |
| `make lint` | FAIL，既有 SMP warning baseline 漂移 |

双架构编译在同一 Docker 会话内严格按照 RV64 → LA64 串行执行。新鲜产物：

- `build/rv64/release/normal/kernel/kernel-rv`：2026-08-03 10:10:18 +08:00，71,464,936 bytes。
- `build/la64/release/normal/kernel/kernel-la`：2026-08-03 10:12:31 +08:00，86,385,616 bytes。

`make lint` 报告的新 warning baseline 文件为：

- `os/src/drivers/rng/mod.rs`
- `os/src/fs/ext4/bitmap.rs`
- `os/src/mm/user_mapper.rs`
- `os/src/smp.rs`
- `os/src/syscall/process/time.rs`

上述文件均不在 Batch 1 diff 中，因此没有使用 `--capture-baseline` 掩盖独立问题。

## 未运行项目

- QEMU：NOT RUN。本批只融合 CI、runner 和工具盘基础设施，没有修改内核运行时核心路径。
- 完整 tools disk 重建：NOT RUN。该目标需要重新下载 Alpine/APK 内容；当前通过 Make 合同、
  已提交 ELF 架构、动态解释器及双架构 initramfs 编译验证其静态闭环。
