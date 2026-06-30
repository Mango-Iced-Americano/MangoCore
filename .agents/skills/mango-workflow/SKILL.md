---
name: mango-workflow
description: 自动维护 oskernel2026-mango 项目的工作日志、可复用经验模式，以及性能调试知识库。每次代码修改后触发：更新 Doc/Work_Log.md；调试/性能任务前加载 references/ 作为前置参考；发现可复用模式时沉淀到 references/。
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

在 `Doc/Work_Log.md` 顶部追加日期戳条目（如果当天已有条目则追加到该条目下）：

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

如果本次修改揭示了**可能跨对话复用**的经验，追加到对应 reference 文件：

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

## 约束

- Work_Log.md 追加在**顶部**（最新在前）
- Reference 文件按**现象分类**追加在底部
- 使用中文编写
- 每次代码修改后执行 **A → D** 全流程（更新 Work_Log → 沉淀经验 → 同步文档）
