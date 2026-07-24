---
title: "MangoCore SMP 适配 Agent 执行规范"
category: plan
status: stable
owner: MangoCore Team
last_updated: 2026-07-21
tags: [smp, agent, workflow, review, safety]
related_docs:
  - "docs/10_plan/smp-8core-implementation.md"
  - "docs/01_architecture/lock-order.md"
  - "docs/08_testing/README.md"
---

# MangoCore SMP 适配 Agent 执行规范

> 本规范是 SMP 适配期间的强制人工协作约定。目标是把高风险并发改造拆成可理解、
> 可验证、可回退的小批次，而不是机械追求最少行数。

## 1. 适用范围

以下工作只要服务于 SMP 目标，就必须遵守本规范：

- BSP/AP 启动、boot stack、per-CPU 和 CPU-local 寄存器；
- trap、IPI、timer、idle、抢占点和中断开关；
- 调度器、runqueue、任务状态、affinity 和迁移；
- 页表、TLB shootdown、ASID、CoW 和用户 MM；
- 为 SMP 修改的锁、原子操作、unsafe、驱动、文件系统和网络代码；
- SMP ktest、QEMU 参数、构建配置、Work Log 和设计文档。

纯只读分析不消耗代码额度，但必须汇报重要发现。任何代码写入前都要进入本规范的人工确认流程。

## 2. 人工确认模型

每个实施批次经过两个确认点：

1. **修改前确认**：agent 提交批次目标、目标文件、预计关键代码行数、并发影响、风险和验证方式；
2. **修改后确认**：agent 完成修改与验证，详细汇报实际行数、设计思路、结果和残余风险，然后停止。

未获得修改前确认不得编辑 SMP 代码。提交修改后报告后，不得自动进入下一批。

用户给出的“继续全部计划”等总体授权不替代逐批沟通。用户可以预先批准一个列出编号、范围和
预算的连续批次序列，但 agent 每批仍须停止、提交报告并等待下一条明确指令；序列授权不允许静默
跨过人工审核点。

每批使用编号 <code>SMP-P&lt;phase&gt;-B&lt;sequence&gt;</code>，便于报告、Work Log 和证据文件互相对应。

## 3. 关键代码约 50 行原则

### 3.1 什么属于关键代码

以下内容计入关键代码：

- os/src、user/src 和 dependency 中进入生产路径的 Rust、汇编、C 和头文件；
- linker script、build.rs，以及改变 SMP 启动或运行语义的 Makefile/QEMU 配置；
- unsafe、原子操作、锁、CSR、PTE、IPI、调度状态机相关的宏和常量。

以下内容不计入关键代码额度，但必须单独统计：

- 直接解释关键代码的纯注释行和空行；
- kernel test、用户态 regression 和测试 fixture；
- Work Log、证据 manifest 和设计文档；
- 只改变诊断文本、不改变控制流的日志内容。

测试脚本或配置一旦改变 QEMU 拓扑、超时、PASS 判定或实际功能行为，就按关键代码计算。

### 3.2 计数方法

关键代码按补丁中的非空、非纯注释代码行计算：

<code>critical_lines = critical_added + critical_deleted</code>

- 修改一行按一次删除加一次新增计算；
- 移动关键代码按删除和新增分别计算；
- 同一批次的多次 patch 累计计算；
- 含尾部注释的代码行仍是关键代码行；
- formatter 或 codegen 意外改变关键代码时同样计入。

计数采用“机器生成原始补丁账本 + 人工语义分类”两层证据：

1. 保存 `git diff --numstat`、`git diff --unified=0` 和目标文件清单，机器不得漏掉 rename、
   generated file、配置或汇编；
2. 对每个新增/删除行标记 critical、comment、test、doc 或 diagnostic，并写明排除理由；
3. 脚本只能辅助分类，无法识别“日志是否改变控制流”“测试配置是否改变 PASS 判定”等语义，
   未分类行必须 fail-closed 计入 critical；
4. 报告同时给出 raw added/deleted 和 semantic critical count，人工可据补丁复算。

不得只引用一个自报总数，也不得把自动脚本输出当作无需审查的权威结论。

### 3.3 允许少量超出

50 行是批次设计目标，不是为了破坏正确性的机械硬切点。

- 原则上将关键代码控制在 50 行以内；
- 为保持一个不变量完整、双架构接口一致或错误路径闭合，可以少量超出；
- “少量”通常指不超过 10 行，即关键代码总量原则上不超过 60 行；
- 修改前已预见超出时，在申请中说明原因和预计总数；
- 修改中才发现需要超出时，先暂停关键代码写入并向用户说明；
- 超过 60 行或同时扩展到第二个不变量时，必须拆批并重新确认。

不得通过压缩多条语句到一行、删减必要注释或隐藏到测试代码中规避统计。

## 4. 修改前门禁

### 4.1 只读准备

每批开始前必须：

1. 加载 mango-workflow；
2. 阅读本规范和 SMP 实施方案中的当前 Phase；
3. SMP bug、竞态或子系统故障先读 debugging-patterns.md；
4. 性能、计数器或 QEMU 长测先读 harness-patterns.md；
5. 检查 git status、目标文件现有 diff 和相关调用链；
6. 检查 `docs/01_architecture/lock-order.md`，列出本批新增或改变的锁关系；
7. 确认位于冻结基线派生的专用 SMP branch/worktree，并确认前一批已经完成人工审核；
8. 尽可能运行能证明当前缺口的最小 RED 或基线测试。

不得只根据旧文档或记忆修改并发代码。当前源码、汇编入口、调用方和测试入口都要重新核对。

### 4.2 修改申请必须包含

| 字段 | 内容 |
|---|---|
| 批次目标 | 本批只建立或修复哪个不变量 |
| 当前证据 | 当前代码位置、实际行为和 RED/缺口 |
| 修改范围 | 精确到文件、函数、类型或汇编标签 |
| 明确不修改 | 防止范围静默扩大 |
| 行数预算 | 关键新增、关键删除、合计及可能超出原因 |
| 非关键改动 | 预计注释、测试、文档和 Work Log |
| 并发影响 | 所有权、锁、原子顺序、中断和生命周期 |
| 验证方式 | 双架构编译、focused QEMU 和单核回归 |
| 回退边界 | 可以精确反向恢复的 hunk |

申请必须说明为什么本批能够独立保持内核可编译和语义安全。

### 4.3 必须重新确认的变化

出现以下任一情况，原批准失效：

- 目标文件或要维护的不变量发生变化；
- 关键代码预计超过 60 行；
- 需要新增 unsafe、锁层级、IPI reason 或公共接口；
- 实际锁关系超出 lock-order.md 已批准的部分序；
- RED 结果与原根因假设不一致；
- 需要触碰用户已有修改；
- 中间状态必须依赖下一批才安全。

## 5. 注释规范

注释要解释“为什么正确、与谁同步、失败会怎样”，而不是逐字翻译代码。

### 5.1 每个关键 hunk 的最低要求

紧邻注释至少说明：

- 当前 CPU、远端 CPU、任务或 MM 中谁拥有状态；
- 哪个锁、原子状态或关中断区保护它；
- 修改前后的合法状态；
- 对应的读取、唤醒或 ack 方；
- 是否允许睡眠、等待或调度；
- 单核退化路径和架构差异。

### 5.2 需要接近一行一注释的位置

以下位置原则上每条关键指令或语句都有直接注释：

- boot entry 的栈计算、CPU ID 保存、BSP/AP 分支和页表切换；
- trap 汇编的寄存器保存恢复、CSR 读写、特权级切换和返回；
- IPI doorbell、pending reason、mailbox 发布、读取和 ack；
- Acquire、Release、AcqRel、SeqCst 原子操作；
- PTE 修改、generation、TLB flush、shootdown 和 frame 延迟释放；
- LoongArch ASID 分配、epoch rollover 和 invtlb；
- runqueue 所有权转移和任务状态 CAS；
- unsafe 块、裸指针、UnsafeCell 以及 Send/Sync 实现。

机械性的连续寄存器保存恢复可以使用分组注释，但必须写明寄存器范围、栈布局和恢复顺序。

### 5.3 原子与锁注释

每个非 Relaxed 原子操作要说明：

1. 发布或获取了哪些数据；
2. 与哪个具体原子操作形成同步；
3. 为什么更弱 ordering 不够；
4. 重复 IPI、并发写入或超时时如何保持幂等。

新增或改变锁使用时，要说明锁顺序、是否关闭本地中断、是否可能进入等待点，以及为何不会和 IPI/trap 自锁。

### 5.4 禁止的注释

- “设置变量”“进入循环”一类复述代码表面的注释；
- 与当前实现不一致的未来时说明；
- 用 TODO 代替当前批次必须满足的安全条件；
- 用“当前只有单核”证明 unsafe、裸指针或 static mut 安全；
- 声称远程 TLB 已刷新，但代码只有本地 sfence.vma 或 invtlb。

## 6. 修改执行纪律

- 使用宿主环境支持的精确 hunk 编辑方式，禁止整文件重写；在 Codex 环境中使用 apply_patch；
- 不运行会重写整文件或全仓库的 formatter；
- 不在同批混合重命名、格式整理和行为变化；
- 不顺手修复无关 warning、typo 或邻近 bug；
- 每次写入后更新关键代码行数；
- 新增测试不得放宽断言、扩大超时或跳过路径来制造 PASS；
- 未经用户明确要求，不委派 subagent 修改 SMP 代码。

每批应保持可编译、可回退、语义闭合。禁止提交假 IPI、无 ack shootdown、固定 CPU0 或其他临时 workaround。

### 6.1 超出范围或验证失败

如果正确修复超出批准范围：

1. 停止继续写关键代码；
2. 汇报已完成内容、实际行数和新发现；
3. 提出更小的拆分或新的批次申请；
4. 等待用户决定继续、保留还是撤销。

验证失败时先只读定位。修复仍属于原不变量且总体约 50 行时，可以说明后继续；涉及新不变量或明显超过 60 行时必须重新确认。

禁止使用 git reset 或 git checkout 清理工作树。需要撤销未提交批次时，使用精确反向 patch 并先
征得用户同意；已经提交且没有后续依赖时优先使用 `git revert`，存在依赖批次时先给出逐 hunk
回退顺序和影响，不能机械 revert 破坏后续不变量。

### 6.2 脏工作树

- 修改前记录 git status 和目标文件 diff；
- 不覆盖、整理或回退用户已有改动；
- 目标 hunk 与已有改动重叠时先请求专项许可；
- 无法可靠分离 agent 与用户改动时停止；
- 不删除无关未跟踪文件或历史证据。

SMP 实施默认从冻结基线创建独立 branch/worktree。人工审核后只有在用户明确授权 commit 时，
才把本批批准文件独立提交；不得为了“一批一提交”把用户无关修改一并暂存，也不得自行 push。

## 7. SMP 专项审查清单

### 7.1 BSP/AP 与启动

- 每个 CPU 在使用前获得唯一 boot stack；
- AP 使用的 stack 和 boot flag 不被 BSP 清 BSS；
- 只有 CPU0 执行全局初始化；
- AP 在访问堆和全局对象前 Acquire 观察 BSP 的 Release；
- 非法 CPU ID 和 online timeout 可诊断；
- Phase 1 AP online 后只进入 park loop，不调用旧 run_tasks，不使能普通 timer 中断；
- CORE_NUM=1 走同一实现的退化路径。

### 7.2 trap、IPI 与 timer

- 用户寄存器和 CPU-local 寄存器保存恢复完整；
- 内核 IPI handler 不分配、不睡眠、不获取普通业务锁；
- doorbell 前发布 payload，handler 正确清中断源并读取 reason；
- 重复或合并 IPI 保持幂等；
- 内核态 timer/IPI 只设置 deferred state，不直接切换任务；
- idle 检查、发布和休眠之间不存在 lost wakeup。
- IPI-only 与 timer-enabled 中断窗口分阶段验证，timer 回调工作延迟到安全点。

### 7.3 调度器

- TCB 只能存在于一个 runqueue 或一个 current 槽；
- 每次状态 CAS 都有成功、失败和重试语义；
- 不同时持有两个 runqueue 锁；
- 不嵌套 task.inner 与 runqueue 锁；
- 远程 enqueue 在释放锁后发送 IPI；
- current_task 返回 Arc，不制造伪 static 引用；
- 可变 current hint 有集中更新/失效协议，或改读权威对象；
- interruptible_queue 不作为 runnable queue，旧扫描不能绕过状态 CAS 重复入队；
- block、wake、exit、migration 和 affinity 有 focused race 测试。

### 7.4 MM、TLB 与 ASID

- PTE 修改明确区分页表是否已经发布；
- 已发布页表只通过统一 TLB batch 修改；
- generation、active mask、远程 flush 和 ack 顺序闭合；
- ack 前不释放或复用 frame、页表页、内核栈或 ASID；
- shootdown 等待路径自身能响应 IPI；
- LoongArch ASID 属于 MM，epoch rollover 做全核失效；
- RISC-V RFENCE 和 IPI fallback 提供相同上层语义。
- RISC-V stale 测试记录 victim 无 trap 窗口和 trap count，不能依赖 trap.S 的全量 sfence.vma；
- Phase 4 跨核用户测试保持 CPU/MM-only，不进入 Phase 5 才审计的共享子系统。

### 7.5 共享子系统

- 删除以单核为依据的 unsafe 安全证明；
- IRQ 可能访问的锁说明 irq-safe 策略；
- 持锁路径不 yield、schedule 或等待远端 ack；
- static mut 替换为原子、锁或 per-CPU 所有权；
- CPU0 housekeeping 与任意 CPU syscall 不重复推进全局状态。

### 7.6 阶段依赖复核

| 阶段 | 允许出现的并发 | 本阶段禁止提前引入 |
|---|---|---|
| 0/0.5 | 单 CPU 任务；锁原语/console focused test | AP 运行任务、普通 timer IRQ |
| 1 | AP online + park mailbox | AP run_tasks、远程 runqueue |
| 2 | IPI-only，随后 deferred timer | per-CPU runnable task |
| 2.5 | CPU0 单核状态 CAS、本地 TLB batch | remote shootdown、用户迁移 |
| 3a | per-CPU queue、目标选择、远程 enqueue | 默认开启 steal、用户 MM 跨核 |
| 3b | 可关闭的 work stealing | 普通用户任务跨核 |
| 4 | hermetic CPU/MM 用户测试 | 未审计 FS/net/device 并发 |
| 5 | 审计通过的共享子系统与普通用户任务 | 未证明的 unsafe/全局状态 |

## 8. 验证与证据

### 8.1 最小验证顺序

每个关键代码批次依次执行：

1. 静态检查 diff、关键代码行数和注释覆盖；
2. Docker 内执行 RV64 kernel build；
3. RV64 结束后再执行 LA64 kernel build；
4. 执行与本批不变量对应的 focused QEMU/ktest；
5. 执行 CORE_NUM=1 单核回归，并以 CORE_NUM=2 作为日常最小 SMP 配置；
6. 涉及 AP、IPI、runqueue 或 shootdown 时执行本阶段要求的 4/8 核门禁；
7. 核心竞态路径执行必要的重复或并发压力。

调度、IPI 和 TLB 竞态测试应显式使用 `-accel tcg,thread=multi`，保存完整命令并记录宿主侧
vCPU 线程证据；MTTCG 不可用时必须标为覆盖限制。

双架构编译命令：

~~~text
make rv64-kernel-build-only
make la64-kernel-build-only
~~~

不得以 cargo check、宿主机编译或单架构编译替代。

### 8.2 结果分类

- PASS：命令、退出码、结束标记和新鲜证据完整；
- FAIL：已执行并出现编译、panic、超时或断言失败；
- BLOCKED：环境、镜像、权限或前置资源阻止执行；
- NOT RUN：本批未执行。

BLOCKED 和 NOT RUN 不得写成“预计通过”。既有失败要说明是否与本批相关。

### 8.3 证据归档

证据写入当天唯一目录 <code>docs/Work_Log/evidence/YYYY-MM-DD/</code>，
文件使用 <code>smp-P&lt;phase&gt;-B&lt;sequence&gt;-</code> 前缀。

至少记录：

- git describe 和修改前后 status；
- Docker container ID、image digest、QEMU 版本和 mount 映射；
- 完整命令、CORE_NUM、配置和 exit status；
- 构建/QEMU 完整日志与 head-tail；
- PASS/FAIL 判定和时间戳；
- 关键代码行数统计；
- raw diff 账本、逐行语义分类和排除理由；
- 日志相对被测代码的新鲜性检查。

没有可复核证据时，只能报告“已执行但不可验收”。

## 9. 修改后详细汇报

完成每批后必须停止编码，并按以下内容汇报。

### 9.1 结论

- 批次编号与目标；
- 状态：完成、失败或阻塞；
- 是否完全落在批准范围；
- 当前动作固定为“等待人工审核”，即使存在预批准序列也不自动开始下一批。

### 9.2 行数账本

| 文件 | 关键新增 | 关键删除 | 关键合计 | 注释 | 测试 | 文档 |
|---|---:|---:|---:|---:|---:|---:|
| path | N | N | N | N | N | N |

给出关键代码总数、是否超过 50、超出理由和统计方法。
同时附 raw diff 行数、未分类行数和机器辅助工具版本；未分类行按关键代码计数。

### 9.3 修改思路

报告要说明：

1. 修改前的数据流或控制流；
2. 实际竞态、所有权缺口或架构限制；
3. 选择的最小实现方案；
4. 单核和多核下为何成立；
5. 原子、锁、关中断或 TLB 顺序如何闭环；
6. 本批明确没有处理什么。

不得只写“增加 SMP 支持”或“修复竞态”。

### 9.4 逐项变化和风险

按文件、函数或汇编标签列出：

- 行为变化；
- 注释覆盖的不变量；
- 调用方和被调用方变化；
- 错误、超时和重复事件处理；
- 架构共有与专用逻辑边界；
- 未运行验证和尚未证明的场景；
- 下一批前需要人工裁决的风险。

报告末尾声明 mango-workflow 状态，只提出一个下一批候选，不直接执行。

## 10. 明确禁止

- 未经当前批次批准直接编辑 SMP 代码；
- 无说明地明显超过 50 行关键代码；
- 用压缩代码、删除注释或藏入测试规避行数；
- 为减少行数把多个状态变化塞进一行；
- 同批混合启动、调度、TLB 等多个不变量；
- 没有当前源码或 RED 证据时猜测性修复；
- 用固定 CPU0、全局大锁或关闭中断到底掩盖问题；
- weakening test、扩大超时或忽略失败制造 PASS；
- 没有双架构验证和证据却声称完成；
- 未经要求 commit、push、创建 PR 或进入下一批。
- 用单线程 TCG 结果宣称已覆盖真实 vCPU 并行竞态。

## 11. 批次结束条件

一个批次在以下内容全部交付后结束：

- 已统计实际关键代码行数并解释任何超出；
- 关键 hunk 均有符合要求的注释；
- 双架构编译和必要 QEMU 已执行，或明确标为 BLOCKED/NOT RUN；
- 证据已归档并检查新鲜性；
- mango-workflow A→D 已执行；
- 详细报告已提交；
- agent 已停止并等待用户审核。

## 附录 A：修改前申请模板

~~~markdown
### SMP-Px-Bxx 修改申请

目标：
- 本批不变量：

当前证据：
- 代码位置：
- 当前行为或 RED：

范围：
- 文件与符号：
- 明确不修改：

行数预算：
- 关键新增：
- 关键删除：
- 关键合计：
- 超过 50 的必要性：
- 预计注释/测试/文档：

并发设计：
- 状态所有者：
- 锁与中断：
- lock-order.md 关系：
- 原子同步：
- 生命周期/TLB：

验证与风险：
- 双架构 build：
- focused QEMU：
- 单核回归：
- 已知风险：
- 回退 hunk：

请求：请确认是否批准 SMP-Px-Bxx。
~~~

## 附录 B：修改后报告模板

~~~markdown
### SMP-Px-Bxx 修改报告

结论：
- 状态：
- 是否符合批准范围：
- 当前动作：等待人工审核

行数账本：
| 文件 | 关键新增 | 关键删除 | 关键合计 | 注释 | 测试 | 文档 |
|---|---:|---:|---:|---:|---:|---:|
| ... | ... | ... | ... | ... | ... | ... |
- 关键代码总计：
- 超出 50 的理由：
- raw added/deleted：
- 未分类行（按 critical 计）：
- 统计方法/工具版本：

修改思路：
1. 修改前控制流：
2. 根因或不变量缺口：
3. 最小方案：
4. 单核/多核正确性：
5. 同步、锁或 TLB 闭环：
6. 本批边界：

逐项修改：
- 文件/符号：
  - 行为变化：
  - 注释覆盖：
  - 调用关系：
  - 错误与并发处理：

验证：
- 命令：
- 结果：
- 证据：
- 日志标记：

残余风险：
- 未运行项：
- 未证明场景：
- 需要人工裁决：

下一批唯一候选：
- 目标：
- 预计关键代码：
- 不执行，等待用户批准。

mango-workflow: loaded, references: ...
~~~
