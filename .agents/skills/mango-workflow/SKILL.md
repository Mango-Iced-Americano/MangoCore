---
name: mango-workflow
description: 自动维护 oskernel2026-mango 项目的工作日志、可复用经验模式，以及性能调试知识库。每次代码修改后触发：更新 docs/Work_Log/YYYY-MM-DD.md；调试/性能任务前加载 references/ 作为前置参考；发现可复用模式时沉淀到 references/。
version: 1.1.0
allowed-tools: Read, Write, Edit, Grep, Bash, Glob
---

# Mango Worklog & Knowledge Harness

你是 oskernel2026-mango 项目的知识管理员。你的职责是：
- **代码修改后**自动记录工作日志到 `Work_Log.md`
- **调试/查性能前**加载 `references/` 作为前置参考，避免重复踩坑
- **发现可复用的经验时**沉淀到 `references/`

## 触发条件

以下任一情况发生时，必须执行本 Skill：

1. 完成了一次代码修改（无论大小）
2. 修复了一个 bug
3. 新增了一个功能
4. 用户说"记录一下"、"更新 worklog"、"沉淀经验"
5. 编译或测试结果有值得记录的发现
6. **涉及性能调试、渐进退化、计数器插桩时** — 先加载 references/，再动手
7. **涉及文档更新** — 修改代码后，检查 docs/ 中对应模块文档是否需要同步更新

## 工作流程

### 0. 前置参考（调试/性能任务时读）

开始调试性能退化、非确定性 bug、或遇到可疑模式前，先加载以下参考：

| 场景 | 读什么 |
|------|--------|
| 性能漂移 / 渐进退化 | `references/harness-patterns.md`（§渐进性能退化调试方法论） |
| 常见 Bug 模式 | `references/debugging-patterns.md`（按子系统分类） |

读完后再开始分析和插桩，避免从零开始摸索。

### A. 更新 Work_Log（每次修改后）

在 `docs/Work_Log/YYYY-MM-DD.md` 顶部追加日期戳条目（如果当天已有条目则追加到该条目下）：

```markdown
## YYYY-MM-DD

### 简短标题（描述本次修改）

**涉及文件：**
- `path/to/file1.rs` — 改了什么
- `path/to/file2.rs` — 改了什么

**验证：**
- `make rv64-kernel-build-only` ✅
- `make la64-kernel-build-only` ✅
- QEMU 测试结果（如有）

**备注：**（可选，值得注意的边界条件或已知限制）
```

### B. 沉淀经验（发现可复用模式时）

如果本次修改揭示了**可能跨对话复用**的经验，追加到对应 reference 文件；同时检查 reference 中是否有**过时或已被 AGENTS.md 完整覆盖**的内容，如有则删除。

| 经验类型 | 目标文件 |
|---------|---------|
| Bug 根因 → 修复模式 | `references/harness-patterns.md` |
| 调试技巧 / 排查方法 | `references/debugging-patterns.md` |

格式：
```markdown
## [现象简述]

- **根因**: ...
- **修复**: ...
- **教训**: ...
- **相关文件**: `path/to/file.rs`
```

### C. 判断标准：该不该沉淀？

✅ 应该沉淀：
- 同一个 bug 可能在不同模块复现（如 TLB flush、锁顺序）
- Linux ABI 对齐规则（如 errno 优先级）
- 本项目特有的编译/构建约束

❌ 不沉淀：
- 一次性 typo 或语法错误
- 已经在 AGENTS.md Critical Pitfalls 中覆盖的
- 纯项目特定、不可能复现的

### D. 同步文档（每次代码修改后）

**特殊文档：`AI-Usage-Report.md`**
- 当使用**新的 AI 工具/模型**时，需更新第 2 节工具清单
- 当发生**重要的 AI-assisted 成果**（如 Oracle 发现关键 bug 根因）时，需更新第 5 节案例表和第 7/8 节证据表
- 每次更新 Work_Log 后，检查是否需要在 AI-Usage-Report.md 中补充对应记录

修改代码后，检查本次改动的源文件是否命中 `docs/` 下某篇文档 YAML frontmatter 中的 `code_paths` 字段：

```markdown
---
code_paths:
  - "os/src/net/config.rs"
  - "os/src/net/routing.rs"
---
```

**检查方法：**
1. 列出本次修改的所有 `.rs` 源文件路径
2. 搜索 `docs/` 下所有 `.md` 文件的 `code_paths` 字段，检查是否有匹配
3. 如果有匹配，则该文档需要更新或标记为 `draft`

**操作：**
- 如果文档内容与当前源码一致 → 无需操作
- 如果文档已过时 → 更新文档内容，或至少在 frontmatter 中将 `status` 改为 `draft`
- 如果文档新增/重构 → 更新 `related_docs` 和 `entry_points`

**搜索命令参考：**
```bash
# 查找 docs/ 下引用了某源码路径的文档
rg -l "os/src/net/config.rs" docs/ --type md
```

### E. 证据纪律（子任务验证）

任何子任务（subagent）在报告测试结果时，必须提供可验证的执行证据，而不是仅凭临时日志声明成功。

**证据归档规范：**

- **唯一持久归档路径**：`docs/Work_Log/evidence/YYYY-MM-DD/` 是唯一受版本控制的证据归档目录。每个日期只能有一个日期目录，所有当天可验收证据必须写入该目录。
- **日期目录命名**：固定使用 `YYYY-MM-DD`，如 `docs/Work_Log/evidence/2026-07-16/`。同一天的不同测试通过文件名前缀区分，不得再创建多个主题子目录。
- **禁止使用 `testresults/`**：项目根目录 `testresults/` 已保留，禁止写入任何证据文件。子任务不得以任何理由向此路径输出内容。
- **`testresult/` 仅限临时输出**：项目根目录 `testresult/` 已被 `.gitignore` 忽略，可作为测试框架的临时工作区，但其内容不具备验收效力。要验收的证据必须复制到当天的 `docs/Work_Log/evidence/YYYY-MM-DD/` 持久路径。

**硬性要求：**

- **Docker 隔离**：所有编译和测试须在 Docker 容器内完成。记录 container ID 和宿主机工作目录到容器的挂载映射（`docker inspect <container> --format '{{range .Mounts}}{{println .Source "->" .Destination}}{{end}}'`）。
- **持久化证据路径**：证据必须写入当天的 `docs/Work_Log/evidence/YYYY-MM-DD/` 目录，该目录受版本控制，容灾后仍可访问。容器 `/tmp` 下的临时日志和 `testresult/` 下的临时输出均不是有效证据，子任务不得以此作为完成依据。
- **结果元数据**：每次测试运行的结果目录下至少包含以下文件：
  - `git-hash.txt` — 内核 commit（`git describe --always --dirty`）
  - `container-id.txt` — container ID 与 mount 映射
  - `config.txt` — 注入的 `os_test.conf` 内容或其 checksum
  - `qemu-output.log` — 完整 QEMU 串口输出日志
  - `qemu-head-tail.txt` — 日志首尾各 30-50 行，证明测试实际运行至结束
  - 执行的完整命令与 exit status 记录
- **父级可读**：证据必须交付到调用方（父 agent）可以读取的持久路径，不能仅存在于子任务临时工作区或 `docker exec` 会话中。
- **新鲜性检查**：证据文件的时间戳必须晚于被测试代码或配置的最后修改时间，证明结果产自当前改动，而非老旧缓存。父 agent 必须进行这项检查。
- **不可保留时声明**：如果环境限制导致部分或全部证据不可保留（如容器销毁），报告中必须明确列出缺失哪些字段及原因。

**验收规则：**
- 父 agent 收到测试结果后，必须先验证证据完整性和新鲜性。
- 证据不满足要求的，不得验收为通过。
- 子任务交付的结果若仅引用 `/tmp` 下临时日志或 `testresult/` 下的临时输出，视为无效交付，必须重做。
- 任何声称"QEMU 测试通过"但没有对应元数据的结论，不具备有效性。

### F. 编排工作流（高爆炸半径内核回归调试）

高爆炸半径问题（如文件系统挂载、VFS 核心路径、内存子系统）的调试不能线性推进，必须多轨并行且每轮有明确验收门禁。

**触发条件：** 当问题同时满足以下特征时启用本工作流：
- 修改涉及核心子系统（VFS、MountFS、PageCache、mm）
- 单个 LTP suite 的失败可能由上游全局状态损坏引起
- 存在多个互斥的候选根因假设

#### ① 级联故障分解 — 最小有序 LTP 序列

不要直接跑全量 LTP。先分解：

1. 从用户给出的级联故障假设中提取"最少 LTP 用例集"，验证假设是否成立
2. 用例顺序必须反映依赖关系：清理/隔离用例在前，语义验证在后
3. 优先跑隔离性用例（如 mount namespace、bind mount 独立目录），确认全局状态是否隔离
4. 只有最小序列从 RED 转为 GREEN 后，才向相邻 suite 扩散

#### ② 双轨并行参考

对每个待验证的语义点，同时开两条轨道：

- **轨道 A — 本地代码分析**：读当前内核源码，确认实际行为
- **轨道 B — DragonOS / Linux 6.6 参考**：查阅 DragonOS 对应实现和 Linux 6.6 源码，确认参考语义

两条轨道的结论汇合后，才能形成"当前行为 vs 期望行为"的差异清单。单轨结论不可靠。

#### ③ 角色分工

| 角色 | 职责 | 交付物 |
|------|------|--------|
| explore | 扫描代码、阅读 LTP 用例、确认当前行为 | 代码路径分析、LTP 期望语义 |
| librarian | 查阅 DragonOS/Linux 参考实现 | 参考语义文档、差异清单 |
| implementation | 根据差异清单实现修复 | diff/patch |
| oracle | 审核差异清单完整性 + 验证修复证据 | 拒绝/通过意见 |

父 orchestrator 负责发起并行任务、收集产出、合成结论、裁决是否进入下一轮。

#### ④ RED → GREEN 门禁

1. **必须看到 RED 才能声称会 GREEN**：在修改前先确认当前代码在 LTP 最小序列上确实为 RED。如果最小序列就已经是 GREEN，说明级联假设不成立，应退回重新分解。
2. 修改后最小序列必须全 GREEN。
3. 扩散验证：最小序列 GREEN 后，再跑相邻 suite 确认没有引入回归。
4. 全量 LTP 是最终门禁——最小序列 GREEN 不保证全量 GREEN，但全量 RED 应优先回到①检查级联分解。

#### ⑤ Oracle 拒绝是正向设计信号

Oracle 拒绝修复方案不是失败，而是设计循环的必要反馈：

- 拒绝原因必须分类：证据不足 / 逻辑不完整 / 漏掉边界条件 / 副作用未评估
- 每轮拒绝后，必须更新差异清单再进入下一轮实现
- 连续两轮被同一原因拒绝，说明需要回到②补充参考轨道

#### ⑥ 范围边界：区分 P0 与后续任务

一轮编排只解决一个问题域。必须明确：

- **P0 边界**：当前轮只修复导致最小序列 RED 的全局状态损坏。清理阶段的全局污染必须优先消除。
- **后续任务**：最小序列通过后暴露的语义级缺陷（如错误码优先级、路径穿越检查）记为新任务，不在本轮处理。
- 不区分 P0 和后续任务会导致编排膨胀，失去焦点。

#### ⑦ 父 Orchestrator 的合成职责

子任务交付的原始结论不直接等于最终结论。父 orchestrator 必须：

- 合并多轨结论，交叉验证一致性
- 对子任务的"测试通过"声明执行证据检查（见 §E）
- 对 oracle 的拒绝意见执行分类和优先级排序
- 只有经过合成和裁决后的结论，才能进入 Work_Log 或作为修复依据
- 子任务声称的 PASS 如果缺少父级验证，不具备最终效力

## 约束

- Work_Log.md 追加在**顶部**（最新在前）
- Reference 文件按**现象分类**追加在底部
- 使用中文编写
- 每次代码修改后执行 **A → D** 全流程（更新 Work_Log → 沉淀经验 → 同步文档）
