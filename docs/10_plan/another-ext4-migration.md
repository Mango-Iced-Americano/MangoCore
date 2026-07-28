# Phase 0/1：another_ext4 迁移与接入门禁设计

**状态：Phase 0 设计冻结；Phase 1 source/build isolation 已通过 remediation-2，未激活运行时路径**
**日期：2026-07-19**
**分支：feat/another-ext4-backend**

## 1. 文档位置与目标

`docs/10_plan` 是本仓库现有的计划文档目录，对应请求中的 plan documentation location。本文件与 [another-ext4-baseline.md](another-ext4-baseline.md) 共同构成后续实现的唯一门禁依据。Phase 0 冻结来源、供应链、边界、缓存身份、I/O 契约、测试和性能决策；Phase 1 已完成独立 fork、精确 gitlink 与可选 Cargo feature 的 source/build 隔离，但不激活运行时路径。

目标是让 MangoCore 可以在保留 lwext4 的前提下，接入一个独立版本化的 another_ext4 后端。启动路径继续使用 lwext4，默认行为不变。显式 mount 当前仍可能走旧的手写 ext4 路径，这构成另一条后端分歧，必须在迁移中以双后端策略和 A/B 一致性测试显式覆盖，不能假定启动 lwext4 已代表所有 mount 行为。

## 2. 当前边界与架构接缝

当前 Mango 后端路由必须记录为：

```text
启动初始化 → lwext4
显式 mount → 旧 ext4 路径，和启动 lwext4 存在语义与缓存边界分歧
```

迁移只允许沿以下接缝推进：

| 接缝 | Mango 现有责任 | 迁移要求 |
|---|---|---|
| VFS | 路径解析、dentry、权限、File 和 IndexNode 语义、mount 生命周期 | 只接收稳定的后端能力，不把 ext4 内部状态泄露给 syscall 或 PageCache |
| PageCache | 普通文件数据页、脏页状态、writeback、redirty、重试、预读、淘汰 | 独占普通文件数据缓存，后端不得再建立不可见的第二份数据缓存 |
| BlockDevice | 块地址、块大小、读写、flush 和设备错误 | 通过 adapter 提供可失败、可批量、可报告能力的 I/O |
| another_ext4 adapter | ext4 API 到 Mango 接缝的转换 | 负责 I/O 传输、错误映射、批量提交和能力声明，不拥有普通文件数据缓存 |
| another_ext4 core | ext4 元数据、extent、分配、JBD2、orphan、recovery | 保持上游语义边界，不复制到 Mango core |

### 2.1 所有权冻结

* **VFS** 拥有 names、dentries、permissions、open-file lifetime 和 mmap orchestration。
* **Mango PageCache** 独占普通文件数据，以及 Dirty、Writeback、Redirty、retry、readahead 和 eviction 状态机。
* **another_ext4** 拥有 ext4 metadata、extent、allocation、JBD2、orphan 和 recovery。
* **adapter** 拥有 I/O transfer、errors、batching 和 capabilities。

任何实现若让 another_ext4 core 直接管理普通文件数据页，或让 VFS 依赖 ext4 私有 inode 指针，均不得进入后续阶段。

### 2.2 身份与缓存规则

缓存及 writeback 的稳定身份是：

```text
filesystem instance + ext4 inode number + generation/reclaim protection
```

pathname 永远不是 writeback identity。unlink、rename、inode number 复用和 mount namespace 变化都不能让旧页面写回新对象。generation 或等价 reclaim protection 必须能阻止旧 inode 的延迟回收、旧 PageCache 和新 inode 复用同一 inode number 时发生交叉写回。

元数据缓存和普通数据缓存分开处理：

1. ext4 metadata cache 由 another_ext4 管理，包含 inode、目录、extent、位图、JBD2 和 recovery 所需状态。
2. 普通文件数据只由 Mango PageCache 管理，another_ext4 通过明确的 page read、page write 和 flush 接口访问块设备。
3. metadata dirty 不等同于普通 data dirty。事务提交、数据写回、inode size 更新和日志 checkpoint 必须分别可观察并可失败。
4. another_ext4 内部若有不可关闭的临时块缓存，必须在 adapter 设计中证明不会成为普通文件数据的第二权威副本，并定义失效、容量和 flush 关系，否则不得接入。

### 2.3 锁顺序与 I/O 约束

锁顺序固定为：

```text
VFS namespace/dentry → filesystem instance → inode metadata → PageCache state → BlockDevice submission
```

反向获取、跨路径循环等待和持有高层锁进入低层回调均禁止。所有路径遵守“no I/O under lock”：持锁阶段只读取或更新状态，先复制所需句柄和请求描述，释放锁后执行块设备 I/O、日志提交、等待或唤醒，再以 generation 检查结果并重新取得必要的锁。不得在 fd table、dentry、inode、PageCache 或 ext4 metadata 锁内等待 I/O，也不得在锁内执行隐式 Drop 以触发 close 或 flush。

## 3. 来源、版本与供应链

Oracle 已判定独立 `another_ext4` `8bc3842` 不适合作为核心 baseline。Phase 0 采用 DragonOS monorepo 中的 subtree 历史作为来源，具体位置为 `45931ee:kernel/crates/another_ext4`，再抽取到 Mango 的独立 fork。两者关系必须保留在 provenance 记录中，不能把独立仓库快照误写成 DragonOS 版本。

| 来源 | 标识 | 结论 | 允许用途 |
|---|---|---|---|
| 独立 another_ext4 | `8bc3842` | 独立版本，但不适合作为核心 baseline | 仅作差异、接口和风险参考，不作为接入基线 |
| DragonOS monorepo | `45931ee:kernel/crates/another_ext4` | 选定的源代码和历史起点 | 抽取 subtree 历史，形成 Mango fork 的初始内容 |
| Mango fork | `git@github.com:Mango-Iced-Americano/another_ext4.git`，`mango` 分支，最终 pin `6887c41ef212b483a6841c87cb4d4b025b8d2c1b` | 已发布的 Phase 1 供应链 | 只通过 `dependency/another_ext4` 子模块消费 |

### 3.1 必需分支

远端和本地工作流必须保留以下分支名及谱系：

* `dragonos`，保存抽取前后可追溯的 DragonOS 来源历史。
* `sync`，执行和审查 subtree 同步，Phase 1 采用的 split lineage 为 `571b85084fade21f5c26726a78e71356210c4f86`。
* `mango`，保存 Mango 适配所需的最小 fork 提交，最终消费 pin 为 `6887c41ef212b483a6841c87cb4d4b025b8d2c1b`。

Mango-Iced-Americano/another_ext4 已发布。最终证据固定为 SSH URL `git@github.com:Mango-Iced-Americano/another_ext4.git`、分支 `mango` 和完整 commit `6887c41ef212b483a6841c87cb4d4b025b8d2c1b`；本次 Phase 1 记录以该最终 pin 为准。`8bc3842` 仍是独立仓库对照分支的固定起点，`45931ee` 仍是用于 subtree split 的 DragonOS monorepo 来源；`sync` lineage 的 split commit 为 `571b85084fade21f5c26726a78e71356210c4f86`，不得含 Mango 专属补丁。

### 3.2 子模块与 pin

只有 Mango fork 可以成为子模块目标，目标路径固定为：

```text
dependency/another_ext4
```

不得把整个 DragonOS monorepo 添加为子模块，不得复制 DragonOS core、VFS 或其他核心目录。Mango 主仓库的子模块记录已固定到 `mango` 分支上的精确、完整 40 位 commit `6887c41ef212b483a6841c87cb4d4b025b8d2c1b`。Phase 1 只允许此固定 gitlink、可选本地 Cargo dependency 和非默认 feature；禁止 pin 分支名、tag、浮动 HEAD 或未经审计的短 SHA。

### 3.3 可复现 subtree 同步设计

同步脚本的设计契约如下，Phase 0 不创建脚本：

1. 使用固定 upstream URL、固定源 commit `45931ee` 和固定源目录 `kernel/crates/another_ext4`。
2. 在临时工作树中 fetch 上游，验证 commit 对象和目录存在，再执行 `git subtree split --prefix=kernel/crates/another_ext4 45931ee` 或等价的可审计抽取步骤。
3. 将抽取结果写入 `sync/dragonos-monorepo`，保留源 commit、源路径和抽取结果 SHA。
4. 只把抽取后的 crate 内容同步到 Mango fork 的 `dragonos`，然后在 `mango` 分支增加最小适配提交。
5. 运行必须是幂等的，重复执行相同输入得到相同 tree；任何 dirty worktree、非预期源路径、对象缺失或 hash 不一致都失败退出。
6. 脚本输出源 URL、源 commit、source prefix、split commit、目标 tree、目标 commit、工具版本和执行时间，供 UPSTREAM.md 和证据归档引用。

`UPSTREAM.md` 至少包含以下字段：

```text
upstream_url:
upstream_project:
upstream_commit: 45931ee
upstream_subtree: kernel/crates/another_ext4
extraction_commit:
extraction_command:
extraction_tool_versions:
mango_fork_url:
mango_branch: mango
mango_commit:
source_license:
local_patches:
patch_rationale:
known_deviations:
verification_commands:
last_sync_at:
maintainer:
```

### 3.4 本地同步验证与已发布 pin

已在 `/tmp/opencode` 建立临时 bare remote 和临时 `mango` worktree，仅用于验证同步模型。该临时仓库不能作为 MangoCore submodule URL，也不能替代已发布的组织 fork。

* 已验证 DragonOS source commit：`45931ee3b3e66892533563f73023021a83f89b2d`。
* 已验证 `dragonos` 到 `sync` 的 subtree split commit：`571b85084fade21f5c26726a78e71356210c4f86`。
* 临时 `mango` 分支包含 `f0ef2603`（provenance 和同步脚本）与 `03a5ee5c`（禁止同步 tags）。
* 在全新临时 clone 中运行脚本后，`sync/dragonos-monorepo` 再次解析为同一 split commit，且 tag 数量为零。

这些值证明抽取工作流可重复。已发布 Mango fork 的 `mango` 分支最终提供 `6887c41ef212b483a6841c87cb4d4b025b8d2c1b`，并由 Phase 1 最终证据精确固定；主仓库仍不得引用临时路径。

## 4. 后端能力契约

另一个后端只有在能力声明真实且可测试时才可被 adapter 暴露。能力矩阵如下，空白或猜测值均视为不支持：

| 能力 | lwext4 | 旧 ext4 | another_ext4 目标 | 接入门禁 |
|---|---|---|---|---|
| 启动默认 | 是 | 否 | 否 | 保持 lwext4 |
| 显式 mount | 现状可用 | 现状分歧 | Phase 0 禁止激活 | 先完成 A/B |
| fallible read | 需以现有 adapter 事实确认 | 现有语义 | 必须返回错误 | 注入短读、设备错和坏块测试 |
| fallible batched read | 需以现有 adapter 事实确认 | 现有语义 | 必须声明 batch 边界和部分完成 | 逐批次验证返回长度和错误 |
| fallible write | 需以现有 adapter 事实确认 | 现有语义 | 必须返回错误 | 覆盖短写、重试和 redirty |
| fallible batched write | 需以现有 adapter 事实确认 | 现有语义 | 必须声明部分提交 | 覆盖批次拆分和失败恢复 |
| flush | 需以现有 adapter 事实确认 | 现有语义 | 必须明确 flush 层次 | fsync、syncfs、unmount 顺序测试 |
| metadata journal/JBD2 | 后端责任不同 | 旧后端责任 | another_ext4 责任 | 事务、recovery、orphan 测试 |
| PageCache 数据所有权 | Mango | Mango | Mango | 禁止后端第二权威数据缓存 |

### 4.1 双后端政策

1. lwext4 保持可用并保持默认启动后端。
2. another_ext4 在 adapter、PageCache、错误语义和 flush 契约完成前不得接入启动、显式 mount 或自动选择逻辑。
3. 允许通过明确的开发测试开关选择后端，但不能改变默认配置，也不能让生产路径静默 fallback。
4. 每个后端都必须能独立 mount、unmount、读写、fsync 和报告错误。失败只能按明确策略返回，不能把 another_ext4 的未支持能力伪装成成功。
5. 旧 ext4 的显式 mount 分歧必须记录为独立比较对象，不得用 another_ext4 接入掩盖或删除既有实现。

## 5. 操作顺序与缓存一致性

### 5.1 读路径

```text
VFS open/read/mmap fault
  → 根据 filesystem instance + inode number + generation 获取 PageCache
  → 命中有效页则复制或映射，不调用后端
  → 未命中页则锁定页面状态并登记 Loading
  → 释放 PageCache 锁
  → adapter 发起可失败批量 read
  → 按实际返回长度填充页，短读和错误保留明确状态
  → 重新取得页锁，校验 generation，提交 UpToDate 或记录失败并唤醒等待者
```

读路径不允许用 pathname 作为缓存 key，不允许在 PageCache 锁下等待块设备，不允许把部分读误标为整页有效。错误页必须能重试，不能以旧内容伪装成功。

### 5.2 未来 writeback、fsync、syncfs 和 unmount

* 普通写入先修改 Mango PageCache，标记 Dirty，并根据页身份和 generation 形成 writeback batch。
* writeback 取得稳定 inode 和批次快照后释放 PageCache 锁，再调用 adapter 的 fallible batched write。成功页转为干净，短写、设备错误或后端拒绝转为 Redirty，并保留可重试原因。
* `fsync` 先完成目标文件数据 writeback，再要求 another_ext4 提交对应 inode 和 JBD2 metadata，最后调用设备 flush。任何一步失败都返回失败，不得提前报告成功。
* `syncfs` 先封存 filesystem instance 的可见 dirty 集合，完成普通数据 writeback，再提交该实例的 metadata journal，最后执行设备 flush。其他实例不得被错误地视为已同步。
* unmount 先阻止新引用和新 I/O，等待或取消可安全取消的 PageCache loading，完成或报告所有 dirty writeback，再完成 metadata commit 和设备 flush，最后执行 another_ext4 unmount、撤销 dentry/mount 引用和 reclaim protection。打开文件、mmap 和延迟 writeback 未处理完时不得释放 filesystem instance。

## 6. 九个迁移阶段与门禁

| 阶段 | 内容 | 进入门禁 | 接受门禁 |
|---|---|---|---|
| 1. source/supply-chain | 抽取 `45931ee:kernel/crates/another_ext4`，建立 `dragonos`、`sync/dragonos-monorepo`、`mango` 和 UPSTREAM.md | provenance 表冻结，fork 目标由用户确认 | 抽取可复现，Mango fork 有精确 commit，子模块只指向 fork |
| 2. crate/构建隔离 | 使 crate 可在 Mango 双架构工具链下编译，隔离 feature 和依赖 | 阶段 1 的 SHA 和许可证已确认 | 两架构串行编译无新增错误，未改变运行时路由 |
| 3. adapter/BlockDevice | 定义块大小、对齐、批量、短 I/O、错误和 flush 能力 | VFS/PageCache 所有权不变 | 读、写、flush 均可失败，部分完成可重建，能力矩阵有证据 |
| 4. identity/PageCache | 接入 filesystem instance、inode number、generation/reclaim protection | 阶段 3 的 I/O 契约已可表达 | inode 复用、unlink、rename、mount 多实例不会交叉写回 |
| 5. metadata/mount | 接入元数据、extent、分配、JBD2、orphan、recovery 的独立生命周期 | 数据缓存仍只由 Mango PageCache 所有 | mount、recovery、权限、目录和错误语义具备 focused 测试 |
| 6. read path | 仅接入只读或受控读路径 | 阶段 4 和 A/B fixture 通过 | cold、warm、跨页、hole、短读和错误读均正确，默认仍是 lwext4 |
| 7. writeback/fsync | 接入普通写、writeback、fsync、syncfs、unmount | 阶段 6 读一致性通过 | Dirty、Writeback、Redirty、retry、flush 和恢复顺序均通过故障注入 |
| 8. A/B conformance | lwext4、旧 ext4、another_ext4 同镜像和同测试集比较 | 阶段 7 无未解释失败 | 语义差异分类完成，未支持能力显式返回，默认路径无回退漂移 |
| 9. performance decision | 在基线协议下做重复测量和性能决策 | 基线 manifest、计数器和环境完整 | 5% 退化必须调查，10% 退化默认阻断，只有书面批准才能接受例外 |

任何阶段的进入或接受门禁不满足，都不得推进 VFS 或 mount 激活。阶段 9 不是“以后再优化”，它是接入决策的一部分。

### 6.1 Phase 1 isolation gate 与 remediation-2

`scripts/check_another_ext4_isolation.sh` 是只读检查器，用于锁定 Phase 1 的供应链和非激活不变量：Mango fork SSH URL、40 位 gitlink、checkout 与 gitlink 一致性、可选 Cargo dependency、非默认 feature，以及启动/显式 mount 路径仍保持现状。早期 gate 曾因无参数 `open_ext4rs()` 字面量断言与实际带 `BlockDevice` 参数的调用不匹配而失败，原始 `isolation-gate.log` 被保留为诊断证据。remediation-2 已在最终 pin `6887c41ef212b483a6841c87cb4d4b025b8d2c1b` 上通过 isolation gate，详见 `docs/Work_Log/evidence/2026-07-19/another-ext4-phase1-build/remediation-2-isolation-gate.log` 和 `remediation-2-result-status.txt`。实际启动调用仍为 lwext4，显式 mount 仍为旧 ext4，且没有 `ext4_another` 路由。

### 6.2 Phase 1 source/build 范围

Phase 1 的实现范围只包括 source/build 隔离：已发布 Mango fork、最终 pin `6887c41ef212b483a6841c87cb4d4b025b8d2c1b`、固定子模块 gitlink、可选且非默认的 Cargo feature，以及 remediation-2 在 Docker 中按 RV64 后 LA64 的顺序成功执行 isolation gate 和四次编译。四次最终编译均 exit 0：RV64 default、RV64 feature-on、LA64 default、LA64 feature-on，完整命令与日志见 `docs/Work_Log/evidence/2026-07-19/another-ext4-phase1-build/remediation-2-result-status.txt`。早期失败文件仍保留，并由 `remediation-artifact-verification.txt` 确认为 retained diagnostics，不能与最终通过结果混淆。本阶段没有执行 QEMU，没有 another_ext4 运行时路由，也没有改变启动 lwext4 或显式 mount 的旧 ext4 路径；VFS、PageCache、mount 和启动代码不因 Phase 1 而获准变更。编译隔离通过不等于 runtime validation、语义验证或性能验证通过。

## 7. A/B 一致性与性能门禁

### 7.1 A/B conformance

每个 A/B 运行必须固定架构、镜像、块设备、测试配置、libc、测试顺序和重复次数，并分别记录：

* 启动挂载和显式 mount 的路径。
* 文件创建、权限、目录、rename、link、unlink、truncate、hole、extent 边界和 inode 复用。
* read、write、pread、pwrite、mmap、msync、fsync、syncfs、unmount、recovery。
* 短读、短写、设备错误、flush 失败、JBD2 提交失败和重试后的结果。
* PageCache 命中、Loading、Dirty、Writeback、Redirty、retry、readahead、eviction，以及 metadata journal 状态。

结果分为语义一致、明确能力差异、环境跳过和未解释失败。未解释失败不得以“后端不同”结案。

### 7.2 性能决策

比较对象至少包括当前默认 lwext4、显式 mount 的旧 ext4、another_ext4 候选后端，且要按架构分别比较。

* 关键吞吐、延迟、系统调用和内存指标相对对应基线退化超过 5% 时必须调查。
* 退化达到或超过 10% 时阻断接入，除非有明确的书面批准、根因、影响范围和后续计划。
* 任何性能收益都不能抵消错误语义、数据一致性或 flush 失败处理缺陷。
* 计数器必须低开销、默认关闭，并保留无探针对照。至少覆盖 BlockDevice read/write/flush 次数、批次大小、短 I/O、错误和重试，PageCache 命中/缺页/Dirty/Writeback/Redirty/readahead/eviction，metadata transaction commit 和 journal checkpoint。

## 8. 非目标与禁止设计

* 不替换或删除 lwext4，不改变启动默认后端。
* 不在 Phase 0 激活 VFS、mount、syscall 或自动后端选择。
* 不把独立 `8bc3842` 当作核心 baseline。
* 不添加整个 DragonOS monorepo 子模块。
* 不复制 DragonOS VFS、PageCache、BlockDevice 或其他 core 到 Mango。
* 不让 pathname 成为 PageCache 或 writeback identity。
* 不让 another_ext4 管理普通文件数据缓存。
* 不在锁内做任何可能阻塞、等待、I/O、flush、回收或隐式 Drop 的操作。
* 不以无错误返回值、无限重试、静默短写、伪造 flush 或吞掉设备错误来满足接口。
* 不先接入再补批量 I/O、fallible I/O、flush 或性能基线。

## 9. Phase 0 与 Phase 1 完成定义

Phase 0 只有在以下项目全部写入审查记录后才算完成：来源表和差异结论、分支和 fork 操作说明、子模块 pin 规则、adapter 能力矩阵、缓存身份与锁顺序、读写及同步顺序、九阶段门禁、A/B 测试矩阵、性能阈值和非目标。Phase 1 的 source/build scope 已具备 `dragonos` 到 `sync` 的 `571b85084fade21f5c26726a78e71356210c4f86` lineage、已发布 `mango` 分支、最终 pin `6887c41ef212b483a6841c87cb4d4b025b8d2c1b`，以及 remediation-2 isolation gate 和四次 Docker 串行编译成功的证据。早期失败已作为 remediated diagnostics 保留。Phase 1 仍不宣称 QEMU、运行时激活、A/B 语义、性能验证或 baseline runner 认可已经发生。
