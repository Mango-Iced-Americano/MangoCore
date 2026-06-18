# Fix RamFS LoadPageFault on ELF Loading

## Root Cause
`vfs::File::map_to_kernel_space()` relies on `inode.page_cache()` to get physical frames for kernel-space mapping. RamFS stores data in `Vec<u8>` and does NOT implement `page_cache()` → returns `None` → empty frame vector → `insert_program_area` creates no page table entries → accessing the mapped region causes `LoadPageFault @ 0x6000_0000`.

## Fix Plan

### Step 1: Fix `map_to_kernel_space` (os/src/fs/vfs/file.rs)
When `page_cache()` returns `None`, allocate frames manually, read file data via `pread()`, copy into frames, then proceed with mapping.

### Step 2: Compile rv64 kernel
`make rv64-kernel-build-only` in container

### Step 3: Run QEMU verification
Launch QEMU with virtio-net (needed for net device init), check kernel boots past ELF loading.

### Step 4: (Optional) Add page_cache support to ramfs
If time permits, implement `page_cache()` for ramfs as the user suggested — proper cache layer that reads from inode Vec<u8> on miss.
