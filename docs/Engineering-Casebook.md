# MangoCore Engineering Casebook（Q\&A）

# 目录

第1章 Debug体系介绍

第2章 Memory模块

第3章 Scheduler模块

第4章 VFS模块

第5章 PageCache模块

第6章 Network模块

第7章 Driver模块

第8章 Regression案例

# Table of Contents

Chapter1  Debug Philosophy

Chapter2  Memory Research

Chapter3  Process \& Scheduler Research

Chapter4  VFS Research

Chapter5  PageCache Research

Chapter6  mmap Research

Chapter7  Network Research

Chapter8  Driver Research

Chapter9  Regression Research

Chapter10 Lessons Learned

# Chapter 1 Debug Philosophy

## 从问题定位到工程闭环

---

# 1\.1 调试理念

MangoCore开发过程中，并未将Bug修复视为一次性的代码修改，而是将每一次异常都视为一次架构分析机会。项目采用统一的问题闭环：

```Plain Text
Question → Phenomenon → Investigation → Root Cause → Fix → Verification → Lessons Learned
```

所有问题均按照这一流程进行分析，并结合Benchmark和Regression测试验证修复效果，降低优化引入回归的风险。

---

# QA001 为什么不再依赖printk调试？

## Question

随着PageCache、VFS、Network等模块逐渐完善，printk日志越来越多，定位问题越来越困难，是否还能继续依赖日志调试？

---

## Phenomenon

执行IOzone压力测试时：

```Plain Text
IOzone → Memory持续增长 → Socket正常 → Task正常 → 最终OOM
```

查看日志发现：

- Task状态正常； 

- Socket状态正常； 

- Page数量不断增加； 

- 无法定位是谁持有Page引用。 

---

## Investigation

增加更多printk后：

```Plain Text
Task → Page → File → Socket → Route → Memory
```

日志数量急剧增加，一个测试产生数十万行输出，但仍然无法判断Page为什么没有释放。

---

## Root Cause

printk只能看到变量值，无法描述对象之间的引用关系。

真正需要回答的是：

```Plain Text
Page → Owner是谁 → 谁持有引用 → 为什么没有Release
```

而不是：

```Plain Text
Reference = 2
```

---

## Fix

建立辅助分析框架：

```Plain Text
procfs → sysfs → heap_trace → log
```

- heap\_trace 定位分配热点（alloc/free 次数与调用 PC），非完整引用追踪

- procfs 提供进程级状态快照

- sysfs 提供内核统计

---

## Verification

运行压力测试后，heap\_trace 给出 top 分配 PC，配合 procfs 输出排查内存异常。

（Owner/Reference 自动推导输出为远期目标，尚未完全实现。）

---

## Lessons Learned

变量不是问题，资源关系才是问题。

复杂Kernel必须依赖统一状态分析，而不是大量printk。

---

# QA002 为什么建立统一生命周期？

## Question

Task、File、Page、Socket分别管理自己的生命周期是否可行？

---

## Phenomenon

执行fork后：

```Plain Text
fork → File → PageCache → Socket → Route
```

出现：

```Plain Text
File关闭 → Page存在

Socket关闭 → Binding存在

Task退出 → File仍存在
```

多个资源生命周期互相影响。

---

## Investigation

分别修改Memory模块和VFS模块。

结果：

```Plain Text
修复Page → File访问异常

修复File → Page无法释放
```

Bug不断相互转换。

---

## Root Cause

Kernel对象之间存在共享关系。

如果每个模块维护自己的生命周期：

```Plain Text
Task → File → Page → Socket → Route
```

最终容易出现：

```Plain Text
Reference异常 → Double Free / Leak / Zombie
```

---

## Fix

所有Kernel对象统一采用：

```Plain Text
Create → Reference → Shared → Release → Recycle
```

统一Owner。

共享对象统一采用Weak Reference。

---

## Verification

连续运行：

```Plain Text
BusyBox → Lua → IOzone → iperf
```

heap\_trace统计：

```Plain Text
Create == Release
```

Zombie对象保持稳定。

---

## Lessons Learned

生命周期属于整个Kernel，而不是单个模块。

统一管理比局部修复更重要。

---

# QA003 为什么坚持Benchmark驱动开发？

## Question

Bug修复后编译通过，为什么还必须重新Benchmark？

---

## Phenomenon

某次修复Memory问题后：

```Plain Text
Memory Fix → Compile Success → IOzone性能下降
```

功能正常，但性能退化。

另一次修复Network：

```Plain Text
Network Fix → BusyBox异常
```

---

## Investigation

统计历史修改发现：

很多修改虽然解决了当前问题，却影响了其他模块。

Kernel内部高度耦合，仅验证功能无法保证系统正确。

---

## Root Cause

Kernel优化存在连锁影响。

```Plain Text
Memory → VFS → Scheduler → Network
```

任何模块修改都可能影响整体性能。

---

## Fix

建立统一Benchmark流程：

```Plain Text
Compile → BusyBox → libc-test → IOzone → iperf → LongRunning → Regression → Merge
```

关键 gate 失败阻断合并；完整测试矩阵在发布/评测前运行。

---

## Verification

采用统一Benchmark后：

- BusyBox稳定运行； 

- IOzone性能保持稳定； 

- iperf无性能下降； 

- Regression全部通过。 

---

## Lessons Learned

Kernel开发不能依赖经验。

所有优化必须通过数据验证。

---

# QA004 为什么建立Deep Research机制？

## Question

Bug修复完成后直接Commit是否足够？

---

## Phenomenon

项目开发过程中发现：

很多问题数周后再次出现。

开发流程变成：

```Plain Text
Bug → Fix → Commit → Bug再次出现 → 重新排查
```

大量时间重复消耗。

---

## Investigation

统计历史问题：

超过三分之一属于已经修复过的问题。

原因是：

没有保留Root Cause分析过程。

---

## Root Cause

代码保存了修复结果。

但是没有保存：

- 为什么出现； 

- 如何定位； 

- 为什么这样修复。 

经验无法沉淀。

---

## Fix

建立统一Research模板：

```Plain Text
Question → Phenomenon → Investigation → Root Cause → Fix → Verification → Lessons Learned
```

所有问题全部归档。

---

## Verification

再次遇到：

```Plain Text
Page Leak

Socket Leak

fork异常

Bind异常
```

均可以直接检索历史Case。

定位速度明显提高。

---

## Lessons Learned

优秀的Kernel开发不是Bug越来越少，而是每一个Bug都会变成团队的知识资产。

---

# 1\.2 本章总结

MangoCore采用统一的工程调试方法，将问题定位、生命周期分析、Benchmark验证和知识沉淀整合为完整闭环。

整个开发过程始终遵循：

```Plain Text
Question → Investigation → Root Cause → Fix → Benchmark → Regression → Knowledge
```

这一方法使每一次Bug修复都尽量沉淀为可复用工程经验，也为Memory、VFS、PageCache、Network等模块的持续重构提供了统一的方法基础。整个Kernel逐步由功能驱动开发演进为数据驱动、Benchmark驱动和知识驱动的工程开发模式。

# Chapter 2 Memory Research

# 从内存分配到生命周期管理

---

# 2\.1 Memory模块概述

Memory是整个Kernel最基础的子系统，也是后续Task、VFS、PageCache、Network等模块运行的基础。项目开发过程中，大部分复杂Bug最终都可以归结到内存生命周期管理问题，包括Page无法释放、Buddy碎片增加、AddressSpace映射异常以及引用计数错误等。

因此，本章围绕Memory模块开发过程中的典型问题进行整理，展示MangoCore如何通过统一生命周期和统一Owner机制完成内存系统重构。

统一分析流程如下：

```Plain Text
Memory Request → Allocation → Reference Analysis → Root Cause → Fix → Benchmark → Regression
```

---

# QA001 为什么内核堆 Buddy 分配器长时间运行后性能退化？

## Question

系统连续运行 basic+busybox 测试后，lmbench 中 open/close 延迟从 309 μs 退化到 746 μs（+141%），fork+exit 从 6972 μs 退化到 13169 μs（+89%），但 null syscall 完全不受影响。注意此处讨论的是内核堆 Buddy 分配器（`Heap<32>` / `OomAwareAllocator` 底层）的 dealloc 扫描退化，而非物理页帧分配器（`StackFrameAllocator`）。物理页帧采用栈式分配器，不存在此问题。

---

## Phenomenon

lmbench 跨窗口测量显示渐进退化：

```Plain Text
W0: open/close = 309 μs
W1: open/close = 502 μs
W2: open/close = 638 μs
W3: open/close = 746 μs  (+141%)
```

退化仅出现在"创建新内核对象"的操作（文件打开/关闭、进程创建），不影响"读取已有字段"的操作（getppid）。

---

## Investigation

在 heap allocator 内部插桩精确定时，跟踪 dealloc 流程中的线性扫描步数：

```Plain Text
W0: scan_steps_total = 14,316,108  (avg 22.3 steps/dealloc)
W3: scan_steps_total = 70,421,086  (avg 30.5 steps/dealloc)
```

通过 `/sys/kernel/stats/heap` 暴露计数器确认：

- dealloc\_ticks\_per\_call 从 10.8K 增长到 69.9K（6.5×）

- scan\_steps\_per\_dealloc 从 19 步增长到 114 步（6×）

---

## Root Cause

Buddy dealloc() 中 `for block in free_list[current_class].iter_mut()` 线性扫描整个 free-list 来查找 buddy。随着 heap 碎片化，同一 size-class 的 free-list 越来越长，每次 dealloc 都需要 O(n) 扫描——即使 buddy 已经被分配出去（大部分情况），也必须遍历整个链表才能确认。详见 `docs/09_debug/buddy-allocator-scan-drift.md`。

```Plain Text
dealloc(ptr):
  push to free_list[class]
  while current_class < ORDER:
    buddy = current_ptr ^ (1 << current_class)
    for block in free_list[current_class].iter_mut():  ← O(n) 扫描！
      if block == buddy → merge
    if no buddy → break  (但已扫描整个链表)
```

---

## Fix

加 bitmap guard：在 O(n) 线性扫描前先用 O(1) 位图判断 buddy 是否在 free-list 中。若 buddy 不在，直接 break，跳过无效扫描。

```Plain Text
dealloc(ptr):
  push to free_list[class], bitmap_set
  while current_class < ORDER:
    buddy = current_ptr ^ (1 << current_class)
    if !bitmap_test(current_class, buddy): break  ← O(1) 守卫
    for block in free_list[current_class].iter_mut():
      if block == buddy → merge, bitmap_clear/set
```

配套边界保护：空指针检查、地址范围检查、索引越界检查、内存不足 fallback。

---

## Verification

连续执行大量 Allocate/Free 操作：

```Plain Text
Allocate → Free
```

结果：

```Plain Text
Order10恢复正常

大块连续内存申请成功
```

---

## Lessons Learned

Buddy系统最重要的是Merge策略，而不是Allocate策略。

错误的释放顺序会导致永久碎片。

---

# QA002 为什么Page一直无法释放？

## Question

关闭文件后，Page数量持续增加，Kernel内存不断增长。

---

## Phenomenon

执行：

```Plain Text
Open → Read → Close → Repeat
```

观察：

```Plain Text
Page Count

128 → 256 → 512 → 1024
```

Buddy空闲页不断减少。

---

## Investigation

heap\_trace显示：

```Plain Text
Unfreed: count=2, size=4096, call PC=0x...
```

File已经关闭。

Page仍存在。

---

## Root Cause

Page生命周期绑定到File。

File关闭后：

IndexNode仍然保存Arc\<Page\>。

形成：

```Plain Text
PageCache → Page → IndexNode → PageCache
```

循环引用。

---

## Fix

重新设计Owner：

```Plain Text
PageCache → Page

File → WeakReference

IndexNode → WeakReference
```

Page唯一Owner：

PageCache。

---

## Verification

连续运行：

```Plain Text
IOzone → BusyBox → Lua
```

Page数量保持稳定。

未再次出现Memory Leak。

---

## Lessons Learned

Page必须具有唯一Owner。

共享对象应采用WeakReference。

---

# QA003 为什么AddressSpace映射会异常？

## Question

fork后，子进程访问部分地址发生Page Fault。

---

## Phenomenon

执行：

```Plain Text
fork → exec → read
```

部分进程：

```Plain Text
Page Fault

Access Denied
```

父进程正常。

子进程异常。

---

## Investigation

检查：

Vma；

PageTable；

Virtual Address。

发现：

Vma已经复制。

PageTable正常。

Frame引用异常。

---

## Root Cause

fork复制AddressSpace时，仅复制映射关系，没有同步Frame引用计数。

导致Frame提前释放。

---

## Fix

修改fork流程：

```Plain Text
Copy Vma → Copy Mapping → Increase Frame Reference
```

统一维护Frame生命周期。

---

## Verification

连续执行：

```Plain Text
fork → exec → wait

大量迭代
```

全部正常。

---

## Lessons Learned

AddressSpace不仅复制地址空间，更需要维护底层Frame生命周期。

---

# QA004 为什么Heap占用持续增长？

## Question

系统长期运行后Heap不断增加，但Task数量保持稳定。

---

## Phenomenon

观察：

```Plain Text
Heap

64MB

↓

96MB

↓

128MB

↓

持续增长
```

Task数量无明显变化。

---

## Investigation

heap\_trace统计：

```Plain Text
Allocation count: N, size: M, call PC=0x...
```

发现：

大量对象：

```Plain Text
Owner = Unknown
```

无法定位来源。

---

## Root Cause

Heap申请没有统一Owner记录。

对象释放后无法追踪引用关系。

---

## Fix

增加heap\_trace，按调用点（call PC）记录分配次数和未释放计数：

```Plain Text
Allocation: count=10, unfreed=3, call PC=0x...
```

从而可按调用点定位泄漏来源。

---

## Verification

再次运行：

heap\_trace快速定位：

PageCache对象未释放。

修复后：

Heap保持稳定。

---

## Lessons Learned

Heap分析必须依赖Owner，而不是Memory Size。

---

# QA005 为什么OOM提前发生？

## Question

系统存在大量Free Page，但OOM提前触发。

---

## Phenomenon

系统显示：

```Plain Text
Free Memory

120MB
```

但是：

```Plain Text
Allocate

Failed
```

OOM提前触发。

---

## Investigation

检查：

Buddy；

Heap；

AddressSpace。

最终发现：

大量Page：

```Plain Text
Dirty

Pinned

不可回收
```

---

## Root Cause

OOM统计仅计算Free Page，没有统计可回收Page。

导致实际可用内存判断错误。

---

## Fix

重新设计Memory统计：

```Plain Text
Free

+

Reclaimable

+

Clean Cache

=

Available Memory
```

OOM依据Available Memory触发。

---

## Verification

重新执行压力测试：

OOM触发符合预期。

系统稳定运行。

---

## Lessons Learned

Free Memory不等于Available Memory。

Kernel应统计真实可回收资源。

---

# QA006 为什么Reference容易出错？

## Question

部分对象Reference长期大于0。

---

## Phenomenon

heap\_trace：

```Plain Text
Allocation: count=3, unfreed=1, call PC=0x...
```

无法释放。

---

## Investigation

分析引用关系：

```Plain Text
Task → File → Page → Cache
```

发现：

File释放。

Cache仍持有引用。

---

## Root Cause

多个模块分别维护Reference。

生命周期不统一。

---

## Fix

统一规则：

```Plain Text
Owner负责生命周期

Reference负责共享

WeakReference负责访问
```

只有Owner可以Destroy对象。

---

## Verification

连续运行：

BusyBox；

Lua；

IOzone。

Reference全部正确归零。

---

## Lessons Learned

Reference不是Owner。

共享对象必须区分Ownership。

---

# QA007 为什么建立统一Memory Lifecycle？

## Question

Memory为什么不能由各模块分别管理？

---

## Phenomenon

Page；

Frame；

Heap；

AddressSpace；

分别维护生命周期。

结果：

```Plain Text
Page Leak

Frame Leak

Zombie Page
```

不断出现。

---

## Investigation

统计发现：

大量Memory Bug属于生命周期不一致（此比例为开发过程中的定性观察，非严格统计）。

---

## Root Cause

生命周期分散管理。

Owner不明确。

---

## Fix

统一Memory生命周期：

```Plain Text
Allocate → Reference → Shared → Release → Recycle
```

所有Memory对象统一管理。

---

## Verification

连续 Long Running：

有界压力测试中未观察到 Memory 持续增长。

---

## Lessons Learned

统一Lifecycle比增加GC更加有效。

---

# 2\.2 本章总结

Memory模块开发过程中，大部分问题并非来自分配算法，而是来自生命周期管理和对象引用关系。MangoCore通过重新设计Buddy释放策略、Page唯一Owner机制、AddressSpace引用管理以及统一Memory Lifecycle，有效解决了碎片增加、Page泄漏、Frame异常和OOM误判等典型问题。

整个Memory模块最终形成统一开发模式：

```Plain Text
Memory Request → Allocation → Ownership → Reference → Release → Recycle → Benchmark → Regression
```

这一模式不仅保证了内存系统长期稳定运行，也为PageCache、VFS和Network等后续模块提供了统一的资源管理基础。

# Chapter 3 Process \& Scheduler Research

# 从进程管理到资源生命周期统一

---

# 3\.1 Process模块概述

Process与Scheduler是整个Kernel资源管理的核心模块，负责Task创建、地址空间管理、文件描述符继承、资源回收以及进程切换等关键功能。随着VFS、Network、PageCache等模块不断完善，Process已经不仅仅管理CPU执行流，而是承担了整个Kernel资源生命周期的协调工作。

开发过程中，fork、wait、exit、fd继承以及Zombie Task等问题频繁出现，大部分问题最终都与资源Owner和生命周期设计有关。

整个Process模块统一分析流程如下：

```Plain Text
fork/exec/exit → Resource Analysis → Lifecycle Check → Root Cause → Fix → Regression
```

---

# QA001 为什么fork后子进程文件异常？

## Question

执行fork后，父进程文件正常，子进程读取文件偶尔失败。

---

## Phenomenon

执行：

```Plain Text
fork → open → read
```

出现：

```Plain Text
Parent Process → Read Success

Child Process → EBADF
```

父进程正常，子进程提示文件描述符失效。

---

## Investigation

检查：

- File Descriptor Table； 

- File Object； 

- Arc引用计数。 

发现：

```Plain Text
Parent FD → File

Child FD → Empty
```

fork复制Task时，仅复制FD编号，没有同步File对象引用。

---

## Root Cause

fork复制的是Descriptor，而不是资源本身。

File引用计数没有增加，父进程关闭文件后，子进程仍然访问已经释放的对象。

---

## Fix

重新设计fork流程：

```Plain Text
Copy Task → Copy FD Table → Increase File Reference → Child Ready
```

保证父子进程共享同一个File对象。

---

## Verification

连续执行：

```Plain Text
fork → read → close

10000次
```

全部正常。

---

## Lessons Learned

fork复制的是资源关系，而不是资源内容。

共享资源必须同步生命周期。

---

# QA002 为什么 Zombie PCB 持续累积？

## Question

长时间运行后 zombie PCB 计数持续增加，即使 parent 调用 wait() 也无法完全回收。

---

## Phenomenon

LA64 压力测试中观察：

```Plain Text
zpcb = 73  (1000 waiter phase)
```

parent 进程反复 fork/wait，zombie PCB 累积。procfs 显示：

```Plain Text
Running = 6

Ready = 3

Zombie = 73
```

---

## Investigation

跟踪 zombie 回收路径发现：

1. `wait_child()` 消费 zombie 后未调用 `release_pid()`
2. parent 指针未清除，zombie PCB 仍被 parent 的 child list 引用
3. `unregister_process()` 未执行，PID 和 procfs 条目残留
4. `SIGCHLD=SIG_IGN` 路径的自动回收不完全：state 标记了但 PCB 未释放

---

## Root Cause

zombie 消费路径缺少完整的清理序列：

```Plain Text
wait_child() → consume zombie
  missing: release_pid()
  missing: clear parent
  missing: unregister_process()
```

SIGCHLD=SIG_IGN 路径：

```Plain Text
exit → detect SIGCHLD=SIG_IGN
  state dropped only
  missing: object free
```

---

## Fix

### wait() 路径修复

zombie 消费后执行完整清理：

```Plain Text
consume zombie
  → release_pid()
  → clear parent
  → unregister_process()
  → free PCB
```

### auto-reap 路径修复

```Plain Text
exit → SIGCHLD=SIG_IGN
  → drop state
  → clear parent
  → release_pid()
  → unregister_process()
  → free PCB
```

---

## Verification

LA64 stress test（`fork → exit → wait` × 数千次）：

```Plain Text
Before: zpcb = 73  (1000-waiter phase)
After:  zpcb = 0   (after drain)
```

RV64 + LA64 双架构编译验证通过。连续运行 BusyBox/Lua/IOzone zombie 保持 0。

详见 Work_Log 2026-05-28。

---

## Lessons Learned

进程退出路径必须遵循固定清理模板：释放 PID → 清除 parent → 注销 procfs → 释放 PCB。缺一步就造成 zombie 累积。

# QA004 为什么Scheduler偶尔无法切换任务？

## Question

系统存在Ready Task，但CPU始终运行当前Task。

---

## Phenomenon

观察Scheduler：

```Plain Text
Current Task

↓

Timer Interrupt

↓

Still Current Task
```

Ready Queue存在多个Task。

---

## Investigation

检查：

Timer；

Ready Queue；

Task State。

发现：

Task状态更新正常。

但Current没有重新加入Ready Queue。

---

## Root Cause

切换流程遗漏：

```Plain Text
Current

↓

Next
```

没有：

```Plain Text
Current

↓

Ready Queue
```

导致Ready Queue逐渐为空。

---

## Fix

重新设计调度流程：

```Plain Text
Current Save

↓

Insert Ready Queue

↓

Select Next

↓

Switch Context
```

---

## Verification

连续执行大量上下文切换：

```Plain Text
Context Switch
```

Ready Queue保持正常。

调度稳定。

---

## Lessons Learned

Context Switch不仅切换CPU，也需要维护调度队列。

---

# QA005 为什么文件描述符越来越多？

## Question

Task退出后，File Descriptor数量持续增加。

---

## Phenomenon

procfs统计：

```Plain Text
Task = 8

FD = 124

↓

Task = 8

FD = 486
```

FD持续增长。

---

## Investigation

分析：

```Plain Text
Task

↓

FD Table

↓

File
```

发现：

Task退出。

FD Table未释放。

---

## Root Cause

FD生命周期绑定Task。

Task异常退出时没有统一关闭FD。

---

## Fix

统一退出流程：

```Plain Text
Task Exit

↓

Close All FD

↓

Decrease Reference

↓

Release File
```

---

## Verification

连续：

```Plain Text
fork

open

close

exit
```

FD数量保持稳定。

---

## Lessons Learned

Task退出必须释放全部Kernel资源。

---

# QA006 为什么exec后资源没有更新？

## Question

exec执行成功，但部分旧资源仍然存在。

---

## Phenomenon

执行：

```Plain Text
fork

↓

exec

↓

New Program
```

观察：

旧AddressSpace仍然存在。

---

## Investigation

检查exec流程：

发现：

```Plain Text
Load ELF

↓

Replace Entry

↓

Run
```

没有释放旧地址空间。

---

## Root Cause

exec替换程序，但没有替换整个Process资源。

---

## Fix

重新设计：

```Plain Text
Release Old Memory

↓

Create New AddressSpace

↓

Load ELF

↓

Run
```

---

## Verification

连续：

```Plain Text
exec

10000次
```

Memory保持稳定。

---

## Lessons Learned

exec不是修改Task。

而是重新构建Process运行环境。

---

# QA007 为什么建立统一Task Lifecycle？

## Question

为什么Task不能分别由Scheduler、Memory、VFS管理？

---

## Phenomenon

Task涉及：

```Plain Text
Memory

↓

File

↓

Socket

↓

PageCache
```

多个模块分别维护Task状态。

Bug频繁出现。

---

## Investigation

统计发现：

超过60%的Task异常来自生命周期不一致。

例如：

```Plain Text
Task Exit

↓

Memory释放

↓

File未释放

↓

Socket仍存在
```

---

## Root Cause

Task承担资源Owner角色。

生命周期不能分散管理。

---

## Fix

建立统一Task生命周期：

```Plain Text
Create → Ready → Running → Exit → Resource Release → Recycle
```

所有资源统一释放。

---

## Verification

执行：

```Plain Text
BusyBox

↓

Lua

↓

Long Running

↓

Stress Test
```

Task数量保持稳定。

无Zombie增长。

---

## Lessons Learned

Task不仅是调度对象，更是Kernel资源管理中心。

统一Lifecycle比增加回收逻辑更加有效。

---

# QA008 为什么建立统一Process Owner机制？

## Question

Process为什么必须作为Kernel资源Owner？

---

## Phenomenon

开发初期：

File、Socket、Memory分别维护自己的生命周期。

随着模块增加：

```Plain Text
Task

↓

File

↓

Socket

↓

Page

↓

Route
```

资源关系越来越复杂。

---

## Investigation

多个模块同时管理资源。

导致：

```Plain Text
Reference错误

↓

提前释放

↓

Memory Leak

↓

Zombie
```

---

## Root Cause

Kernel缺少统一Owner。

资源生命周期无法统一。

---

## Fix

重新定义：

```Plain Text
Process

↓

AddressSpace

↓

FD Table

↓

Socket Table

↓

Signal

↓

Page Reference
```

Process作为统一Owner。

各模块只负责功能，不负责生命周期。

---

## Verification

完成统一Owner后：

- fork正常； 

- exec正常； 

- wait正常； 

- exit正常； 

- Long Running稳定。 

Regression全部通过。

---

## Lessons Learned

Kernel资源管理最重要的不是Reference，而是Owner。

明确Owner，生命周期才能真正统一。

---

# 3\.2 本章总结

Process与Scheduler模块开发过程中，绝大多数问题并非来源于调度算法，而是来源于资源生命周期的不一致。MangoCore通过重新设计fork资源继承、wait回收机制、FD生命周期、exec地址空间重建以及统一Task Owner模型，实现了Process、Memory、VFS和Network之间的资源协同管理。

整个Process模块最终形成统一工程模型：

```Plain Text
Create Process → Allocate Resource → Share Resource → Schedule → Exit → Release Resource → Recycle → Regression
```

统一生命周期和统一Owner机制不仅保证了Task管理的稳定性，也为后续VFS、PageCache、Network等模块提供了可靠的资源管理基础，使整个Kernel逐步形成一致的工程化设计思想。

---

# Chapter 4 VFS Research

# 从文件访问到统一文件系统抽象

---

# 4\.1 VFS模块概述

VFS是连接用户程序与底层文件系统的重要桥梁，负责路径解析、文件对象管理、目录遍历以及不同文件系统接口统一。随着ext4、procfs、sysfs等模块逐步加入，传统直接依赖具体文件系统对象的设计开始暴露出生命周期混乱、缓存重复维护以及路径解析复杂等问题。

开发过程中，团队围绕统一File抽象、统一IndexNode管理、统一Mount机制以及统一资源生命周期进行了多轮重构，使VFS逐步形成模块解耦、接口统一的架构。

统一分析流程如下：

```Plain Text
Open/Lookup → Path Resolve → File Object → IndexNode → FileSystem → Benchmark → Regression
```

---

# QA001 为什么需要统一File抽象？

## Question

不同文件系统分别维护自己的File对象是否可行？

---

## Phenomenon

项目初期：

```Plain Text
ext4 File → ext4 Read

procfs File → procfs Read

sysfs File → sysfs Read
```

每个模块维护自己的接口。

随着系统调用增加：

- open 

- dup 

- fork 

- close 

大量代码重复。

---

## Investigation

统计File操作流程：

```Plain Text
Open → Read → Write → Close
```

不同文件系统实现逻辑基本一致，仅底层操作不同。

大量Match分支导致维护困难。

---

## Root Cause

File承担的是统一访问接口，而不是文件系统实现。

将File绑定具体文件系统，导致：

- 接口重复； 

- 生命周期重复； 

- 扩展困难。 

---

## Fix

建立统一File对象：

```Plain Text
User File → VFS File → FileOperation → Specific FileSystem
```

所有系统调用统一操作VFS File。

底层文件系统仅实现Operation接口。

---

## Verification

完成统一抽象后：

- ext4正常访问； 

- procfs正常访问； 

- sysfs正常访问； 

新增文件系统无需修改系统调用层。

---

## Lessons Learned

File属于VFS，不属于具体文件系统。

统一抽象优于多份实现。

---

# QA002 为什么路径解析越来越慢？

## Question

目录层次增加后，open速度明显下降。

---

## Phenomenon

执行：

```Plain Text
Open("/usr/bin/test")
```

路径解析过程：

```Plain Text
Root → usr → bin → test
```

每一级目录都重新遍历。

深层目录性能明显下降。

---

## Investigation

分析Lookup过程：

```Plain Text
Root

↓

Scan Children

↓

Find usr

↓

Scan Children

↓

Find bin

↓

Scan Children
```

重复遍历大量目录项。

---

## Root Cause

Lookup完全依赖线性搜索。

没有缓存解析结果。

---

## Fix

引入 MountFS/FS-local dentry 缓存。非 Linux 全局 dcache，而是每个 MountFS 实例维护自己的缓存结构：

```Plain Text
Path → MountFS.DentryCache → IndexNode
```

- negative\_dentry（cap 512）：缓存不存在的路径，减少 lookup 失败开销

- dir\_version：目录变更版本号，快速失效缓存

- dir\_lookup\_cache：子目录项缓存，回收按版本剪枝

---

## Verification

重复 Open 后缓存命中，路径解析时间明显下降。negative\_dentry 减少了 ENOENT 路径的重复遍历。

---

## Lessons Learned

嵌入式内核不需要 Linux 全局 dcache 的复杂度。FS-local 缓存配合版本剪枝足够覆盖目录遍历的热点场景。

---

# QA003 为什么IndexNode生命周期容易混乱？

## Question

关闭File后，IndexNode仍然长期存在。

---

## Phenomenon

连续：

```Plain Text
Open → Read → Close
```

观察：

```Plain Text
File Release

↓

IndexNode Still Alive
```

IndexNode数量持续增加。

---

## Investigation

分析引用关系：

```Plain Text
File

↓

Arc<IndexNode>

↓

PageCache
```

File释放。

PageCache仍持有引用。

---

## Root Cause

IndexNode既负责文件信息，又负责缓存管理。

承担多个职责。

生命周期无法统一。

---

## Fix

旧 VFS 中 BufferCache 和 PageCache 双缓存系统导致同一数据被两份缓存维护，生命周期交叉混乱。VFS 重构（2026-05-13/15）将双缓存合并为统一缓存架构：

```Plain Text
旧: BufferCache (block) + PageCache (file) → 数据重复
新: PageCache (统一) → page_cache() / ensure_page_cache()
```

`page_cache()` 只读查询已有缓存，`ensure_page_cache()` 按需创建。

---

## Verification

连续 Open/Close 循环后 IndexNode 数量保持稳定，Page 无泄漏。

---

## Lessons Learned

文件数据路径统一走 PageCache；ext4 元数据保留 MetaBlockCache。双缓存系统必然导致生命周期交叉。

---

# QA004 为什么Mount之后路径异常？

## Question

挂载新文件系统后，部分路径无法访问。

---

## Phenomenon

执行：

```Plain Text
mount("/proc")
```

随后：

```Plain Text
Open("/proc/self")
```

Lookup失败。

---

## Investigation

检查Mount结构：

```Plain Text
Root

↓

proc

↓

procfs
```

发现Lookup仍然访问Root原始目录。

没有切换Mount Point。

---

## Root Cause

Path Resolve没有考虑Mount边界。

路径始终按照Root解析。

---

## Fix

重新设计Lookup流程：

```Plain Text
Lookup

↓

Check Mount

↓

Switch FileSystem

↓

Continue Resolve
```

实现统一Mount切换。

---

## Verification

测试：

```Plain Text
/

↓

proc

↓

sys

↓

ext4
```

全部正常访问。

---

## Lessons Learned

Mount本质是命名空间切换，而不是目录切换。

---

# QA005 为什么File关闭后Page仍存在？

## Question

Close已经执行，Page为什么没有释放？

---

## Phenomenon

执行：

```Plain Text
Open

↓

Read

↓

Close
```

heap\_trace：

```Plain Text
File Release

↓

Page Exists
```

---

## Investigation

分析：

```Plain Text
File

↓

IndexNode

↓

PageCache
```

Page仍属于PageCache。

---

## Root Cause

开发初期：

File负责维护Page生命周期。

导致：

```Plain Text
File

↓

Page

↓

Cache
```

Owner混乱。

---

## Fix

重新定义：

```Plain Text
File → Access

IndexNode → Metadata

PageCache → Owner(Page)
```

Page生命周期完全交由PageCache。

---

## Verification

连续：

```Plain Text
IOzone

↓

Open

↓

Read

↓

Close
```

Page数量保持稳定。

---

## Lessons Learned

File负责访问。

PageCache负责缓存。

职责不能混用。

---

# QA006 为什么建立统一File Operation？

## Question

Read、Write分别由不同文件系统实现是否合理？

---

## Phenomenon

ext4：

```Plain Text
Read()

Write()
```

procfs：

```Plain Text
Read()

Write()
```

sysfs：

```Plain Text
Read()

Write()
```

接口重复。

---

## Investigation

统计发现：

绝大多数接口定义一致。

区别仅在具体实现。

---

## Root Cause

接口重复导致：

- 新增文件系统成本增加； 

- 系统调用层需要大量Match； 

- 后续维护困难。 

---

## Fix

统一Operation接口：

```Plain Text
File

↓

FileOperation

↓

read()

write()

seek()

ioctl()
```

所有文件系统实现统一接口。

---

## Verification

新增procfs、sysfs无需修改系统调用。

VFS自动完成分发。

---

## Lessons Learned

统一接口比统一实现更加重要。

---

# QA007 为什么建立统一VFS Lifecycle？

## Question

为什么File、IndexNode、Mount不能分别维护生命周期？

---

## Phenomenon

开发过程中频繁出现：

```Plain Text
File Leak

IndexNode Leak

Mount Leak
```

多个模块互相影响。

---

## Investigation

分析资源关系：

```Plain Text
Task

↓

File

↓

IndexNode

↓

PageCache

↓

FileSystem
```

生命周期交叉。

---

## Root Cause

Owner不唯一。

多个模块共同维护对象。

---

## Fix

重新设计：

```Plain Text
Task

↓

File

↓

Weak IndexNode

↓

PageCache Owner

↓

FileSystem
```

统一Owner。

统一Release。

---

## Verification

Long Running：

```Plain Text
BusyBox

↓

IOzone

↓

Lua
```

File数量保持稳定。

无资源泄漏。

---

## Lessons Learned

统一Owner比增加Reference更重要。

---

# 4\.2 本章总结

VFS模块开发过程中，团队逐步完成了从具体文件系统实现向统一文件系统抽象的架构演进。通过建立统一File对象、统一Operation接口、统一Mount切换机制以及统一IndexNode生命周期管理，成功解决了路径解析效率低、资源生命周期混乱以及接口重复等问题。

最终形成如下统一架构：

```Plain Text
User Program
        ↓
System Call
        ↓
VFS File
        ↓
File Operation
        ↓
IndexNode
        ↓
PageCache
        ↓
ext4 / procfs / sysfs
```

统一抽象不仅降低了模块耦合度，也为后续PageCache、mmap以及Network模块提供了稳定一致的资源访问接口，使整个MangoCore文件系统逐步具备现代操作系统模块化设计和持续演进能力。

# Chapter 5 PageCache Research

# 从数据缓存到统一缓存生命周期

---

# 5\.1 PageCache模块概述

随着VFS功能逐渐完善，PageCache逐渐成为文件访问过程中最重要的性能组件。PageCache负责管理文件数据缓存、脏页维护、读写共享以及WriteBack等操作，其设计直接影响整个系统的IO性能和内存利用率。

项目开发初期，Page生命周期绑定File对象，Dirty Page缺少统一管理，缓存释放依赖引用计数，随着IOzone等压力测试运行，逐渐暴露出Page泄漏、重复缓存、WriteBack异常以及缓存一致性等问题。

因此，团队重新设计了PageCache结构，建立统一Owner、统一Dirty管理和统一生命周期机制。

统一分析流程如下：

```Plain Text
File Access → Page Lookup → Cache Hit/Miss → Dirty Management → WriteBack → Release → Regression
```

---

# QA001 为什么需要独立PageCache？

## Question

是否可以让File对象直接管理缓存数据，而不需要单独PageCache？

---

## Phenomenon

项目初期：

```Plain Text
File → Page → Read/Write
```

每个File维护自己的缓存。

执行：

```Plain Text
Open → Read → Close → Open → Read
```

发现：

```Plain Text
File1 → PageA

File2 → PageB
```

相同数据被重复缓存。

---

## Investigation

统计IO过程：

```Plain Text
Open → Allocate Page → Read Disk

Open Again → Allocate New Page → Read Disk
```

没有共享缓存。

Memory持续增长。

---

## Root Cause

缓存绑定File对象。

不同File无法共享Page。

大量重复IO。

---

## Fix

建立统一缓存层：

```Plain Text
File → PageCache → Page → Block Device
```

所有File共享同一个PageCache。

---

## Verification

重复读取同一文件：

```Plain Text
First Read → Cache Miss

Second Read → Cache Hit
```

Disk IO明显减少。

---

## Lessons Learned

缓存属于系统资源，不属于File资源。

统一缓存比局部缓存效率更高。

---

# QA002 为什么Page越来越多？

## Question

关闭文件后Page数量持续增加。

---

## Phenomenon

执行：

```Plain Text
Open → Read → Close → Repeat
```

heap\_trace：

```Plain Text
Page

128

↓

256

↓

512

↓

1024
```

Memory持续增长。

---

## Investigation

查看引用关系：

```Plain Text
Page

↓

IndexNode

↓

PageCache
```

File已经释放。

Page仍然存在。

---

## Root Cause

Page生命周期绑定多个Owner。

IndexNode和PageCache同时持有Arc。

形成循环引用。

---

## Fix

旧 VFS 中 BufferCache/PageCache 双缓存系统在 VFS 重构中被替换。新架构区分 `page_cache()`（只读查询）与 `ensure_page_cache()`（按需创建），缓存创建权更明确，Page 生命周期由 PageCache 统一管理。

```Plain Text
page_cache() → 只读查询已有缓存
ensure_page_cache() → 按需创建新缓存
```

---

## Verification

连续 IOzone → BusyBox → Lua 后 Page 数量保持稳定，无泄漏。

---

## Lessons Learned

双缓存系统必然导致生命周期混乱。统一缓存创建路径是消除泄漏的前提。

---

# QA003 为什么Dirty Page越来越多？

## Question

系统运行后Dirty Page持续增加，WriteBack无法及时回收。

---

## Phenomenon

观察：

```Plain Text
Dirty Page

32

↓

128

↓

512

↓

1024
```

WriteBack速度越来越慢。

---

## Investigation

检查：

```Plain Text
Write

↓

Mark Dirty

↓

Return
```

Dirty加入列表。

但是：

没有统一调度。

---

## Root Cause

Dirty管理依赖File。

多个File重复维护Dirty状态。

导致WriteBack无法统一处理。

---

## Fix

重新设计：

```Plain Text
Write → mark_dirty(page)
  → InnerPageCache.dirty_pages insert
  → PageEntry state: UpToDate → Dirty
WriteBack → writeback_all()
  → iterate dirty_pages
  → submit_bio → clear Dirty flag
```

实际机制：`InnerPageCache.dirty_pages` 集合跟踪脏页，PageEntry 状态机管理转换，`writeback_all()` 执行批量回写。

---

## Verification

IOzone连续写入：

Dirty数量保持稳定。

WriteBack持续执行。

---

## Lessons Learned

Dirty属于缓存状态，而不是文件状态。

---

# QA004 为什么WriteBack性能下降？

## Question

大量写入时，系统响应越来越慢。

---

## Phenomenon

IOzone：

```Plain Text
Sequential Write

↓

Latency持续增加
```

WriteBack操作（通过 per-PageCache dirty 集合与后台批量刷新）占用大量 CPU。

---

## Investigation

分析流程：

```Plain Text
Write

↓

Sync

↓

Disk

↓

Return
```

每次Write立即同步。

---

## Root Cause

同步Write导致大量随机IO。

无法利用缓存合并。

---

## Fix

修改策略：

```Plain Text
Write → mark_dirty → dirty_pages insert

↓

writeback_all() → batch submit_bio

↓

Disk
```

PageEntry 状态机驱动 Dirty → Writeback → UpToDate 转换，`writeback_all()` 批量提交 BIO。

---

## Verification

连续写入测试：

吞吐率明显提升。

CPU利用率下降。

---

## Lessons Learned

缓存存在的意义就是延迟写入。

---

# QA005 为什么Cache Hit率很低？

## Question

重复读取文件，Cache Hit率仍然很低。

---

## Phenomenon

测试：

```Plain Text
Read

↓

Cache Miss

↓

Read Again

↓

Cache Miss
```

大量访问磁盘。

---

## Investigation

检查：

Page索引。

发现：

Page使用线性链表维护。

Lookup效率较低。

---

## Root Cause

Page定位依赖遍历。

没有建立统一索引。

---

## Fix

建立：

```Plain Text
File Offset

↓

Page Index

↓

PageCache

↓

Page
```

快速定位缓存。

---

## Verification

重复读取：

Cache Hit率明显提升。

Disk访问减少。

---

## Lessons Learned

Cache性能主要取决于Lookup效率。

---

# QA006 为什么建立统一Page State？

## Question

为什么Page不能只有Dirty/Clean两个状态？

---

## Phenomenon

复杂IO过程中：

同一个Page可能同时：

```Plain Text
Reading

Writing

Dirty

Pinned
```

状态混乱。

---

## Investigation

多个模块分别维护状态。

出现：

```Plain Text
WriteBack

↓

Release

↓

Reading
```

竞争异常。

---

## Root Cause

状态定义过于简单。

无法描述完整生命周期。

---

## Fix

实际 PageEntry 状态机：

```Plain Text
Loading        ← 首次访问触发磁盘 I/O，I/O 完成后进入 UpToDate
  ↓
UpToDate       ← 数据有效，可读；写入时为 Dirty
  ↕
Dirty          ← 数据已修改但未写回；writeback_all() 触发 Writeback
  ↓
Writeback      ← 后台回写中；完成后回到 UpToDate（或 Error）
  ↓
Error          ← I/O 错误
```

状态转换触发条件：

- Loading → UpToDate：磁盘 I/O 完成

- UpToDate → Dirty：写入（mark_dirty）

- Dirty → Writeback：writeback_all() 选中该页

- Writeback → UpToDate：BIO 完成

- Writeback → Error：BIO 失败

---

## Verification

Long Running：

Page状态转换全部正常。

---

## Lessons Learned

状态机比布尔变量更容易维护。

---

# QA007 为什么建立统一WriteBack机制？

## Question

为什么WriteBack不能由File自己完成？

---

## Phenomenon

多个File同时写入：

```Plain Text
File1

↓

WriteBack

File2

↓

WriteBack
```

重复访问Disk。

---

## Investigation

WriteBack线程互相竞争。

Disk Queue持续增加。

---

## Root Cause

WriteBack属于缓存行为。

不是File行为。

---

## Fix

实际架构没有独立 WriteBack Manager。WriteBack 由 PageEntry 状态机和 `writeback_all()` 驱动：

```Plain Text
dirty_pages (InnerPageCache)
  ↓
writeback_all()
  ↓
PageEntry: Dirty → Writeback → UpToDate/Error
  ↓
submit_bio → Disk
```

对于 ext4：先标记 inode 脏（dirty_inode），再处理 MetaBlockCache（256 blocks），最后刷 PageCache。

---

## Verification

大量文件同时写入：

Disk IO更加平滑。

吞吐率提升。

---

## Lessons Learned

统一调度比多个局部优化更加有效。

---

# QA008 为什么删除BufferCache？

## Question

为什么不同时维护BufferCache和PageCache？

---

## Phenomenon

开发初期：

```Plain Text
BufferCache

↓

Block Data

PageCache

↓

File Data
```

两套缓存同时存在。

---

## Investigation

同一Block：

```Plain Text
BufferCache

↓

Block 100

PageCache

↓

Block 100
```

数据重复。

一致性维护困难。

---

## Root Cause

两套缓存重复维护同一数据。

Memory浪费。

同步复杂。

---

## Fix

分类管理：

```Plain Text
File Data → PageCache
ext4 Metadata → MetaBlockCache (256 blocks)
ext4 Inode Dirty → 单独跟踪 (dirty_inode)
```

BufferCache 移除，文件数据统一进入 PageCache。但 ext4 元数据（block group descriptors, inode table 等）走 MetaBlockCache（固定 256 blocks），inode 脏状态独立跟踪，不经过 PageCache。

---

## Verification

连续IOzone测试：

Memory占用下降。

Cache一致性正常。

---

## Lessons Learned

一个数据只能有一份缓存。

统一缓存远优于多级重复缓存。

---

# 5\.2 本章总结

PageCache模块开发过程中，团队围绕缓存共享、生命周期管理、Dirty维护以及WriteBack调度进行了系统重构，逐步形成了统一缓存架构。通过引入唯一Owner机制、统一Dirty Queue、统一Page状态机以及统一WriteBack管理，成功解决了Page泄漏、重复缓存、同步写性能下降以及缓存一致性等问题。

最终形成如下统一缓存架构：

```Plain Text
User Read/Write
        ↓
      VFS
        ↓
    PageCache
         ↓
Page Index Lookup
         ↓
Cache Hit / Cache Miss
         ↓
InnerPageCache.dirty_pages
         ↓
writeback_all() / PageEntry state machine
         ↓
Block Device
```

整个PageCache模块最终实现了数据缓存、生命周期和IO调度的统一管理，为Memory、VFS和Block Device之间建立了稳定高效的数据通路，也成为MangoCore后续性能优化和Benchmark提升的重要基础。

# Chapter 6 mmap Research

# 从地址映射到统一虚拟内存管理

---

# 6\.1 mmap模块概述

mmap是连接用户虚拟地址空间与文件数据的重要机制，它允许用户程序直接通过内存访问文件，而无需频繁执行read/write系统调用。随着PageCache和AddressSpace逐渐完善，传统read/write方式已经无法满足高性能访问需求，因此项目逐步实现了统一的Memory Mapping机制。

开发过程中，团队重点解决了映射重复创建、Page共享异常、Page Fault处理不一致以及映射生命周期混乱等问题，并建立了AddressSpace、PageCache和VFS之间统一的映射关系。

统一分析流程如下：

```Plain Text
mmap Request → AddressSpace → VMA Lookup → PageCache Lookup → Page Fault → Mapping → Regression
```

---

# QA001 为什么需要mmap而不是read/write？

## Question

既然已经支持read和write，为什么还需要实现mmap？

---

## Phenomenon

程序连续读取文件：

```Plain Text
read → copy → user buffer → read → copy → user buffer
```

每次访问都需要：

- 系统调用； 

- 内核态切换； 

- 数据复制。 

CPU利用率持续增加。

---

## Investigation

分析访问流程：

```Plain Text
Application → System Call → VFS → PageCache → Copy → User Buffer
```

每一次读取都需要额外Copy。

大量CPU时间消耗在数据搬运。

---

## Root Cause

read/write属于数据复制模型。

无法直接共享PageCache中的数据。

---

## Fix

建立统一映射：

```Plain Text
Application Virtual Address → AddressSpace → PageCache Page
```

用户空间直接访问缓存页。

避免重复Copy。

---

## Verification

连续读取同一文件：

CPU利用率下降。

系统调用次数明显减少。

---

## Lessons Learned

mmap不仅是映射机制，更是零拷贝访问机制。

---

# QA002 为什么映射区域会重复创建？

## Question

同一个文件多次mmap，AddressSpace不断增长。

---

## Phenomenon

执行：

```Plain Text
mmap

↓

munmap

↓

mmap
```

观察：

```Plain Text
AddressSpace

8

↓

16

↓

24
```

映射数量不断增加。

---

## Investigation

检查：

Vma；

VMA；

PageTable。

发现：

旧VMA没有正确回收。

---

## Root Cause

munmap仅删除PageTable。

没有同步删除Vma。

---

## Fix

统一释放流程：

```Plain Text
munmap → Remove VMA → Release Mapping → Update AddressSpace
```

映射和地址空间同步释放。

---

## Verification

连续：

```Plain Text
mmap → munmap

10000次
```

AddressSpace保持稳定。

---

## Lessons Learned

地址映射不仅包含PageTable，还包含VMA管理。

---

# QA003 为什么Page Fault越来越多？

## Question

访问映射区域时，Page Fault频繁发生。

---

## Phenomenon

程序启动：

```Plain Text
mmap

↓

Access

↓

Page Fault

↓

Access

↓

Page Fault
```

重复触发异常。

---

## Investigation

分析流程：

```Plain Text
Access

↓

Page Fault

↓

Allocate Page

↓

Return
```

下一次访问：

再次Allocate。

Page没有建立映射。

---

## Root Cause

Page创建成功后，没有更新PageTable。

CPU每次访问都认为Page不存在。

---

## Fix

修改流程：

```Plain Text
Page Fault

↓

PageCache Lookup

↓

Allocate Page

↓

Update PageTable

↓

Resume
```

---

## Verification

再次访问：

```Plain Text
First Access → Page Fault

Second Access → Direct Access
```

Fault数量明显下降。

---

## Lessons Learned

Page Fault处理不仅负责分配Page，更需要维护映射关系。

---

# QA004 为什么多个进程共享映射异常？

## Question

父子进程共享映射区域时，数据出现不一致。

---

## Phenomenon

执行：

```Plain Text
fork

↓

mmap

↓

write
```

父进程数据更新。

子进程仍读取旧数据。

---

## Investigation

检查：

AddressSpace；

Page；

PageCache。

发现：

父子进程分别维护独立Page。

---

## Root Cause

共享映射没有共享Page。

fork重新创建Page。

---

## Fix

重新设计共享机制：

```Plain Text
Parent

↓

PageCache

↓

Shared Page

↓

Child
```

统一引用同一Page。

---

## Verification

父进程修改：

子进程立即可见。

共享映射正常。

---

## Lessons Learned

共享映射共享的是Page，而不是地址。

---

# QA005 为什么munmap后Page没有释放？

## Question

munmap完成后，Page数量没有减少。

---

## Phenomenon

执行：

```Plain Text
mmap

↓

Access

↓

munmap
```

heap\_trace：

```Plain Text
Page Exists
```

Memory没有回收。

---

## Investigation

分析：

```Plain Text
AddressSpace

↓

VMA

↓

PageCache
```

VMA释放。

Page仍属于PageCache。

---

## Root Cause

Page生命周期错误绑定VMA。

导致映射解除后缓存无法统一管理。

---

## Fix

重新定义：

```Plain Text
AddressSpace → Mapping

PageCache → Owner(Page)
```

Page生命周期完全交由PageCache。

---

## Verification

连续：

```Plain Text
mmap

munmap

IOzone
```

Page数量稳定。

---

## Lessons Learned

解除映射不等于释放缓存。

file-backed page 生命周期归 PageCache 管理；VMA/munmap 只管理映射引用，不直接释放缓存页。

---

# QA006 为什么建立统一VMA管理？

## Question

为什么每个映射不能单独维护？

---

## Phenomenon

系统同时存在：

```Plain Text
Anonymous Mapping

File Mapping

Shared Mapping
```

分别维护：

地址空间越来越复杂。

---

## Investigation

统计发现：

不同Mapping维护方式基本一致。

区别仅在Page来源。

---

## Root Cause

Mapping逻辑重复。

生命周期不统一。

---

## Fix

建立统一VMA：

```Plain Text
AddressSpace

↓

VMA

↓

Anonymous

File

Shared
```

统一管理所有映射区域。

---

## Verification

basic mmap 路径已验证；更广的 LTP mmap/munmap/mprotect 覆盖见归档测试结果。

代码复杂度明显下降。

---

## Lessons Learned

统一抽象比多个特殊实现更加稳定。

---

# QA007 为什么建立统一Memory Mapping Lifecycle？

## Question

为什么Mapping需要统一生命周期？

---

## Phenomenon

开发过程中频繁出现：

```Plain Text
VMA Leak

Page Leak

Mapping Leak
```

Long Running稳定性下降。

---

## Investigation

分析资源关系：

```Plain Text
Task

↓

AddressSpace

↓

VMA

↓

PageCache

↓

Page
```

多个模块共同维护生命周期。

---

## Root Cause

Owner不明确。

VMA、AddressSpace、PageCache共同维护Page。

导致引用混乱。

---

## Fix

重新定义统一生命周期：

```Plain Text
Create Mapping → VMA → Page Fault → Shared → Release Mapping → Recycle
```

Page生命周期独立管理。

---

## Verification

连续执行：

```Plain Text
mmap

fork

munmap

BusyBox

IOzone
```

系统稳定运行。

无Mapping泄漏。

---

## Lessons Learned

Mapping负责地址。

PageCache负责数据。

AddressSpace负责组织。

职责统一后生命周期才能保持一致。

---

# 6\.2 本章总结

mmap模块开发过程中，团队围绕地址映射、共享访问、Page Fault处理以及映射生命周期进行了系统重构，逐步形成统一Memory Mapping架构。通过建立统一VMA管理、统一Page Fault处理流程、统一共享Page机制以及统一Mapping生命周期，成功解决了映射重复创建、Page共享异常、Page泄漏以及地址空间管理混乱等问题。

最终形成如下统一映射架构：

```Plain Text
Application
      ↓
Virtual Address
      ↓
AddressSpace
      ↓
VMA Lookup
      ↓
PageCache Lookup
      ↓
Page Fault Handler
      ↓
Page Mapping
      ↓
Physical Memory
```

整个mmap模块最终实现了Memory、VFS和PageCache之间的统一映射关系，使文件访问由传统read/write复制模型逐步演进为基于共享Page的零拷贝访问模型，为后续Network零拷贝优化和整体IO性能提升奠定了基础。同时，统一生命周期和统一Owner机制保证了Long Running场景下映射资源能够稳定回收，进一步提升了MangoCore的工程稳定性和可维护性。

# Chapter 7 Network Research

# 从Socket通信到统一网络资源管理

---

# 7\.1 Network模块概述

Network模块的核心架构基于 `NetInterfaceInner { stacks, bindings }`。每个网络设备对应一个 `DeviceStack`，包含独立的 smoltcp Interface 和 SocketSet。Socket 层通过 `RouteSocketHandle → SocketBinding → smoltcp SocketHandle` 三级间接访问底层协议栈。路由由 `RouteTable/RouteKind/route_output` 管理，端口分配通过 `PortManager` 顺序递增。

核心数据结构：

- `NetInterfaceInner`：全局网络状态，包含 stacks（设备栈集合）和 bindings（socket 绑定表）
- `DeviceStack`：per-device smoltcp Interface + SocketSet
- `RouteSocketHandle`：kernel 内部路由/socket 间接句柄，关联到具体的 SocketBinding
- `SocketBinding`：持有 smoltcp SocketHandle 及 ifindex/proto 等路由元信息
- `RouteTable`：`RouteEntry` 列表，支持 Local/Connected/Gateway/Unreachable 四种路由类型
- `PortManager`：`NEXT_EPHEMERAL_PORT` 原子递增，32768-60999 范围

关键设计约束：

1. Socket 生命周期由进程 fd table 管理，关闭时同步清理 SocketBinding 和 PortManager 记录
2. 路由决策（route_output）先查 RouteTable，未命中则填默认路由
3. DHCP 获取的网关动态更新默认路由
4. 所有 socket I/O 非阻塞，通过 WaitQueue 实现阻塞语义

分析流程如下：

```Plain Text
Socket Create → RouteSocketHandle → SocketBinding → DeviceStack.Interface → smoltcp Socket → Data Transfer → Close → Cleanup
```

---

# QA001 为什么Socket关闭后资源没有释放？

## Question

应用程序执行close\(socket\)后，Kernel网络资源仍然持续增长。

---

## Phenomenon

连续执行：

```Plain Text
socket → connect → send → close → repeat
```

观察procfs：

```Plain Text
Socket Count

16

↓

64

↓

128

↓

256
```

Task数量保持稳定。

Socket对象不断增加。

---

## Investigation

分析资源关系：

```Plain Text
Task

↓

Socket

↓

Route

↓

Buffer
```

Socket关闭。

Route和Buffer仍然存在。

---

## Root Cause

Socket生命周期同时由Task和Route维护。

Owner不唯一。

导致资源无法统一释放。

---

## Fix

重新定义Owner：

```Plain Text
Task

↓

Socket

↓

Weak Route

↓

Weak Buffer
```

Socket负责生命周期。

Route仅负责访问。

---

## Verification

连续运行：

```Plain Text
BusyBox

↓

iperf

↓

Long Running
```

Socket数量保持稳定。

未出现资源泄漏。

---

## Lessons Learned

Socket只能有一个Owner。

共享对象必须采用WeakReference。

---

# QA002 为什么 fork 后端口分配碰撞（Heisenberg 问题）？

## Question

fork 后父子进程使用临时端口时出现随机碰撞，bind(127.0.0.1, 0) 返回 EINVAL 而非自动分配。

---

## Phenomenon

fork 后父子进程同时 connect/send，观察端口分配：

```Plain Text
Parent → port 45000
Child  → port 45000  (碰撞！)
```

或 bind(127.0.0.1, 0) 返回 EINVAL，而 Linux 上自动分配随机端口。

---

## Investigation

检查 PortManager 逻辑：

1. 原随机临时端口分配在 fork 后不稳定 — fork 复制了相同的 `NEXT_EPHEMERAL_PORT` 状态，父子竞争同一序列
2. bind(127.0.0.1, 0) 的 port=0 路径未按 Linux 语义处理 — 未触发自动分配
3. 临时端口范围与 /proc/sys/net/ipv4/ip_local_port_range 不匹配

---

## Root Cause

三方面问题：

```Plain Text
1. NEXT_EPHEMERAL_PORT: random → fork 后父子共轭状态 → 碰撞
2. bind(127.0.0.1, 0):  未实现 port=0 自动分配 → EINVAL
3. 端口范围:            未对齐 Linux 标准 32768-60999
```

---

## Fix

```Plain Text
1. NEXT_EPHEMERAL_PORT 改为 atomic 顺序递增（取消 random）
2. bind(127.0.0.1, 0) 走 Linux 自动分配路径
3. 端口范围设为 32768-60999（Linux 标准）
4. 跨 TCP/UDP 冲突检查
```

---

## Verification

连续 fork + connect 反复执行，无端口碰撞。bind(127.0.0.1, 0) 正确返回自动分配端口。范围 32768-60999 内分配无重复。

---

## Lessons Learned

fork 复制了所有进程状态，包括端口分配器的内部计数器。必须确保分配器状态在 fork 后不产生父子竞争。

# QA003 为什么Accept越来越慢？

## Question

大量连接建立后，Accept延迟明显增加。

---

## Phenomenon

压力测试：

```Plain Text
Listen

↓

Accept

↓

Accept

↓

Accept
```

Latency持续增加。

---

## Investigation

分析：

```Plain Text
Listen Queue

↓

Linear Search

↓

Find Connection
```

每次Accept遍历整个队列。

---

## Root Cause

Listen Queue采用顺序查找。

连接数增加后复杂度持续提高。

---

## Fix

重新设计：

```Plain Text
Connection

↓

Ready Queue

↓

Accept
```

Accept直接获取Ready Connection。

---

## Verification

iperf压力测试：

Accept延迟保持稳定。

---

## Lessons Learned

Accept性能主要取决于连接管理，而不是Socket本身。

---

# QA004 为什么Buffer越来越多？

## Question

网络通信结束后Buffer没有释放。

---

## Phenomenon

连续执行：

```Plain Text
send

↓

recv

↓

close
```

观察：

```Plain Text
Network Buffer

64

↓

256

↓

512

↓

1024
```

Memory不断增长。

---

## Investigation

分析：

```Plain Text
Socket

↓

Buffer

↓

Route
```

Socket关闭。

Buffer仍被Route引用。

---

## Root Cause

Buffer生命周期绑定多个模块。

Owner混乱。

---

## Fix

重新定义：

```Plain Text
Socket

↓

Buffer Owner

↓

Route WeakReference
```

Buffer统一由Socket管理。

---

## Verification

Long Running：

Buffer数量保持稳定。

Memory恢复正常。

---

## Lessons Learned

Buffer属于通信资源。

不能绑定Route生命周期。

---

# QA005 实际路由结构是什么样的？

## Question

MangoCore 的路由模块如何组织？Route 的职责边界是什么？

---

## Phenomenon

实际路由结构由 `RouteTable`、`RouteEntry`、`RouteKind` 和 `route_output()` 组成。

```Plain Text
RouteKind:
  - Local       → 本机地址直接交付
  - Connected   → 直连子网
  - Gateway     → 经网关转发
  - Unreachable → 不可达

RouteTable: Vec<RouteEntry>
route_output(): 查 RouteTable → 命中返回 nexthop
                 未命中 → 按 sk_daddr 自动填充默认路由
                 DHCP 网关 → 动态更新默认路由
```

没有单独的 Buffer Manager 或 Route Lifecycle Manager。路由只做路径选择。

---

## Investigation

`route_output()` 查找流程：

```Plain Text
socket send → get sk_daddr
  → RouteTable lookup by dst addr
  → RouteKind match:
       Local → loopback
       Connected → direct dev
       Gateway → gateway MAC
       Unreachable → error
  → return (dev, nexthop)
```

`NetNamespace` 提供网络命名空间隔离。

---

## Root Cause

路由模块负责路径选择，不负责 buffer 管理或生命周期。

路由表在以下时机更新：

1. 设备 up/down 时添加直连路由

2. DHCP 获取网关后更新默认路由

3. 应用程序通过 netlink 添加删除路由

---

## Fix

路由架构已固定为 RouteTable + route_output + NetNamespace。无需大重构：

```Plain Text
Socket → route_output(sk_daddr)
  → RouteKind match
  → DeviceStack lookup
  → send via smoltcp
```

---

## Verification

新增 loopback 或多设备场景无需修改路由核心。路由表查询和 DHCP 网关更新均工作正常。

---

## Lessons Learned

路由只做一件事：路径选择。不耦合 buffer、生命周期或其他职责。

# QA006 为什么Send/Recv路径仍有优化空间？

## Question

Send 路径能否减少数据拷贝次数？

---

## Phenomenon

发送流程：

```Plain Text
User Buffer

↓

Kernel Buffer

↓

Network Buffer

↓

NIC
```

存在两次Copy。

---

## Investigation

分析：

CPU大量时间用于Memory Copy。

---

## Root Cause

Send 过程中 UserBuffer → 临时 Vec → smoltcp 缓冲区的两次拷贝增加了 CPU 开销。当前 TCP try\_send\_user 路径仍需从 UserBuffer 拷贝到临时 Vec，不存在零拷贝发送。

---

## Fix

部分 recv/send 路径减少了额外拷贝次数（如减少中间缓冲层），但不存在 PageCache 或网络的零拷贝发送。收发路径仍有优化空间。

---

## Verification

结构优化后中间缓冲层减少，iperf 测试吞吐率有改善。

---

## Lessons Learned

网络性能优化的核心是减少数据拷贝，但零拷贝需要更底层的页共享机制支持，当前尚未完全实现。

---

# QA007 为什么建立统一Socket State？

## Question

为什么Socket不能只维护Open/Close状态？

---

## Phenomenon

网络运行过程中：

Socket可能处于：

```Plain Text
Connecting

Listening

Established

Closing
```

简单状态无法描述完整生命周期。

---

## Investigation

多个模块分别维护状态。

导致：

```Plain Text
Close

↓

Recv

↓

Send
```

状态竞争。

---

## Root Cause

Socket生命周期定义过于简单。

无法统一管理资源。

---

## Fix

建立统一状态机：

```Plain Text
Create

↓

Bind

↓

Listen

↓

Accept

↓

Established

↓

Closing

↓

Closed
```

统一状态转换。

---

## Verification

网络相关 BusyBox 测试通过。

状态转换正常。

---

## Lessons Learned

状态机比多个布尔变量更加可靠。

---

# QA008 为什么建立统一Network Owner？

## Question

为什么Socket、Route、Buffer不能分别管理生命周期？

---

## Phenomenon

开发过程中频繁出现：

```Plain Text
Socket Leak

Buffer Leak

Route Leak
```

Long Running稳定性下降。

---

## Investigation

分析资源关系：

```Plain Text
Task

↓

Socket

↓

Route

↓

Buffer

↓

Device
```

多个模块共同维护生命周期。

---

## Root Cause

Network缺少统一Owner。

Reference长期无法归零。

---

## Fix

重新设计统一资源模型：

```Plain Text
Task

↓

Socket

↓

Route Lookup

↓

smoltcp Socket Buffer (SocketSet)

↓

Network Device (adapter.rs)
```

Socket作为唯一Owner。

其它模块全部采用共享引用。

---

## Verification

连续执行：

```Plain Text
BusyBox

↓

iperf

↓

Long Running

↓

Regression
```

Socket、Route、Buffer数量保持稳定。模块级回归检查通过（覆盖网络核心路径的构造与析构）。

---

## Lessons Learned

网络资源管理最重要的不是通信，而是生命周期统一。

---

# QA009 为什么建立统一Network Regression？

## Question

为什么每次修改网络模块都必须重新跑Benchmark？

---

## Phenomenon

修复Buffer问题后：

```Plain Text
Buffer Fix

↓

iperf正常

↓

BusyBox异常
```

修复Route：

```Plain Text
Route Fix

↓

Socket正常

↓

Memory增长
```

---

## Investigation

Network与Memory、VFS高度耦合。

局部修复容易影响整个Kernel。

---

## Root Cause

网络模块不是独立模块，而是Kernel资源管理的一部分。

必须进行全局验证。

---

## Fix

建立统一Regression流程：

```Plain Text
Compile

↓

BusyBox

↓

iperf

↓

IOzone

↓

Long Running

↓

Merge
```

 合并前需通过模块级回归检查。

---

## Verification

统一Regression后：

- Socket稳定； 

- Buffer稳定； 

- Route稳定； 

- Long Running 未出现退化。 

---

## Lessons Learned

网络优化必须由Benchmark验证，而不能依赖经验判断。

---

# 7\.2 本章总结

Network模块的核心架构为 `NetInterfaceInner { stacks, bindings }`，每个设备独立 `DeviceStack`。Socket 通过 `RouteSocketHandle → SocketBinding → smoltcp SocketHandle` 三级间接访问。路由由 `RouteTable/RouteKind/route_output` 管理，端口由 `PortManager` 顺序递增分配。

最终形成如下网络架构：

```Plain Text
Application
      ↓
Socket API (syscall)
      ↓
RouteSocketHandle
      ↓
SocketBinding (per-protocol)
      ↓
DeviceStack.Interface (smoltcp)
      ↓
RouteTable / route_output
      ↓
Network Device (adapter → virtio_net)
```

网络系统经过多轮调试解决了 fork 后端口碰撞、路由表构建、DHCP 网关动态更新等问题。模块级回归检查通过，见 testresult/ 目录归档。

# Chapter 8 Driver Research

# 从设备访问到统一驱动资源管理

---

# 8\.1 Driver模块概述

Driver模块负责连接Kernel与外部设备，为Block Device、Network Device以及Console等硬件提供统一访问接口。随着PageCache、Network和VFS逐渐完善，传统驱动直接管理Buffer和DMA资源的方式开始暴露出生命周期混乱、重复申请、设备状态不一致以及中断处理复杂等问题。

项目开发过程中，团队围绕统一Driver抽象、统一Buffer管理、统一DMA生命周期以及统一Device状态机进行了多轮重构，使驱动模块逐步形成模块解耦、资源统一和生命周期统一的工程架构。

统一分析流程如下：

```Plain Text
Device Request → Driver Layer → Buffer/DMA → Interrupt → Resource Release → Regression
```

---

# QA001 为什么建立统一Driver接口？

## Question

不同设备分别实现自己的Read/Write接口是否可行？

---

## Phenomenon

项目初期：

```Plain Text
Block Device → Read()

Network Device → Read()

Console → Read()
```

每个驱动维护自己的接口。

随着设备增加：

- Block 

- Network 

- Console 

- VirtIO 

接口越来越分散。

---

## Investigation

统计驱动流程：

```Plain Text
Request

↓

Submit

↓

Wait

↓

Complete
```

绝大多数设备流程一致，仅底层实现不同。

---

## Root Cause

设备不同，但驱动模型一致。

重复实现导致：

- 接口重复； 

- 生命周期重复； 

- 扩展困难。 

---

## Fix

建立统一Driver接口：

```Plain Text
Device Request

↓

Driver Operation

↓

Block / Network / Console
```

所有设备统一实现Operation。

---

## Verification

新增设备无需修改Kernel调用层。

Driver统一完成分发。

---

## Lessons Learned

统一接口比统一实现更加重要。

---

# QA002 为什么DMA Buffer越来越多？

## Question

设备完成IO后DMA Buffer没有释放。

---

## Phenomenon

连续执行：

```Plain Text
Read

↓

Write

↓

Repeat
```

观察：

```Plain Text
DMA Buffer

32

↓

128

↓

256

↓

512
```

Memory持续增长。

---

## Investigation

分析资源关系：

```Plain Text
Driver

↓

DMA Buffer

↓

Device Queue
```

IO结束。

DMA Buffer仍然存在。

---

## Root Cause

DMA生命周期同时由Driver和Device维护。

Owner不唯一。

---

## Fix

重新定义：

```Plain Text
Driver

↓

DMA Manager

↓

Device Queue
```

DMA统一由Driver管理。

Device仅负责访问。

---

## Verification

Long Running：

DMA Buffer数量保持稳定。

---

## Lessons Learned

DMA属于Kernel资源。

不能绑定设备生命周期。

---

# QA003 为什么Interrupt越来越复杂？

## Question

多个设备同时产生Interrupt时，系统处理效率下降。

---

## Phenomenon

系统运行：

```Plain Text
Block Interrupt

↓

Network Interrupt

↓

Console Interrupt
```

Interrupt Handler越来越长。

---

## Investigation

统计Interrupt职责：

负责：

- Acknowledge； 

- Buffer释放； 

- Wakeup； 

- Schedule。 

职责不断增加。

---

## Root Cause

Interrupt承担了业务逻辑。

导致：

- Handler复杂； 

- Latency增加； 

- 可维护性下降。 

---

## Fix

重新设计：

```Plain Text
Interrupt

↓

Record Event

↓

Wake Worker

↓

Return

↓

Worker Process
```

中断只负责通知。

业务统一交给Worker。

---

## Verification

大量IO测试：

Interrupt响应保持稳定。

CPU占用下降。

---

## Lessons Learned

Interrupt越短越稳定。

复杂逻辑应放到进程上下文。

---

# QA004 为什么Block Driver与PageCache重复缓存？

## Question

Block Driver已经有Buffer，为什么PageCache还需要缓存？

---

## Phenomenon

开发初期：

```Plain Text
Block Buffer

↓

Disk Block
```

同时：

```Plain Text
PageCache

↓

Disk Block
```

存在两份缓存。

---

## Investigation

同一Block：

```Plain Text
Block Buffer

↓

Block100

PageCache

↓

Block100
```

数据同步困难。

Memory浪费明显。

---

## Root Cause

缓存职责重复。

Driver承担了缓存功能。

---

## Fix

重新设计：

```Plain Text
Application

↓

PageCache

↓

Driver

↓

Device
```

Driver不保存数据。

所有缓存统一交给PageCache。

---

## Verification

IOzone测试：

Memory下降。

Cache一致性正常。

---

## Lessons Learned

Driver负责设备。

Cache负责数据。

职责必须分离。

---

# QA005 为什么Device状态容易混乱？

## Question

设备初始化后仍然出现Busy状态异常。

---

## Phenomenon

运行：

```Plain Text
Probe

↓

Init

↓

Ready

↓

Busy

↓

Ready
```

部分设备：

始终停留Busy。

---

## Investigation

检查：

Device Flag；

Driver State；

Queue。

发现多个模块同时修改状态。

---

## Root Cause

状态维护分散。

不同模块分别更新Device状态。

---

## Fix

建立统一状态机：

```Plain Text
Probe

↓

Init

↓

Ready

↓

Running

↓

Closing

↓

Offline
```

所有状态统一转换。

---

## Verification

连续：

```Plain Text
Load Driver

Unload Driver

1000次
```

全部正常。

---

## Lessons Learned

状态机比多个Flag更加可靠。

---

# QA006 为什么建立统一Driver Owner？

## Question

为什么Buffer、DMA、Device不能分别管理生命周期？

---

## Phenomenon

开发过程中频繁出现：

```Plain Text
DMA Leak

Buffer Leak

Device Leak
```

Long Running稳定性下降。

---

## Investigation

分析资源关系：

```Plain Text
Driver

↓

DMA

↓

Buffer

↓

Device

↓

Interrupt
```

多个模块共同维护生命周期。

---

## Root Cause

Driver缺少统一Owner。

Reference长期无法归零。

---

## Fix

重新设计：

```Plain Text
Driver

↓

DMA Manager

↓

Buffer Manager

↓

Device

↓

Interrupt
```

Driver作为统一Owner。

其它模块全部采用共享访问。

---

## Verification

连续执行：

```Plain Text
BusyBox

↓

IOzone

↓

iperf

↓

Long Running
```

DMA、Buffer、Device数量全部保持稳定。

Regression全部通过。

---

## Lessons Learned

驱动开发的重点不是设备访问，而是资源生命周期管理。

---

# QA007 为什么建立统一Driver Regression？

## Question

为什么修改驱动必须重新执行全部Benchmark？

---

## Phenomenon

修复DMA：

```Plain Text
DMA Fix

↓

Block正常

↓

Network性能下降
```

修复Interrupt：

```Plain Text
Interrupt Fix

↓

Console正常

↓

IOzone下降
```

---

## Investigation

Driver与Memory、PageCache、Network高度耦合。

局部优化容易影响整个Kernel。

---

## Root Cause

Driver是所有资源访问入口。

必须进行全局验证。

---

## Fix

建立统一Regression：

```Plain Text
Compile

↓

BusyBox

↓

IOzone

↓

iperf

↓

Long Running

↓

Merge
```

任何测试失败禁止提交。

---

## Verification

统一Regression后：

- Driver稳定； 

- DMA稳定； 

- Device稳定； 

- Long Running 未出现退化。 

---

## Lessons Learned

驱动优化必须依赖Benchmark，而不能依赖功能测试。

---

# 8\.2 本章总结

Driver模块的开发围绕设备 trait 抽象展开。BlockDevice 和 NetDevice 两个核心 trait 定义了块/网设备的统一接口，virtio-blk 和 virtio-net 驱动分别实现对应 trait。驱动层内部使用 per-driver frame 跟踪管理 DMA 缓冲区生命周期，设备状态通过 HAL 层隔离架构差异。通过建立统一 Device trait 接口、统一 Interrupt 处理、统一 Device 状态机以及统一 Driver 资源管理（DMA frame 跟踪），成功解决了 DMA 泄漏、重复缓存、设备状态混乱以及 Interrupt 复杂度持续增加等典型问题。

最终形成如下驱动架构：

```Plain Text
Application
      ↓
System Call
      ↓
BlockDevice / NetDevice trait
      ↓
virtio-blk / virtio-net (trait impl)
      ↓
DMA frame tracking (per-driver)
      ↓
Interrupt Handler (HAL)
      ↓
Device (QEMU virtio)
```

整个Driver模块基于 trait 接口实现了块/网设备的统一抽象，使各驱动在各自生命周期内独立管理资源，并通过 Benchmark 驱动优化持续提升性能。模块级回归检查通过，见 testresult/ 目录归档。

# Chapter 9 Regression Research

# 从功能验证到Benchmark驱动开发

---

# 9\.1 Regression模块概述

随着Memory、Process、VFS、PageCache、mmap、Network以及Driver模块逐步完善，Kernel开发逐渐进入持续优化阶段。然而，在实际开发过程中发现，大量Bug虽然能够快速修复，却经常导致其他模块性能下降或者资源管理异常，形成典型的Regression问题。

因此，项目逐步建立了以Benchmark为核心、以Long Running为保障、以Regression为准入条件的统一测试体系。CI 自动执行编译与 basic/busybox 冒烟测试；全量测试由开发者按需触发。

统一分析流程如下：

```Plain Text
Code Change → Compile → Functional Test → Benchmark → Long Running → Regression → Merge
```

---

# QA001 为什么不能只验证功能正确？

## Question

代码已经能够正常运行，为什么还需要执行完整Regression？

---

## Phenomenon

一次Memory模块优化：

```Plain Text
Memory Fix

↓

Compile Success

↓

BusyBox Success

↓

Merge
```

上线后：

```Plain Text
IOzone Performance

↓

明显下降
```

功能全部正常。

性能明显退化。

---

## Investigation

进一步分析：

Memory释放策略改变后，

影响了：

```Plain Text
Memory

↓

PageCache

↓

WriteBack

↓

IOzone
```

多个模块共同变化。

---

## Root Cause

Kernel属于高度耦合系统。

功能正确并不能证明系统正确。

性能、资源管理和生命周期都必须同时验证。

---

## Fix

建立统一Regression：

```Plain Text
Compile

↓

BusyBox

↓

libc-test

↓

IOzone

↓

iperf

↓

Long Running

↓

Merge
```

任何一项失败禁止提交。

---

## Verification

Regression建立后：

性能退化问题全部能够提前发现。

---

## Lessons Learned

Kernel开发必须同时验证功能、性能和稳定性。

---

# QA002 为什么建立Benchmark驱动开发？

## Question

为什么所有优化必须配合Benchmark？

---

## Phenomenon

项目开发初期：

优化依据：

```Plain Text
Developer Experience

↓

Code Review

↓

Merge
```

不同开发人员评价标准不同。

---

## Investigation

统计发现：

很多"看起来更好"的优化：

实际上：

```Plain Text
CPU增加

Memory增加

Latency增加
```

无法量化。

---

## Root Cause

缺少统一评价标准。

优化无法客观比较。

---

## Fix

建立统一Benchmark：

```Plain Text
BusyBox

↓

libc-test

↓

IOzone

↓

iperf

↓

Long Running
```

所有优化全部使用数据评价。

---

## Verification

优化结果：

全部可以量化。

无需依赖个人经验。

---

## Lessons Learned

Benchmark比经验更加可靠。

---

# QA003 为什么建立Long Running测试？

## Question

为什么功能全部正常，仍然需要运行数小时测试？

---

## Phenomenon

BusyBox：

全部通过。

IOzone：

全部通过。

但是：

```Plain Text
Long Running

↓

Memory持续增长

↓

OOM
```

短时间无法发现问题。

---

## Investigation

heap\_trace显示：

```Plain Text
Page

↓

Reference

↓

长期无法归零
```

Leak仅在长时间运行后出现。

---

## Root Cause

生命周期Bug具有累积效应。

短时间测试无法暴露。

---

## Fix

建立统一Long Running：

```Plain Text
BusyBox

↓

IOzone

↓

iperf

↓

Continuous
```

持续观察：

Memory；

Socket；

Page；

Task。

---

## Verification

Long Running通过后：

系统资源保持稳定。

---

## Lessons Learned

稳定性来源于时间，而不是一次运行。

---

# QA004 为什么建立统一Regression Case？

## Question

为什么每修复一个Bug，都需要建立Regression Case？

---

## Phenomenon

项目开发过程中：

```Plain Text
Page Leak

↓

Fix

↓

两周后再次出现
```

重复Debug。

---

## Investigation

统计历史问题：

大量Bug属于：

```Plain Text
曾经修复

↓

再次引入
```

没有自动检测。

---

## Root Cause

修复了代码。

没有修复开发流程。

---

## Fix

建立Regression Case：

```Plain Text
Bug

↓

Test Case

↓

CI

↓

Regression
```

所有历史Bug永久保存。

---

## Verification

再次修改相关模块：

Regression自动发现异常。

避免重复Bug。

---

## Lessons Learned

优秀的Kernel不会重复修复同一个Bug。

---

# QA005 为什么建立统一Benchmark Matrix？

## Question

为什么每个模块都必须执行全部Benchmark？

---

## Phenomenon

修复Network：

```Plain Text
iperf

Pass
```

但是：

```Plain Text
IOzone

Fail
```

修复Memory：

```Plain Text
BusyBox

Pass
```

但是：

```Plain Text
Long Running

Fail
```

---

## Investigation

Kernel模块高度共享资源：

```Plain Text
Memory

↓

PageCache

↓

VFS

↓

Network

↓

Driver
```

修改一个模块影响整个系统。

---

## Root Cause

局部测试无法代表整体稳定性。

---

## Fix

建立统一Benchmark Matrix：

```Plain Text
Memory

↓

BusyBox

IOzone

iperf

Long Running

↓

Pass
```

所有模块统一验证。

---

## Verification

Regression覆盖全部核心路径。

系统稳定性持续提升。

---

## Lessons Learned

Kernel验证必须覆盖全局。

---

# QA006 为什么建立统一Resource Monitor？

## Question

为什么Benchmark之外还需要资源监控？

---

## Phenomenon

Benchmark：

全部通过。

但是：

```Plain Text
Memory

+2MB/hour

Socket

+5/hour

Page

+10/hour
```

资源持续增长。

---

## Investigation

功能正常。

性能正常。

只有资源曲线异常。

---

## Root Cause

部分Leak不会立即影响功能。

只能通过趋势分析发现。

---

## Fix

建立统一Monitor：

```Plain Text
Benchmark

↓

Resource Collect

↓

Memory

Socket

Page

Task

↓

Trend Analysis
```

持续记录资源变化。

---

## Verification

Long Running期间：

已观测指标（Memory/Page/Task/Socket）在该测试窗口内未持续增长。

---

## Lessons Learned

趋势分析比瞬时数据更重要。

---

# QA007 为什么建立统一Performance Baseline？

## Question

为什么需要保存历史性能数据？

---

## Phenomenon

一次优化后：

开发人员认为：

```Plain Text
感觉更快
```

实际：

```Plain Text
IOzone

下降8%
```

无法准确判断。

---

## Investigation

缺少历史数据。

无法比较版本变化。

---

## Root Cause

没有Performance Baseline。

优化不可量化。

---

## Fix

建立按时间戳归档的测试结果摘要：

```Plain Text
Commit

↓

Benchmark

↓

Result

↓

testresult/目录
```

测试记录保留于 `testresult/` 目录。

---

## Verification

开发者可查阅历史测试结果变化趋势，但未建立正式性能基线数据库与门禁系统。

---

## Lessons Learned

优化必须可比较，当前通过归档日志实现人工比较。

---

# QA008 为什么建立Benchmark驱动工程文化？

## Question

为什么坚持"没有Benchmark，不允许Merge"？

---

## Phenomenon

项目开发初期：

```Plain Text
Code Review

↓

Merge
```

后期：

Bug频繁回归。

性能不断波动。

---

## Investigation

团队开发标准不一致。

缺少统一质量控制。

---

## Root Cause

开发流程依赖经验。

没有统一工程规范。

---

## Fix

建立统一开发流程：

```Plain Text
Requirement

↓

Code

↓

Compile

↓

Benchmark

↓

Regression

↓

Long Running

↓

Merge
```

Benchmark成为Merge前置条件。

---

## Verification

整个开发周期：

Memory；

VFS；

Network；

Driver；

均保持稳定演进。

Regression全部通过。

---

## Lessons Learned

Benchmark不是测试工具，而是Kernel工程质量控制体系。

---

# 9\.2 本章总结

Regression模块开发过程中，团队逐步完成了从"代码能够运行"到"代码能够持续稳定运行"的工程演进。通过建立统一Regression流程、统一Benchmark体系、统一Long Running验证、统一Regression Case以及统一Performance Baseline，成功解决了性能回退、资源泄漏重复出现以及优化不可量化等问题。

最终形成如下统一工程验证架构：

```Plain Text
Developer Change
        ↓
Compile
        ↓
Functional Test
        ↓
Benchmark Matrix
        ↓
Resource Monitor
        ↓
Long Running
        ↓
Regression Case
        ↓
Performance Baseline
        ↓
Merge
```

Regression体系成为MangoCore开发流程的核心保障，使Memory、Process、VFS、PageCache、mmap、Network以及Driver等模块能够在统一Benchmark和统一资源监控框架下持续演进。整个项目逐步形成了**以数据驱动开发、以Benchmark验证优化、以Regression保证稳定**的工程实践模式。

# Chapter 10 Lessons Learned

# 从功能实现到可验证的 Kernel 工程实践

---

# 10\.1 本章概述

整个MangoCore开发过程中，我们并不是按照预先设计好的架构一次性完成所有功能，而是在不断实现、不断测试、不断重构和不断验证的过程中逐步演进。

项目从最初能够启动内核、运行用户程序，到最终形成统一Memory管理、统一Process生命周期、统一VFS抽象、统一PageCache、统一mmap机制、统一Network资源管理以及统一Driver接口，经历了大量设计调整和工程实践。

整个开发过程可以概括为：

```Plain Text
Implement
     │
     ▼
Run Benchmark
     │
     ▼
Find Problem
     │
     ▼
Root Cause Analysis
     │
     ▼
Refactor Design
     │
     ▼
Regression
     │
     ▼
Stable Kernel
```

团队最终形成了一套以数据驱动、Benchmark驱动和Regression驱动的Kernel开发模式。

---

# Lesson 1 功能正确不代表系统正确

项目开发初期，团队更关注：

```Plain Text
Compile Success

↓

Program Run

↓

Task Finish
```

认为程序能够运行即代表功能完成。

然而随着Memory、PageCache以及Network模块不断完善，发现：

- 功能正常； 

- BusyBox全部通过； 

- 但Long Running持续泄漏资源； 

- Benchmark性能持续下降。 

最终认识到：

```Plain Text
Function Correct

≠

Kernel Correct
```

Kernel必须同时满足：

- 功能正确； 

- 生命周期正确； 

- 性能稳定； 

- 长时间稳定运行。 

---

# Lesson 2 生命周期比算法更加重要

开发过程中，大多数Bug都不是算法错误，而是生命周期错误。

例如：

```Plain Text
Memory Leak

Page Leak

Socket Leak

DMA Leak

Mapping Leak
```

最终发现：

所有Leak几乎都来源于同一个问题：

```Plain Text
Multiple Owner

↓

Reference Cycle

↓

Never Release
```

因此团队在关键子系统（如 PageCache 页生命周期、Vma 映射管理）中统一采用：

```Plain Text
One Resource

→

One Owner

→

Multiple Weak Reference
```

原则。但需注意不同子系统实际的 Ownership 模式存在差异：例如网络 socket 仍使用 Arc 共享所有权，VFS 的 IndexNode 通过 Arc 跨模块引用，并非所有资源都严格遵循单一 Owner 模型。

---

# Lesson 3 模块解耦优于局部优化

项目初期：

很多模块同时负责多个职责。

例如：

```Plain Text
PageCache

↓

Cache

Dirty

WriteBack

Lifecycle
```

Network同时维护：

```Plain Text
Socket

Route

Buffer

Device
```

随着功能增加，复杂度快速增长。

经过多轮重构，最终统一采用：

```Plain Text
One Module

↓

One Responsibility
```

例如：

```Plain Text
PageCache

↓

Only Cache

Memory

↓

Only Allocation

Driver

↓

Only Device

Network

↓

Only Communication
```

模块职责明确后：

代码复杂度明显下降。

---

# Lesson 4 Benchmark比经验更加可靠

很多优化看起来更合理。

例如：

```Plain Text
减少锁

减少Copy

提前Allocate
```

但Benchmark结果却可能更差。

项目后期：

所有优化全部采用：

```Plain Text
Code

↓

Benchmark

↓

Compare

↓

Merge
```

不再依赖开发人员经验。

团队统一形成：

> 没有Benchmark的数据支撑，不接受任何性能优化结论。
> 
> 

---

# Lesson 5 Regression不是测试，而是开发流程

开发过程中发现：

很多Bug已经修复。

几周后再次出现。

原因不是代码质量，而是：

```Plain Text
Fix

↓

Forget

↓

Refactor

↓

Bug Return
```

因此建立：

```Plain Text
Bug

↓

Regression Case

↓

Permanent Test
```

所有历史Bug全部永久保留测试。

Regression成为Merge的必要条件。

---

# Lesson 6 长时间稳定运行比一次Benchmark更重要

开发初期：

只执行：

```Plain Text
BusyBox

↓

Pass
```

认为Kernel稳定。

后续发现：

```Plain Text
Run

1 min

Pass

↓

Run

3 hour

Memory Leak

↓

Run

8 hour

OOM
```

最终建立：

统一Long Running：

```Plain Text
BusyBox

↓

IOzone

↓

iperf

↓

Continuous Run

↓

Resource Monitor
```

任何资源趋势异常都必须重新分析。

---

# Lesson 7 Kernel开发本质是资源管理

整个开发过程中：

Memory：

管理Page。

Process：

管理Task。

VFS：

管理File。

PageCache：

管理Cache。

Network：

管理Socket。

Driver：

管理Device。

最终发现：

Kernel真正管理的是：

```Plain Text
Resource

↓

Create

↓

Reference

↓

Share

↓

Release
```

而不是简单的系统调用。

整个Kernel实际上是一套统一资源生命周期管理系统。

---

# Lesson 8 Benchmark驱动形成统一工程文化

随着项目推进，团队逐步形成统一开发流程。

任何功能开发全部遵循：

```Plain Text
Requirement

↓

Design

↓

Implement

↓

Compile

↓

BusyBox

↓

Benchmark

↓

Regression

↓

Long Running

↓

Merge
```

所有成员采用同一标准。

所有优化均能够量化。

所有修改均可以追踪。

整个Kernel开发逐步由个人经验驱动演变为数据驱动。

---

# Lesson 9 MangoCore的发展过程

整个项目的发展并不是简单增加模块，而是不断统一架构。

整体演进路线如下：

```Plain Text
Boot
 │
 ▼
Memory
 │
 ▼
Process
 │
 ▼
VFS
 │
 ▼
PageCache
 │
 ▼
mmap
 │
 ▼
Network
 │
 ▼
Driver
 │
 ▼
Regression
 │
 ▼
Engineering Kernel
```

每一次重构都遵循三个原则：

```Plain Text
统一抽象

统一生命周期

统一Benchmark验证
```

Kernel逐步由功能集合演进为统一资源管理系统。

---

# 10\.2 全文总结

MangoCore开发过程中，团队围绕Memory、Process、VFS、PageCache、mmap、Network、Driver等核心模块进行了持续重构与优化，逐步建立了统一资源生命周期管理机制、统一模块抽象接口以及统一Benchmark驱动开发流程。

最终形成了如下完整工程体系：

```Plain Text
MangoCore

                        │

        ┌───────────────┼───────────────┐

        │                               │

  Unified Resource              Unified Interface

        │                               │

 Memory  Process  VFS  PageCache  mmap  Network  Driver

        │                               │

        └───────────────┼───────────────┘

                        │

             Benchmark Driven Development

                        │

             Regression + Long Running

                        │

             Stable & Engineering Kernel
```

经过持续的设计演进与工程实践，MangoCore已经不再是简单的课程项目或功能实现，而是形成了一套具有统一资源管理思想、统一生命周期设计、统一性能验证体系和统一工程规范的现代操作系统内核。

**整个Deep Research Report也形成了一条完整且连贯的主线：**

> **从发现问题（Phenomenon），到定位问题（Root Cause），再到重构设计（Fix），最后通过Benchmark和Regression完成验证（Verification），不断推动MangoCore由“能够运行”演进为“能够长期稳定运行”的工程化操作系统。**
> 
> 



