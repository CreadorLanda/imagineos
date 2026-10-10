# ==============================================================================
# ImagineOS - Makefile raiz (orquestrador)
# ==============================================================================
#
# Este arquivo só orquestra. A lógica de cada etapa vive em build/:
#
#   build/common.mk    variáveis, ferramentas e artefatos compartilhados
#   build/kernel.mk    kernel dreamcore, bootstrap.elf e dzImage
#   build/userland.mk  apps/utilitários Rust, libc, runtime C e TinyCC
#   build/ramfs.mk     árvore ramfs/ e empacotamento do ramfs.tar
#   build/iso.mk       ISO UEFI e execução no QEMU
#
# Cada etapa também roda isolada:
#
#   make -f build/kernel.mk dzimage
#   make -f build/userland.mk user-programs
#   make -f build/ramfs.mk ramfs-image
#   make -f build/iso.mk iso
#
# A raiz apenas delega cada alvo ao makefile correspondente com $(MAKE) -f, de
# modo que as dependências continuam sendo resolvidas pelo make (sem duplicar
# regras) e o pipeline segue incremental.

include build/common.mk

KERNEL_MK   := $(ROOT)/build/kernel.mk
USERLAND_MK := $(ROOT)/build/userland.mk
RAMFS_MK    := $(ROOT)/build/ramfs.mk
ISO_MK      := $(ROOT)/build/iso.mk

.PHONY: all kernel bootstrap dzimage tools ramfs ramfs-image \
        user-programs user-libs c-app tcc-source \
        iso run disk-image run-disk installer-disk run-installer clean

# Padrão: mantemos `kernel` como alvo padrão para não surpreender quem só quer
# o ELF do kernel.
all: kernel

kernel bootstrap dzimage tools:
	$(MAKE) -f $(KERNEL_MK) $@

ramfs ramfs-image:
	$(MAKE) -f $(RAMFS_MK) $@

user-programs user-libs c-app tcc-source:
	$(MAKE) -f $(USERLAND_MK) $@

iso run disk-image run-disk installer-disk run-installer:
	$(MAKE) -f $(ISO_MK) $@

# ------------------------------------------------------------------------------
# Alvos agregadores
# ------------------------------------------------------------------------------

# Compila tudo que entra na imagem, sem montar a ISO.
image: kernel user-programs ramfs-image

# Pipeline completo até a ISO inicializável.
release: iso

clean:
	rm -rf target $(BUILD_DIR)
