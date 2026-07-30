# B26 LoongArch ASID+VPN 精准 shootdown 证据摘要

## 1. 证据范围

- 状态：`pass`
- 冻结 HEAD：`8a5a875741019c23a7593c367bc7d319174c4ee6`
- 冻结 tracked diff SHA-256：
  `c6c365d2ce660341c85e857f00c0b21a5114f03cd00b0878fa7b97ef879e4663`
- untracked content SHA-256：
  `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`
- 配置：`CORE_NUM=8`、`KTEST=smp`、`KREPEAT=1`、`PROFILE=normal`
- 本摘要只证明 B26 冻结源码的双架构编译、8 核 focused SMP 测试、slot 并发隔离与
  既有 frame/ASID 生命周期用例；不外推连续 range、RV64 MM-owned ASID、初赛全量或
  普通用户任务迁移。

## 2. Docker 环境

- 容器：`mangocore-smp-integration-20260725-os-dev-1`
- 容器 ID：
  `a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`
- 镜像：`zhouzhouyi/os-contest:20260510`
- 挂载：
  `/home/lzm/projects/MangoCore-smp-integration-20260725 => /app`
- DeepSeek、dispatcher 和完整 child logs 均位于本地忽略的 `cc-codex/`，未纳入提交或
  远端仓库；这里只归档人工复核后的结果与哈希。

## 3. 串行命令与结果

四项由本地受限 Docker runner 严格串行执行：

```text
docker exec mangocore-smp-integration-20260725-os-dev-1 bash -lc \
  'cd /app && make kernel ARCH=rv64 PROFILE=normal CORE_NUM=8'
docker exec mangocore-smp-integration-20260725-os-dev-1 bash -lc \
  'cd /app && make kernel ARCH=la64 PROFILE=normal CORE_NUM=8'
docker exec mangocore-smp-integration-20260725-os-dev-1 bash -lc \
  'cd /app && make ktest ARCH=rv64 PROFILE=normal CORE_NUM=8 KTEST=smp KREPEAT=1'
docker exec mangocore-smp-integration-20260725-os-dev-1 bash -lc \
  'cd /app && make ktest ARCH=la64 PROFILE=normal CORE_NUM=8 KTEST=smp KREPEAT=1'
```

| child job | recipe | 秒 | exit | timeout | forbidden | mutation | 结果 |
|---|---|---:|---:|---|---:|---|---|
| `agent-441d4e4f126c-r01-rv64-kernel-build` | RV64 build | 127.178 | 0 | false | 0 | false | PASS |
| `agent-441d4e4f126c-r02-la64-kernel-build` | LA64 build | 140.670 | 0 | false | 0 | false | PASS |
| `agent-441d4e4f126c-r03-rv64-ktest` | RV64 SMP ktest | 135.939 | 0 | false | 0 | false | 20/20 PASS |
| `agent-441d4e4f126c-r04-la64-ktest` | LA64 SMP ktest | 145.016 | 0 | false | 0 | false | 20/20 PASS |

## 4. 原始日志锚点与哈希

两架构 QEMU 日志均包含：

- `configured=8`、`online_mask=0xff`
- TAP plan `1..20`
- `ok 16 smp::user_tlb_page_sync_uses_arch_backend`
- `ok 17 smp::concurrent_page_shootdowns_keep_payloads_separate`
- `[KTEST RESULT: PASS]`

LA64 还包含 `[machine_init] user ASIDs: 1023`。原始 stdout/stderr 只保存在本地，哈希为：

| child job | stdout SHA-256 | stderr SHA-256 |
|---|---|---|
| RV64 build | `662d6932067be75001f1c291e4538bd800a876819b30a3924b47d897e2ae96d7` | `bd9d6b4eb87a19fe41ab3f14cd6833b39f8e2b4f598657eca3dabf25f245fe91` |
| LA64 build | `a4a563e94ac8c8d2ddd01445264983a86a17afb4fa9ca49b977b02bc2758bdd8` | `d66969ddba6e443533dd3bec2f3ed9f9ffe1d7869618d030435de1581a6c1f39` |
| RV64 ktest | `3f63a4a8389cd1de40dc7b5d2d8643db34394e96a6d615f7ef66afcc22664129` | `aa2b3abbafb950a8f2708b760300c95f39a495d6945ba5ed7a22252775413b9a` |
| LA64 ktest | `2eb049422384aa71f2aef6b1e5881047c63d94259a6b94bfd5026e9dde26f1ac` | `ad1f80f637b4185386c0317a2ca64237054c64343610ac21a59d16bc31e7af29` |

## 5. 审查结论与人工修正

DeepSeek 只读审查认为以下闭环无阻断问题：VM 锁内冻结 ASID/VPN；锁外 IPI/ack；每发起
CPU 固定 slot；STOP/timeout fail-stop；LA64 `invtlb 0x5` 页对对齐；ack 后释放退休 frame。

GPT/Codex 逐项复核源码和日志，并修正一个证据表述：并发测试使用的是同一个
`AddressSpace`，即同一 ASID 下不同 VPN，而不是“不同 ASID/VPN”。它直接证明多个发起者
共享 reason bit 时 slot payload 不覆盖；MM-owned ASID 与 epoch rollover 分别由
`address_space_owns_asid` 和 `loongarch_asid_rollover_flushes_before_reuse` 验证。

最终判定：B26 在本摘要限定范围内为 `pass`。
