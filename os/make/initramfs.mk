# Initramfs image targets.
initramfs-rv: user
	@DNS_SERVER=$(DNS_SERVER) ../scripts/build_initramfs.sh rv64 $(MODE) $(INITRAMFS_DIR_RV)

initramfs-la: user
	@DNS_SERVER=$(DNS_SERVER) ../scripts/build_initramfs.sh la64 $(MODE) $(INITRAMFS_DIR_LA)

initramfs-all: initramfs-rv initramfs-la
