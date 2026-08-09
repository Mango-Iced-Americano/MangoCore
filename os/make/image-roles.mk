# Machine-readable image/drive role manifest (Make/Python v2).
#
# x0 is always the bootstrap/root consumer: a project-built development rootfs
# or an immutable external competition sdcard.  x1 is always project-owned and
# contains the tools payload in P1 plus the FAT32 LTP scratch area in P2.
# Regression attaches no disks. KTest attaches a regenerated, clean ext4 x0
# fixture so filesystem contract tests do not depend on mutable rootfs images.
IMAGE_ROLE_MANIFEST_VERSION := 2
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

# BuildStorm uses the official public userspace image, not the 4 GiB
# competition image above.  Keep one decompressed golden copy and run QEMU
# through a disposable qcow2 overlay so repeated runs neither re-expand the
# archive nor mutate the supplied official image.
IMAGE_ROLE_RV64_BUILDSTORM_GOLDEN_X0 ?= $(BUILD_ROOT)/buildstorm-input/sdcard-rv-pub.img
IMAGE_ROLE_LA64_BUILDSTORM_GOLDEN_X0 ?= $(BUILD_ROOT)/buildstorm-input/sdcard-la-pub.img
IMAGE_ROLE_RV64_BUILDSTORM_X0 ?= $(BUILD_ROOT)/buildstorm-input/sdcard-rv-pub-run.qcow2
IMAGE_ROLE_LA64_BUILDSTORM_X0 ?= $(BUILD_ROOT)/buildstorm-input/sdcard-la-pub-run.qcow2
IMAGE_ROLE_RV64_BUILDSTORM_ARCHIVE ?= $(or $(wildcard ../sdcard-rv-pub.img.gz),$(wildcard ../sdcard-rv-pub.img(1).gz))
IMAGE_ROLE_LA64_BUILDSTORM_ARCHIVE ?= $(or $(wildcard ../sdcard-la-pub.img.gz),$(wildcard ../sdcard-la-pub.img(1).gz))

# A command-line development-x0 override is parsed before recipes execute, so
# reject an official x0 alias even for `make -n`.  The Python checker resolves
# symlinks and compares existing files by device/inode as well as pathname.
define reject_official_development_x0_override
$(if $(filter command line environment environment override,$(origin $(1))),$(if $(shell python3 ../scripts/image_roles.py validate-mutable --repo-root .. --arch $(2) --path "$($(1))" >/dev/null 2>&1 || printf rejected),$(error $(1) resolves to an immutable official x0),),)
endef
$(call reject_official_development_x0_override,IMAGE_ROLE_RV64_DEVELOPMENT_X0,rv64)
$(call reject_official_development_x0_override,IMAGE_ROLE_LA64_DEVELOPMENT_X0,la64)

IMAGE_ROLE_RV64_DERIVED_X0 := ../build/development/rv64/sdcard-rv-derived.img
IMAGE_ROLE_LA64_DERIVED_X0 := ../build/development/la64/sdcard-la-derived.img
IMAGE_ROLE_RV64_DERIVED_X0_NEXT := ../build/development/rv64/sdcard-rv-derived.next.img
IMAGE_ROLE_LA64_DERIVED_X0_NEXT := ../build/development/la64/sdcard-la-derived.next.img
IMAGE_ROLE_RV64_COMPETITION_X0_ARCHIVE := ../fs-img-dir/sdcard-rv.img.xz
IMAGE_ROLE_LA64_COMPETITION_X0_ARCHIVE := ../fs-img-dir/sdcard-la.img.xz
IMAGE_ROLE_RV64_COMPETITION_X0_CHECKSUM := ../sdcard-rv.img.sha256
IMAGE_ROLE_LA64_COMPETITION_X0_CHECKSUM := ../sdcard-la.img.sha256
IMAGE_ROLE_RV64_COMPETITION_X0_ARCHIVE_CHECKSUM := ../fs-img-dir/sdcard-rv.img.xz.sha256
IMAGE_ROLE_LA64_COMPETITION_X0_ARCHIVE_CHECKSUM := ../fs-img-dir/sdcard-la.img.xz.sha256
IMAGE_ROLE_RV64_X1 = $(IMAGE_ROLE_RV64_PRODUCT_ROOT)/tools/disk.img
IMAGE_ROLE_LA64_X1 = $(IMAGE_ROLE_LA64_PRODUCT_ROOT)/tools/disk-la.img

IMAGE_ROLE_X1_PARTITION1 := tools-ext4
IMAGE_ROLE_X1_PARTITION2 := scratch-fat32
IMAGE_ROLE_X1_SCRATCH_DEVICE := /dev/vdb2
IMAGE_ROLE_COMPETITION_PROVENANCE := oscomp/testsuits-for-oskernel pre-20250615
IMAGE_ROLE_COMPETITION_CHECKSUM_POLICY := sha256-record-derived-input
