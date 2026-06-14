---
title: "<Module Title>"
module: "<kernel module name>"
category: <fs|mm|net|syscall|process|driver|overview|testing>
status: draft  # draft | stable | deprecated
owner: ""
last_updated: "YYYY-MM-DD"
code_paths:
  - "os/src/<path>"
entry_points:
  - "<type/function/syscall>"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "<ltp-testcase>"
  oscomp:
    - "<oscomp-group>"
related_docs:
  - "docs/<path>.md"
---

## Overview

<One sentence describing the module purpose. Example: "内存管理子系统负责物理页帧分配、SV39 页表管理、VMA 跟踪和缺页处理。">

## Design Goals

- **POSIX 兼容性**: <哪些 POSIX 接口需要实现，语义对标 Linux 哪个版本>
- **LTP 兼容性**: <哪些 LTP 测试用例需要通过>
- **性能**: <性能约束，如分配延迟、吞吐量、内存开销等>

## Architecture

<ASCII 架构图占位>

```
+---------------------------+
|  <Module Name>            |
|  +----------------------+ |
|  | <Sub-module A>       | |
|  +----------------------+ |
|  +----------------------+ |
|  | <Sub-module B>       | |
|  +----------------------+ |
+---------------------------+
```

### Sub-modules

| 子模块 | 路径 | 职责 |
|--------|------|------|
| <A> | `os/src/<path>` | <职责描述> |
| <B> | `os/src/<path>` | <职责描述> |
| <C> | `os/src/<path>` | <职责描述> |

## Key Data Structures

| 结构/类型 | 定义位置 | 用途 | 关键字段 |
|-----------|----------|------|----------|
| `<StructName>` | `os/src/<path>` | <用途> | `<field>: <type>` |
| `<EnumName>` | `os/src/<path>` | <用途> | `<variant>` |
| `<TraitName>` | `os/src/<path>` | <用途> | `<method>` |

## Execution Flow

### Flow 1: <流程名称>

流程描述: <什么操作触发，经过哪些模块>

```text
<entry point>
  -> <module A>.<function>()
    -> <module B>.<function>()
      -> <result or next step>
  -> <return path>
```

### Flow 2: <流程名称>

```text
<entry point>
  -> <module C>.<function>()
    -> <module D>.<function>()
  -> <return path>
```

### 关键路径说明

- **路径 1**: <说明>
- **路径 2**: <说明>
- **错误处理**: <说明>

## Interfaces / APIs

### Syscall APIs

| Syscall ID | 函数签名 | 描述 | 返回值 |
|-----------|----------|------|--------|
| `<SYSCALL_XXX>` | `pub fn sys_xxx(args) -> isize` | <描述> | `<0 错误，>=0 成功>` |

### Kernel Internal APIs

| 函数 | 可见性 | 描述 |
|------|--------|------|
| `<fn_name>` | `pub(crate)` | <描述> |
| `<fn_name>` | `pub` | <描述> |

### Trait Definitions

```rust
/// <trait 说明>
pub trait <TraitName> {
    /// <方法说明>
    fn <method>(&self, <args>) -> Result<type, Error>;
}
```

## Test Mapping

| 特性 | Syscall / API | LTP 用例 | OSCOMP 分组 | 状态 |
|------|--------------|----------|-------------|------|
| <特性> | `sys_xxx` | `<testcase>` | `<group>` | pass / fail / partial |
| <特性> | `sys_xxx` | `<testcase>` | `<group>` | pass / fail / partial |

### LTP 跳过清单

| 用例 | 跳过原因 | 跟踪 Issue |
|------|----------|------------|
| `<testcase>` | <原因> | <issue 链接> |

## Known Issues

1. **<问题标题>**
   - 现象: <描述>
   - 根因: <分析>
   - 影响: <影响范围>
   - 修复方向: <建议>

2. **<问题标题>**
   - 现象: <描述>
   - 根因: <分析>
   - 影响: <影响范围>
   - 修复方向: <建议>

## References

- <外部文档链接或描述>
- <DragonOS / Linux 对应实现: path>
- <相关内核标准: POSIX.1-2008 / SUSv4>
