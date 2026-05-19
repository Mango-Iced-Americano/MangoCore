SUDO=$(if [ $(whoami) = "root" ]; then echo -n ""; else echo -n "sudo"; fi)
U_FS_DIR="../fs-img-dir"
U_FS="$1"
BLK_SZ="4096"
TARGET=riscv64gc-unknown-none-elf
MODE="release"
IMG_SZ="56"
if [ $# -ge 2 ]; then
    if [ "$2" = "laqemu" ]; then
        TARGET=loongarch64-unknown-linux-gnu
        BLK_SZ="4096"
        IMG_SZ="150"
    elif [ "$2" = "2k1000" ]; then
        TARGET=loongarch64-unknown-linux-gnu
        BLK_SZ="2048"
        IMG_SZ="150"
    else
        TARGET=riscv64gc-unknown-none-elf
        BLK_SZ="4096"
        IMG_SZ="512"
    fi
fi

if [ $# -ge 3 ]; then
    MODE="$3"
fi

ARCH=$(echo "${TARGET}" | cut -d- -f1 | grep -o '[a-zA-Z]\+[0-9]\+')
echo
echo Current arch: ${ARCH}
echo

mkdir -p ${U_FS_DIR}
touch ${U_FS}
dd if=/dev/zero of=${U_FS} bs=1M count=${IMG_SZ}

if [ "$4" = "fat32" ]; then
    echo Making fat32 imgage with BLK_SZ=${BLK_SZ}
    mkfs.vfat -F 32 ${U_FS} -S ${BLK_SZ}
    fdisk -l ${U_FS}
fi

if [ "$4" = "ext4" ]; then
    echo Making ext4 imgage with BLK_SZ=${BLK_SZ}
    mkfs.ext4 ${U_FS} -b ${BLK_SZ}
    fdisk -l ${U_FS}
fi

if test -e ${U_FS_DIR}/fs; then
    rm -r ${U_FS_DIR}/fs
fi

mkdir -p ${U_FS_DIR}/fs

# Some container runtimes expose /dev/loop-control but do not pre-create /dev/loopN.
# Create a small set of loop device nodes on demand so mount can attach images.
if [ -c /dev/loop-control ]; then
    i=0
    while [ $i -le 7 ]; do
        if [ ! -b /dev/loop${i} ]; then
            mknod -m 660 /dev/loop${i} b 7 ${i} 2>/dev/null || true
        fi
        i=$((i + 1))
    done
fi

# Mount loop image and fail fast if the environment does not allow mounting.
if ! mount ${U_FS} ${U_FS_DIR}/fs; then
    echo "ERROR: failed to mount ${U_FS} on ${U_FS_DIR}/fs"
    echo "HINT: run in a privileged environment (or with CAP_SYS_ADMIN) to build rootfs images."
    exit 1
fi

# 创建根文件系统
mkdir -p ${U_FS_DIR}/fs/lib
mkdir -p ${U_FS_DIR}/fs/etc
mkdir -p ${U_FS_DIR}/fs/bin
mkdir -p ${U_FS_DIR}/fs/root
printf 'root:x:0:0:root:/root:/bin/bash\n' > ${U_FS_DIR}/fs/etc/passwd
touch ${U_FS_DIR}/fs/root/.bash_history

# 只能copy一个文件夹下所有内容，无法copy单文件
try_copy() {
    if [ -d $1 ]; then
        echo copying $1 ';'
        for programname in $(ls -A $1); do
            cp -fr "$1"/"$programname" $2
        done
    else
        echo "$1" "doesn""'""t exist, skipped."
    fi
}

for programname in $(ls ../user/src/bin); do
    cp -r ../user/target/${TARGET}/${MODE}/${programname%.rs} ${U_FS_DIR}/fs/${programname%.rs}
done

if [ ! -f ${U_FS_DIR}/fs/syscall ]; then
    mkdir -p ${U_FS_DIR}/fs/syscall
fi

if [ "$2" = "laqemu" ]; then
    cp -r ../user/LaTest/* ${U_FS_DIR}/fs/
    cp -r ../user/fs/* ${U_FS_DIR}/fs/
    cp ./bash-la ${U_FS_DIR}/fs/bin/bash
    cp ./busybox-la ${U_FS_DIR}/fs/bin/busybox
    cp ../user/target/loongarch64-unknown-linux-gnu/release/initproc ${U_FS_DIR}/fs/
fi

if [ "$2" = "rvqemu" ]; then
    cp ./bash-rv ${U_FS_DIR}/fs/bin/bash
    cp ./busybox-rv ${U_FS_DIR}/fs/bin/busybox
    cp ../user/target/riscv64gc-unknown-none-elf/release/initproc ${U_FS_DIR}/fs/
    cp ../1.txt ${U_FS_DIR}/fs/ 2>/dev/null || true
fi

umount ${U_FS_DIR}/fs
echo "DONE"
exit 0
