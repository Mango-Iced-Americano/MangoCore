#!/bin/bash

# 源目录和目标目录
SRC_DIR="/mnt/sdcard/glibc"
DST_DIR="/home/stl/osnational/oskernel2025-npucore-blossom/user/LaTest/glibc"

# 确保目标目录存在
mkdir -p "$DST_DIR"

# 要复制的文件列表
FILES=(
    basic_testcode.sh busybox busybox_cmd.txt busybox_testcode.sh
    bw_file_rd bw_mem bw_mmap_rd bw_pipe bw_tcp bw_unix
    date.lua entry-dynamic.exe entry-static.exe file_io.lua hello
    lat_cmd lat_connect lat_ctx lat_dram_page lat_fcntl lat_fifo lat_fs lat_http
    lat_mem_rd lat_mmap lat_ops lat_pagefault lat_pipe lat_pmake lat_proc lat_rand
    lat_rpc lat_select lat_sem lat_sig lat_syscall lat_tcp lat_udp lat_unix
    lat_unix_connect lat_usleep libctest_testcode.sh lmbench lmbench_all
    lmbench_testcode.sh lmdd lua lua_testcode.sh max_min.lua random.lua remove.lua
    round_num.lua run-dynamic.sh run-static.sh runtest.exe sin30.lua sort.lua
    strings.lua test.sh
)

# 复制操作
for file in "${FILES[@]}"; do
    if [ -f "$SRC_DIR/$file" ]; then
        cp "$SRC_DIR/$file" "$DST_DIR/"
        echo "Copied: $file"
    else
        echo "Warning: $file not found in $SRC_DIR"
    fi
done
