# Shared QEMU argument construction. Every launch profile supplies only its
# loader/kernel choice and x0 role; drives and base arguments are centralized.
QEMU_BASE_ARGS = -machine virt -nographic

define qemu_two_drives
-drive if=none,file=$(1),format=raw,id=x0 $(BLK_DEV_x0) -drive if=none,file=$(IMAGE_ROLE_$(2)_X1),format=raw,id=x1 $(BLK_DEV_x1)
endef

define qemu_zero_drives

endef
