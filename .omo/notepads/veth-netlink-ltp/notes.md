# veth-netlink-ltp 实施笔记

## 已修复
- modules.dep/builtin 文件创建 + "veth" 写入 modules.builtin
- modprobe stub (ln -sf /bin/true /bin/modprobe)
- 内核内嵌 initproc 需每次重编才能生效

## 待观察
1. tst_require_drivers: command not found (仅 tcpdump01 测试)
   - PATH 已含 testcases/lib，大部分脚本能正常 source
   - tcpdump01 特殊：它在 tst_net.sh 内调用 tst_require_drivers 但函数未定义
   - 可能原因：tcpdump01 的 test 脚本走了不同的 source 路径
2. veth driver not available → 已修复 modules.builtin 内容，等用户测试验证
