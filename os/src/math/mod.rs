//! 内核可复用数学辅助函数。
//!
//! 该模块只放与架构、内存分配和 syscall 语义无关的小型纯函数。

/// 判断 `num` 是否为 `base` 的非负整数次幂。
///
/// # Semantics
///
/// `1` 被视为任意合法底数的 0 次幂；`base <= 1` 时除 `num == 1`
/// 外均返回 `false`。
pub fn is_power_of(num: u64, base: u64) -> bool {
    if num == 1 {
        return true;
    }
    if base <= 1 || num < base {
        return false;
    }
    if num % base != 0 {
        return false;
    }
    is_power_of(num / base, base)
}
