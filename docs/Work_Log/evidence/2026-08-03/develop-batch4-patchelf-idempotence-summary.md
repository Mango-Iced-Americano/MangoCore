# develop Batch 4.1 工具 ELF 幂等化证据

## 结论

状态：`pass`。

双架构工具准备现在只在 ELF 装载合同不匹配时执行 `patchelf`。RV64 与 LA64 8 核初赛均通过，
且确定性 runner 证明 source-before/source-after 完全一致，Batch 4 暴露的 tracked binary
mutation 已关闭。

## 基线与环境

- 基线 commit：`bc1cae72`
- 工作树：`MangoCore-smp-integration-20260725`
- 容器：`mangocore-smp-integration-20260725-os-dev-1`
- 镜像：`zhouzhouyi/os-contest:20260510`
- image ID：`sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：`sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- RV64/LA64 QEMU：`10.0.2`
- 测试 diff SHA-256：
  `60126f026963195ce3851d330ea2c9bb419523cc838f61a680b9ce8032a3f66d`

## 修复合同

共享 `ensure_alpine_tool_elf(arch, interpreter)` 对两个 ext4 工具依次检查：

1. `patchelf --print-interpreter` 必须等于架构对应 `/tools/lib/ld-musl-*.so.1`；
2. `patchelf --print-rpath` 必须等于字面量 `$ORIGIN/../lib`；
3. `readelf -d` 必须显示 `DT_RPATH`，同值的 `DT_RUNPATH` 也会触发 `--force-rpath` 修复。

Make 中的 `$$` 经一轮展开后交给 shell；单引号继续保护 `$ORIGIN`，不会被当作环境变量。
读取失败产生空值并进入修复分支；真正的 `patchelf` 失败仍使 recipe fail-closed。

## DeepSeek 验证

- 父任务：`develop-batch4-patchelf-validation-r1-20260803`
- effort：`max`
- 权限：只读源码；仅允许两个 preliminary Docker recipe

| Recipe | 状态 | 用时 | QEMU | mutation | marker |
|---|---|---:|---:|---|---|
| RV64 `CORE_NUM=8 mask=0x003` | PASS | 376.025s | exit 0 | false | required 完整、forbidden 空 |
| LA64 `CORE_NUM=8 mask=0x003` | PASS | 373.063s | exit 0 | false | required 完整、forbidden 空 |

两架构 source before/after 的 HEAD、status、tracked diff 和 untracked content 指纹逐项一致。
功能摘要为 basic 21/21、busybox 107/108；唯一非满分项 `busybox kill 10` 属于人工接受基线，
不是 runner 或本批修改失败。

## 边界

- 本批证明正确缓存的 skip path 字节幂等；不声称网络下载源本身可复现。
- 测试没有通过禁用 fingerprint 变绿，mutation 检测保持启用。
- DeepSeek 的自然语言结论由 GPT/Codex 对照两个 child `result.json` 后接受。
