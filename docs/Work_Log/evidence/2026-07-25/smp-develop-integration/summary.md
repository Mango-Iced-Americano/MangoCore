# SMP 分支合并 develop 集中验证摘要

## 源码与环境

- SMP parent: `43c2c5e009196437d3a6164011c4b96032499d60`
- develop merge head: `88abaa1f5c8810975a9222159c2239de0d4030e4`
- 契约修正后的 merge index tree: `0b541b90dc3bbc68cd87945bdd28e83d3b8e28d4`
- Docker container:
  `a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`
- Docker image:
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- Repo digest:
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- Image created: `2026-05-10T08:46:16.065707166Z`
- Toolchain: `rustc 1.97.0-nightly (82bee9650 2026-05-09)`
- QEMU: RV64/LA64 均为 `10.0.2`
- Mount:
  `/home/lzm/projects/MangoCore-smp-integration-20260725 -> /app`

独立 Git worktree 的 `.git` 文件指向宿主主仓库中未挂载的 worktree
metadata，因此容器内 Git 不可用；构建和 QEMU 只消费 `/app` 源码，源码
parent、merge head、index tree 和最终提交由宿主 Git 记录。

## 集中验证结果

| 验证 | 结果 | 关键证据 |
|---|---|---|
| RV64 `CORE_NUM=2` kernel build | PASS | release 内核完成链接 |
| LA64 `CORE_NUM=2` kernel build | PASS | release 内核完成链接 |
| RV64 `CORE_NUM=2 KTEST=smp` | PASS | `online_mask=0x3`，2/2 |
| LA64 `CORE_NUM=2 KTEST=smp` | PASS | `online_mask=0x3`，2/2 |
| RV64 `CORE_NUM=1 KTEST=smp` | PASS | `online_mask=0x1`，2/2 |
| QEMU 拓扑与非法值 | PASS | 双架构显式 2 核；`CORE_NUM=3` 解析期失败 |
| QEMU command matrix | PASS | 双架构 profile/drive/fail-closed 契约通过 |
| canonical entrypoint contract | PASS | 双架构 normal/regression facade 契约通过 |
| Make layering contract | PASS | settings 保持 declaration-only，默认目标与 facade 通过 |

完整命令输出保存在同目录的本地 `.log` 文件中；这些日志受仓库
`.gitignore` 的 `*.log` 规则保护，不进入提交。本文摘要记录可提交的关键
判定与环境指纹。

## 验证中发现并修正

1. develop 删除了 `run_test.sh`，但 QEMU command matrix 仍要求遗留入口
   fail-closed 并指向串行 runner。恢复了只输出迁移提示并退出 64 的 shim，
   未恢复旧并行测试实现。
2. 初版 SMP `CORE_NUM` 条件块违反 arch settings 的 declaration-only
   合同。改为立即展开的变量声明，仍在 Make 解析期拒绝非法值。
3. Make layering 的默认目标检查把 `export VAR := value` 误识别成 target。
   检查器现显式忽略 `export/unexport` 声明。

后两项只改变 Make 解析和测试契约，不改变已经通过双架构构建与 QEMU 的
内核运行语义；修正后重新运行拓扑、QEMU command、entrypoint 和 layering
契约，结果全部 PASS。
