# Machine-readable image/drive role manifest (Make v1).
#
# x0 is always the bootstrap/root consumer: a project-built development rootfs
# or an immutable external competition sdcard.  x1 is always project-owned and
# contains the tools payload in P1 plus the FAT32 LTP scratch area in P2.
# Regression and ktest intentionally attach no disks.
IMAGE_ROLE_MANIFEST_VERSION := 1
IMAGE_ROLE_DRIVE_ORDER := x0 x1
IMAGE_ROLE_OFFICIAL_X0_MUTABLE := no

IMAGE_ROLE_RV64_PRODUCT_ROOT ?= $(BUILD_ROOT)/rv64/$(MODE)/normal
IMAGE_ROLE_LA64_PRODUCT_ROOT ?= $(BUILD_ROOT)/la64/$(MODE)/normal

IMAGE_ROLE_RV64_BOOTSTRAP_ROOT = $(IMAGE_ROLE_RV64_PRODUCT_ROOT)/initramfs/initramfs-rv.cpio
IMAGE_ROLE_LA64_BOOTSTRAP_ROOT = $(IMAGE_ROLE_LA64_PRODUCT_ROOT)/initramfs/initramfs-la.cpio
IMAGE_ROLE_RV64_DEVELOPMENT_X0 = $(IMAGE_ROLE_RV64_PRODUCT_ROOT)/image/rootfs-rv.img
IMAGE_ROLE_LA64_DEVELOPMENT_X0 = $(IMAGE_ROLE_LA64_PRODUCT_ROOT)/image/rootfs-la.img
IMAGE_ROLE_RV64_COMPETITION_X0 := ../sdcard-rv.img
IMAGE_ROLE_LA64_COMPETITION_X0 := ../sdcard-la.img
IMAGE_ROLE_RV64_X1 = $(IMAGE_ROLE_RV64_PRODUCT_ROOT)/tools/disk.img
IMAGE_ROLE_LA64_X1 = $(IMAGE_ROLE_LA64_PRODUCT_ROOT)/tools/disk-la.img

IMAGE_ROLE_X1_PARTITION1 := tools-ext4
IMAGE_ROLE_X1_PARTITION2 := scratch-fat32
IMAGE_ROLE_X1_SCRATCH_DEVICE := /dev/vdb2
IMAGE_ROLE_COMPETITION_PROVENANCE := oscomp/testsuits-for-oskernel pre-20250615
IMAGE_ROLE_COMPETITION_CHECKSUM_POLICY := sha256-record-derived-input
