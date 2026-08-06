# Shared QEMU argument construction. Architecture makefiles provide the
# architecture-specific fragments; this file owns every profile selection.
QEMU_BASE_ARGS = -machine virt -nographic
QEMU_MTTCG_ARGS = -accel tcg,thread=multi
QEMU_SMP_ARGS = -smp cpus=$(CORE_NUM),sockets=1,cores=$(CORE_NUM),threads=1
QEMU_MEMORY ?= 1G
BUILDSTORM_CMDLINE ?= mango.mode=normal profile=buildstorm

define qemu_two_drives
-drive if=none,file=$(1),format=raw,id=x0 $(BLK_DEV_x0) -drive if=none,file=$(IMAGE_ROLE_$(2)_X1),format=raw,id=x1 $(BLK_DEV_x1)
endef

define qemu_zero_drives

endef

define qemu_competition_command
$(QEMU_EXECUTABLE) $(QEMU_BASE_ARGS) $(QEMU_COMPETITION_BEFORE_DRIVES) $(call qemu_two_drives,$(1),$(QEMU_ROLE_ARCH)) $(QEMU_COMPETITION_AFTER_DRIVES)
endef

define qemu_competition_gdb_command
$(QEMU_EXECUTABLE) $(QEMU_BASE_ARGS) $(QEMU_COMPETITION_GDB_BEFORE_DRIVES) $(call qemu_two_drives,$(QEMU_COMPETITION_X0),$(QEMU_ROLE_ARCH)) $(QEMU_COMPETITION_GDB_AFTER_DRIVES)
endef

define qemu_development_command
$(QEMU_EXECUTABLE) $(QEMU_BASE_ARGS) $(QEMU_DEVELOPMENT_BEFORE_DRIVES) $(call qemu_two_drives,$(QEMU_DEVELOPMENT_X0),$(QEMU_ROLE_ARCH)) $(QEMU_DEVELOPMENT_AFTER_DRIVES)
endef

define qemu_buildstorm_command
$(QEMU_EXECUTABLE) $(QEMU_BASE_ARGS) $(QEMU_BUILDSTORM_BEFORE_DRIVES) $(call qemu_two_drives,$(QEMU_COMPETITION_X0),$(QEMU_ROLE_ARCH)) $(QEMU_BUILDSTORM_AFTER_DRIVES)
endef

define qemu_zero_drive_command
$(QEMU_EXECUTABLE) $(QEMU_BASE_ARGS) $(1) $(call qemu_zero_drives) $(2)
endef

define qemu_profile_command
$(strip $(if $(filter normal derived-competition,$(1)),$(call qemu_competition_command,$(QEMU_DERIVED_X0)),$(if $(filter competition,$(1)),$(call qemu_competition_command,$(QEMU_COMPETITION_X0)),$(if $(filter buildstorm,$(1)),$(call qemu_buildstorm_command),$(if $(filter development,$(1)),$(call qemu_development_command),$(if $(filter debug,$(1)),$(call qemu_development_command) -S -s,$(if $(filter regression,$(1)),$(call qemu_zero_drive_command,$(QEMU_REGRESSION_BEFORE_DRIVES),$(QEMU_REGRESSION_AFTER_DRIVES)),$(if $(filter ktest,$(1)),$(call qemu_zero_drive_command,$(QEMU_KTEST_BEFORE_DRIVES),$(QEMU_KTEST_AFTER_DRIVES)),$(error unsupported QEMU profile: $(1))))))))))
endef

qemu-profile-dry-run:
	@printf '%s\n' "$(call qemu_profile_command,$(QEMU_PROFILE))"

.PHONY: qemu-profile-dry-run
