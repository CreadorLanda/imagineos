# ==============================================================================
# ImagineOS - variáveis compartilhadas entre os makefiles de build
# ==============================================================================
#
# Cada makefile em build/ inclui este arquivo. O Makefile raiz também o inclui,
# então os nomes de diretórios e ferramentas existem em um único lugar e os
# sub-makefiles podem ser executados tanto pela raiz quanto isoladamente:
#
#   make -f build/kernel.mk          # compila kernel + dzImage
#   make -f build/ramfs.mk           # empacota o RAMFS
#   make -f build/userland.mk        # compila apps e utilitários
#   make -f build/iso.mk             # gera a ISO
#
# A raiz orquestra chamando estes mesmos arquivos com $(MAKE) -f.

SHELL := /bin/sh

ROOT := $(patsubst %/,%,$(dir $(lastword $(MAKEFILE_LIST))))/..
ROOT := $(abspath $(ROOT))

# ==============================================================================
# IDENTIDADE E ALVO
# ==============================================================================

K_NAME         := dreamcore
K_VERSION      := 0.1.0
CODENAME       := astrid
DISTRIBUTION   := imagineos

TARGET         := x86_64-unknown-none
KERNEL_FEATURES ?=

# ==============================================================================
# FERRAMENTAS
# ==============================================================================

QEMU           := qemu-system-x86_64
OVMF_CODE      ?= OVMF.fd
CARGO          := rustup run stable cargo
RUSTC          := rustup run stable rustc
CC             ?= cc
AR             ?= ar

# ==============================================================================
# DIRETÓRIOS E ARTEFATOS DE SAÍDA
# ==============================================================================

BUILD_DIR      := $(ROOT)/.build
DISTRO_DIR     := $(BUILD_DIR)/distro

KERNEL         := $(ROOT)/target/$(TARGET)/release/dreamcore
BOOTSTRAP      := $(BUILD_DIR)/bootstrap.elf
DZ_IMAGE       := $(BUILD_DIR)/dzImage
RAMFS_IMAGE    := $(BUILD_DIR)/ramfs.tar
RAMFS_INST_IMG := $(BUILD_DIR)/ramfs-installed.tar
ISO_DIR        := $(BUILD_DIR)/iso
ISO_IMAGE      := $(DISTRO_DIR)/$(K_NAME)-$(shell date +%Y-%m-%d-%H-%M)-$(CODENAME).iso
DISK_IMAGE     ?= $(BUILD_DIR)/imagineos-disk.img
INSTALL_TARGET := $(BUILD_DIR)/installer-target.img

# Ferramentas auxiliares compiladas no host
DZPACK_BIN     := $(BUILD_DIR)/dzpack
DFS_IMAGE_BIN  := $(BUILD_DIR)/dfs-image
TCC_HOST       := $(BUILD_DIR)/toolchain/tcc

# Fontes de terceiros e recursos
LIMINE_DIR      := $(ROOT)/third_party/limine
TCC_SOURCE      := $(ROOT)/third_party/tinycc
TCC_CONFIG_STAMP := $(BUILD_DIR)/toolchain/tinycc-config.stamp
TCC_PATCH_STAMP := $(BUILD_DIR)/toolchain/tinycc-imagineos-patched.stamp
TCC_SOURCE_REV  := 43c7708b85681a2fd4451c8a541af4494a8919b2
FONT_ASSET      := $(ROOT)/tools/assets/zap-vga16.psf

# ==============================================================================
# RAMFS E PROGRAMAS DE USUÁRIO
# ==============================================================================

RAMFS_ROOT     := $(ROOT)/ramfs
RAMFS_DIRS     := bin dev etc sbin home system/fonts system/install tmp usr
USER_UTILITIES := cat dmesg distroinstall fdtest globalconf grep kill ls mkdir ps rm shutdown su sudo touch uname vi hello
USER_C_UTILITIES := hello_c tcc

# Manifestos e libs do userland
USER_API_MANIFEST       := $(ROOT)/userland/api/Cargo.toml
USER_API_TARGET_DIR     := $(BUILD_DIR)/user/api-target
USER_API_RLIB           := $(USER_API_TARGET_DIR)/$(TARGET)/release/libimagineos.rlib
USER_API_DEPS           := $(USER_API_TARGET_DIR)/$(TARGET)/release/deps

USER_RUNTIME_MANIFEST   := $(ROOT)/userland/runtimes/rust/Cargo.toml
USER_RUNTIME_TARGET_DIR := $(BUILD_DIR)/user/runtime-target
USER_RUNTIME_RLIB       := $(USER_RUNTIME_TARGET_DIR)/$(TARGET)/release/libimagineos_rt.rlib
USER_RUNTIME_DEPS       := $(USER_RUNTIME_TARGET_DIR)/$(TARGET)/release/deps

# Runtime de programas em C, implementada em Rust (userland/runtimes/c).
# Produz um único objeto com _start, c_runtime_init, setjmp/longjmp e o
# restaurador de sinais, substituindo os antigos crt0.S/crt0.c/signal.S/setjmp.S.
USER_CRT_MANIFEST       := $(ROOT)/userland/runtimes/c/Cargo.toml
USER_CRT_TARGET_DIR     := $(BUILD_DIR)/user/crt-target
USER_CRT_RLIB           := $(BUILD_DIR)/user/libimagineos_crt.a

USER_PROGRAMS           := $(BUILD_DIR)/user/sbin/init \
                           $(BUILD_DIR)/user/sbin/getty \
                           $(BUILD_DIR)/user/bin/shell \
                           $(addprefix $(BUILD_DIR)/user/utilities/,$(USER_UTILITIES) $(USER_C_UTILITIES))

USER_CFLAGS             := -ffreestanding -fno-stack-protector -fno-pic -fno-pie \
                           -fno-builtin -mno-red-zone -nostdinc -I$(ROOT)/userland/libc/include \
                           -isystem $(shell $(CC) -print-file-name=include)
USER_CRT_OBJECTS        := $(BUILD_DIR)/user/crt.o \
                           $(BUILD_DIR)/user/unistd.o \
                           $(BUILD_DIR)/user/path.o $(BUILD_DIR)/user/string.o $(BUILD_DIR)/user/stdio.o \
                           $(BUILD_DIR)/user/stdlib.o $(BUILD_DIR)/user/time.o \
                           $(BUILD_DIR)/user/math.o $(BUILD_DIR)/user/mman.o \
                           $(BUILD_DIR)/user/signals.o
USER_LIBC_OBJECTS       := $(filter-out $(BUILD_DIR)/user/crt.o,$(USER_CRT_OBJECTS))

TCC_NATIVE_SOURCES      := tcc.c libtcc.c tccpp.c tccgen.c tccdbg.c \
                           tccelf.c tccasm.c tccrun.c x86_64-gen.c x86_64-link.c \
                           i386-asm.c
TCC_NATIVE_OBJECTS      := $(patsubst %.c,$(BUILD_DIR)/user/tcc/%.o,$(TCC_NATIVE_SOURCES))
TCC_NATIVE_CFLAGS       := $(USER_CFLAGS) -I$(TCC_SOURCE) -I$(TCC_SOURCE)/include \
                           -DTCC_TARGET_X86_64 -DCONFIG_TCC_STATIC -DCONFIG_TCC_SEMLOCK=0 \
                           -DONE_SOURCE=0 \
                           -DCONFIG_TCC_SYSINCLUDEPATHS=\"/usr/include\" \
                           -DCONFIG_TCC_LIBPATHS=\"/usr/lib:/usr/lib/tcc\" \
                           -DCONFIG_TCC_CRTPREFIX=\"/usr/lib\"
