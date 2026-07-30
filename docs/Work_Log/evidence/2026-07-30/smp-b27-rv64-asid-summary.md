# SMP B27 RISC-V MM-owned ASID 证据摘要

## 1. 证据范围与状态

- 阶段状态：`partial`
- 基线 HEAD：`93e4cd408103d8a0eaf4e64f8325118872a1615b`
- 最终验证冻结 tracked diff SHA-256：
  `381c993527dade83b19db5021e1f6b407fb4b80031f08953dd329fddb24a97ca`
- 最终生产源码 diff SHA-256：
  `14a0f05bccdc10f832751029ffaff2d38eb1bbc55a36a792b6d3b6ed7f6c9528`
- 冻结时 untracked content SHA-256：
  `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`
- 功能门禁通过；仓库 warning baseline 漂移导致 `make lint` RED，所以本摘要不写 `pass`。
- 本摘要不外推 ASIDLEN=0 的动态硬件验证、连续 range、cached CPU detach、普通用户迁移
  或 30 分钟混合压力测试。

## 2. 环境

- 容器：`mangocore-smp-integration-20260725-os-dev-1`
- 容器 ID：
  `a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`
- 镜像：`zhouzhouyi/os-contest:20260510`
- 镜像 ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- Repo digest：
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- 镜像创建时间：`2026-05-10T08:46:16.065707166Z`
- RV64/LA64 QEMU：`10.0.2`
- bind mount：`/home/lzm/projects/MangoCore-smp-integration-20260725 => /app`
- DeepSeek 协作数据与完整原始日志只位于本地忽略目录 `cc-codex/`，未提交或上传。

## 3. 上游语义与人工裁决

上游依据：

- RISC-V Supervisor specification：
  <https://docs.riscv.org/reference/isa/v20260120/priv/supervisor.html>
- SBI RFENCE extension：
  <https://docs.riscv.org/reference/sbi/v3.0/ext-rfence.html>
- Linux RISC-V ASID allocator：
  <https://raw.githubusercontent.com/torvalds/linux/master/arch/riscv/mm/context.c>
- Linux RISC-V TLB flush：
  <https://raw.githubusercontent.com/torvalds/linux/master/arch/riscv/mm/tlbflush.c>

冻结的不变量：

1. 内核 ASID 固定为 0，用户硬件 ASID 从 1 开始。
2. ASID 归 MM 的 `TlbContext` 所有，不归线程/TCB 所有。
3. 一个 epoch 内编号只增不减；复用前必须先完成全 online CPU flush/ack。
4. PTE 修改在 VM 锁内冻结 generation/targets/ASID/range，锁外执行同步和 frame 退休。
5. rollover ack 之后禁止重新安装旧 context；RV64 trap-return 交接窗口必须 IRQ-off。
6. ASIDLEN=0 时继续使用 ASID 0，并在每次用户/内核 SATP 切换后全刷。

DeepSeek 的设计审查正确发现 SBI a4/FID 2、用户 SATP 编码和公共 `asid_context` 三个缺口；
但它认为 flush 后再次安装旧 epoch context 仍然安全。GPT/Codex 按
`snapshot -> handler -> ack -> install -> user access` 时序复核后否决该结论，并增加
IRQ-off 断言与汇编边界。DeepSeek 最终报告还误称 lint 修改了 baseline；人工核对四个
baseline mtime 均为 7 月 25 日，实际变更是 lint 脚本遗留的三个临时 ELF stub。

## 4. 最终冻结命令与结果

所有构建和运行均通过本地受限 gateway 在上述 Docker 容器中串行执行。两项 preliminary
recipe 内部各自先执行对应架构 normal kernel build，再注入临时 `mask=0x003` 配置。

| child job | recipe | 秒 | exit | timeout | mutation | 结果 |
|---|---|---:|---:|---|---|---|
| `agent-c4cdebf2ff40-r01-rv64-ktest` | RV64 8 核 SMP focused | 136.164 | 0 | false | false | 20/20 PASS |
| `agent-c4cdebf2ff40-r02-rv64-preliminary` | RV64 8 核初赛 | 334.122 | 0 | false | false | 312/314 |
| `agent-c4cdebf2ff40-r03-la64-preliminary` | LA64 8 核初赛 | 347.765 | 0 | false | false | 308/314 |
| `agent-c4cdebf2ff40-r04-lint` | 四格 lint（停于 RV64 debug） | 15.473 | 2 | false | true | baseline/tool RED |

RV64 focused 关键锚点：

```text
[mm] RISC-V ASID allocator: 65535 user IDs
[smp] SBI RFENCE enabled for user page shootdown
[smp] minimal boot ready: configured=8 boot_hw_id=7 online_mask=0xff
1..20
[KTEST RESULT: PASS]
```

测试 14—18 分别覆盖 MM-owned ASID、flush-before-reuse rollover、架构页级后端、并发
payload 隔离和 ack 前 frame 不退休。测试直接驱动生产 allocator，但不把测试字段加入
生产 ASID 对象。

初赛失败身份：

| 架构 | basic-musl | basic-glibc | busybox-musl | busybox-glibc | 总分 |
|---|---:|---:|---:|---:|---:|
| RV64 | 102/102 | 102/102 | 54/55（`busybox kill 10`） | 54/55（同项） | 312/314 |
| LA64 | 100/102（`test_brk`） | 100/102（同项） | 54/55（`busybox kill 10`） | 54/55（同项） | 308/314 |

两架构均出现四个 group END 和 `[initproc] run_selected_groups done`；无 panic、fatal trap、
timeout 或 runner failure，失败集合与既有递增基线一致。

支持性冻结轮次（tracked diff
`76428095c6c286d320ac68cc4567190234eb856f724fce9e53d509fd2c94713b`）还得到：

- RV64 normal build：128.784 秒，exit 0；
- LA64 normal build：142.997 秒，exit 0；
- RV64 SMP focused：138.397 秒，20/20 PASS；
- LA64 SMP focused：143.151 秒，20/20 PASS，`online_mask=0xff`。

该轮早于最终注释/rustfmt 和 RV64 trap-return 断言，只作为 LA64 共用字段/测试重构的
补充证据；最终源码的双架构 build 由两项 preliminary recipe 覆盖。

## 5. 最终 ELF 指令证据

最终 RV64 ELF：`/app/build/rv64/release/normal/kernel/kernel-rv`。

trap 入口在 `0x802010b0` 读取旧 SATP 并提取 16-bit ASID；`csrw satp,t0` 后仅当 ASID
为 0 执行 `sfence.vma`。恢复入口在 `0x802010c8` 对 a1 中的完整用户 SATP 做同样判断，
最终 `sret` 位于 `0x80201176`：

```text
802010b0: csrr  t2,satp
802010b4: slli  t2,t2,0x4
802010b6: srli  t2,t2,0x30
802010ba: csrw  satp,t0
802010be: bnez  t2,802010c6
802010c2: sfence.vma

802010c8: slli  t0,a1,0x4
802010cc: srli  t0,t0,0x30
802010d0: csrw  satp,a1
802010d4: bnez  t0,802010dc
802010d8: sfence.vma
```

页级路径生成双操作数指令，例如 `0x802d393e: sfence.vma a0,a1`；IPI fixed-slot handler
同样生成 `sfence.vma va,asid`。SBI RFENCE 路径在 `0x80299b80` 附近生成：

```text
a7 = 0x52464e43
a6 = 2
a1 = 0
a3 = 0x1000
a4 = s2        # frozen ASID
ecall
```

这证明最终机器码不是仅在 Rust 源码层“看起来传了 ASID”。

## 6. 原始日志哈希

| job | stdout SHA-256 | stderr SHA-256 |
|---|---|---|
| RV64 focused | `3e3ac2c4fe8d2f184590f5c778b891af360e2e6aa7cc836d45fefbd30c03fe49` | `9fb5b56b4e8541b25d6b246026b62524653e23fce6c25fa1b1e66e586c12dde7` |
| RV64 preliminary | `092b72e7b5c07d3bd4c2488e1285c9fdd9fff47520cbea77633504edd333b677` | `923d22e1339233e4ed39f92b9de68ec13e685316132b47b97d2ccd35472dcfaf` |
| LA64 preliminary | `833c16d6b0c85b973601ec463803ec5cdada712d845853b99efee4ef7ecb76e7` | `640870510cf602f1d77aca665339b82ed13ab6df469dd38adea710e7dbd2470b` |
| lint | `5bede2307d46c1e526205149ee4fba98eb5bde4d0340d94a39db70ca4c3d9c6e` | `e22f7453b98f9b1edf46eb8c678055dc6871c1e3195ddf7cd9fbe5940b02098c` |
| RV64 raw lint diagnosis | empty | `13c1c603a79a6d54fb1779dec64b4c0c48be4016cf670e250047fbdff6325e0e` |

## 7. lint RED 与清理记录

`make lint` 的 RV64 debug baseline 同时出现四个新增 tuple 和五个已消失 tuple，因此按当前
fail-closed 规则退出。定向 raw check 将四项还原为：

- `drivers/rng/mod.rs`：未构造 enum variant，`dead_code`；
- `fs/ext4/bitmap.rs`：未使用函数，`dead_code`；
- `mm/user_mapper.rs`：未使用方法，`dead_code`；
- `smp.rs`：function item 直接转换为整数。

这些文件均不在 B27 diff 中。没有执行 `--capture-baseline`，也没有把 RED 伪装成 PASS。
Docker 内对 B27 修改到的七个 Rust 文件执行定向 `rustfmt --check` 为 exit 0；全仓
`make -C os lint-format` 因 B27 之外的大量既有格式漂移退出 2，因此没有批量格式化或把
无关改动混入本节点。
`lint-check.sh` 为 build.rs 生成的三个 root-owned 68-byte stub 会落在
`user/tools/rv64/{bin,lib}`；它们在冻结前不存在，本轮通过容器删除了精确路径，清理后
tracked/untracked 指纹恢复。后续应把“只清理本轮创建的 stub”加入脚本 trap，并在独立
提交中人工审查 baseline 更新。

最终判定：B27 功能证据通过；仓库级 lint 仍为 `partial`。
