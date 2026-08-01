# FDT Snapshot ktest — Durable Verification Evidence

> **归档路径:** `docs/Work_Log/evidence/2026-07-30/fdt-snapshot-ktest.md`
> **生成日期:** 2026-07-30
> **父工作日志:** `docs/Work_Log/2026-07-30.md` — "RV64 DTB pre-BSS 持久化快照修复"

---

## Git 元数据

```
git describe --always --dirty: aa46a09b-dirty
```

## 容器隔离证据

**Container ID:** `79040401ef8a`

**Mount 映射**（`docker inspect 79040401ef8a --format '{{range .Mounts}}{{println .Source "->" .Destination}}{{end}}'`）:
```
/root/projects/MangoCore -> /app
```

宿主机工作目录 `/root/projects/MangoCore` 挂载到容器 `/app`，所有编译和测试产物均在容器内产生。

---

## 配置校验

**`os_test.conf` SHA-256:**
```
f92b83ed13bd28eec1c0903a8a3441f9c6c4cc252f4eda13226ca7b15a6a9218
```

注：ktest 模式不使用 `os_test.conf` mask，而是通过 `KTEST=platform_fdt_snapshot` 选择测试组。该 SHA 校验来自构建注入配置（与测试无关，仅归档一致性）。

---

## 执行的命令

### 1. 工具链 preflight（只读检查）

```bash
make toolchain-preflight
```

### 2. QEMU ktest — RV64 FDT snapshot

```bash
make -C os ktest-run ARCH=rv64 PROFILE=normal KTEST=platform_fdt_snapshot KTEST_QEMU_TIMEOUT=60
```

**Exit status:** `0`（成功）

### 3. 双架构内核编译验证（串行）

```bash
make kernel ARCH=rv64 PROFILE=normal
make kernel ARCH=la64 PROFILE=normal
```

**Exit status:** 两命令均为 `0`

---

## QEMU 串口输出（ktest 相关部分）

```
[kernel] Boot protocol: RiscvFdt, hart_id=0, dtb_paddr=0x82200000
[kernel] Console initialized.
[memory] 193625 usable physical frames across 1 region(s)
[memory] region0: [0x90ba7000, 0xc0000000) frames=193625
.data [0x80467000, 0x80b45000)
.bss [0x80b45000, 0x90ba7000)
[kernel] Hello, world!
TAP version 13
# arch: riscv64
# mode: ktest
# repeat: 1
# timeout_ms: 5000
# failfast: false
1..4
ok 1 platform_fdt_snapshot::preserves_live_vf2_mmc_node_shapes
ok 2 platform_fdt_snapshot::rejects_absent_and_malformed_raw_resources
ok 3 platform_fdt_snapshot::exact_compatible_rejects_malformed_raw_snapshot
ok 4 platform_fdt_snapshot::captures_qemu_boot_fdt_raw_properties
# results: 4 passed, 0 failed, 4 total
[KTEST RESULT: PASS]
# ktest: shutting down.
```

**关键观测点：**
- `Boot protocol: RiscvFdt, hart_id=0, dtb_paddr=0x82200000` — 内核正确识别 FDT 协议和 DTB 物理地址
- TAP `timeout_ms: 5000` — 使用 ktest-run 默认每用例超时（`KTEST_QEMU_TIMEOUT=60` 是 QEMU 整体超时）
- `1..4` — 全部 4 个测试用例通过
- `[KTEST RESULT: PASS]` — ktest 框架级成功标记
- `# ktest: shutting down.` — 正常关机

---

## 快照地址 `readelf -sW` 验证

```bash
$ readelf -sW build/rv64/release/normal/kernel/kernel-rv | grep -E "FDT_SNAPSHOT| sdata$| edata$| sbss$| ebss$"
```

结果（等效于 Docker 内输出）:
```
符号名          地址             大小       类型  绑定   Vis       索引 名称
FDT_SNAPSHOT    0000000080944038 00200008  OBJECT GLOBAL DEFAULT    3 mangocore::hal::firmware::FDT_SNAPSHOT
sdata           0000000080467000 00000000  NOTYPE GLOBAL DEFAULT    2
edata           0000000080b45000 00000000  NOTYPE GLOBAL DEFAULT    3
sbss            0000000080b85000 00000000  NOTYPE GLOBAL DEFAULT    4
ebss            0000000090ba7000 00000000  NOTYPE GLOBAL DEFAULT    4
```

快照 `FDT_SNAPSHOT` 位于地址 `0x80944038`，大小 `0x200008`（2 MiB + 8 字节元数据），section index 3（`.data` 段）。验证它位于 `sdata`（section 2）和 `edata`（section 3）之间，且早于 `sbss`（section 4，`0x80b85000`）。`.data.boot` 是 `link_section` 标注而非独立 ELF section；BSS 范围 `[0x80b85000, 0x90ba7000)`，快照不在其内。

---

## 双架构编译验证

| 架构 | 命令 | 结果 |
|------|------|------|
| RV64 | `make kernel ARCH=rv64 PROFILE=normal` | ✅ 编译通过（含既有项目/编译器 warning） |
| LA64 | `make kernel ARCH=la64 PROFILE=normal` | ✅ 编译通过（含既有项目/编译器 warning） |

`make lint` 本次未执行。

LA64 不受影响：它走 `LoongArchLegacy` 协议路径，`has_valid_dtb()` 返回 `false`，始终使用静态 fallback。`init_platform_info()` 的 `build_platform_info()` 对非 `RiscvFdt` 协议返回 `None`，`UbootGo` 同样走静态 fallback。

---

<!-- End of evidence record -->
