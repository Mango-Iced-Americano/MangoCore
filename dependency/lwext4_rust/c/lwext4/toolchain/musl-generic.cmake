if(NOT DEFINED ENV{ARCH})
    set(ARCH "x86_64")
else()
    set(ARCH $ENV{ARCH})
endif()

# Name of the target
set(CMAKE_SYSTEM_NAME "Linux")
set(CMAKE_SYSTEM_PROCESSOR ${ARCH})

# Toolchain settings — use linux-gnu (not musl) cross-compiler
set(TOOLCHAIN_PREFIX ${ARCH}-linux-gnu)

set(CMAKE_C_COMPILER    ${TOOLCHAIN_PREFIX}-gcc)
set(CMAKE_CXX_COMPILER  ${TOOLCHAIN_PREFIX}-g++)
set(AS                  ${TOOLCHAIN_PREFIX}-as)
set(AR                  ${TOOLCHAIN_PREFIX}-ar)
set(OBJCOPY             ${TOOLCHAIN_PREFIX}-objcopy)
set(OBJDUMP             ${TOOLCHAIN_PREFIX}-objdump)
set(SIZE                ${TOOLCHAIN_PREFIX}-size)

set(LD_FLAGS "-nostdlib -static --gc-sections -nostartfiles")

set(CMAKE_C_FLAGS   "-std=gnu99 -fdata-sections -ffunction-sections -U_FORTIFY_SOURCE"         CACHE INTERNAL "c compiler flags")
set(CMAKE_CXX_FLAGS "-fdata-sections -ffunction-sections"                    CACHE INTERNAL "cxx compiler flags")
set(CMAKE_ASM_FLAGS ""                                                       CACHE INTERNAL "asm compiler flags")

# Bare-metal freestanding: no libc, no builtins, no startup files
set(CMAKE_C_FLAGS   "-fPIC -fno-builtin -ffreestanding -nostdlib ${CMAKE_C_FLAGS}"   CACHE INTERNAL "c freestanding flags")
set(CMAKE_CXX_FLAGS "-fPIC -fno-builtin -ffreestanding -nostdlib ${CMAKE_CXX_FLAGS}" CACHE INTERNAL "cxx freestanding flags")

if (APPLE)
    set(CMAKE_EXE_LINKER_FLAGS "-dead_strip"          CACHE INTERNAL "exe link flags")
else (APPLE)
    set(CMAKE_EXE_LINKER_FLAGS "-Wl,--gc-sections"    CACHE INTERNAL "exe link flags")
endif (APPLE)

SET(CMAKE_C_FLAGS_DEBUG   "-O0 -g -ggdb3"  CACHE INTERNAL "c debug compiler flags")
SET(CMAKE_CXX_FLAGS_DEBUG "-O0 -g -ggdb3"  CACHE INTERNAL "cxx debug compiler flags")
SET(CMAKE_ASM_FLAGS_DEBUG "-g -ggdb3"      CACHE INTERNAL "asm debug compiler flags")

SET(CMAKE_C_FLAGS_RELEASE   "-O2 -g -ggdb3"  CACHE INTERNAL "c release compiler flags")
SET(CMAKE_CXX_FLAGS_RELEASE "-O2 -g -ggdb3"  CACHE INTERNAL "cxx release compiler flags")
SET(CMAKE_ASM_FLAGS_RELEASE ""               CACHE INTERNAL "asm release compiler flags")
