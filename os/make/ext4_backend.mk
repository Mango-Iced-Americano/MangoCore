EXT4_BACKEND ?= lwext4

ifeq ($(EXT4_BACKEND),lwext4)
EXT4_BACKEND_FEATURE := ext4_lwext4_backend
else ifeq ($(EXT4_BACKEND),legacy)
EXT4_BACKEND_FEATURE := ext4_legacy_backend
else ifeq ($(EXT4_BACKEND),another)
EXT4_BACKEND_FEATURE := ext4_another_backend
else
$(error unsupported EXT4_BACKEND '$(EXT4_BACKEND)'; expected lwext4, legacy, or another)
endif

KERNEL_BASE_FEATURES := initramfs preload_payloads $(EXT4_BACKEND_FEATURE)
