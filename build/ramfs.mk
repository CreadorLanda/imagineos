# ==============================================================================
# ImagineOS - estrutura e imagem do initramfs (estilo Linux/FHS)
# ==============================================================================
#
# Cria a árvore `ramfs/` na raiz e empacota o USTAR `.build/initramfs.tar`,
# que o Limine carrega como módulo de boot. A árvore segue o FHS:
#   * a árvore de dados de `ramfs/` com a fonte PSF copiada;
#   * os headers da libc e as libs estáticas (libdreamcore.a, libc.a, libm.a);
#   * a árvore de dados de `ramfs/` com a fonte PSF copiada;
#   * os headers da libc e as libs estáticas (libdreamcore.a, libc.a, libm.a);
#   * os objetos crt0/crt1/crti/crtn e o linker script do userspace;
#   * os binários compilados (init, getty, shell e utilitários);
#   * o payload do instalador (bootstrap, dzImage, Limine e initramfs instalado).
#
# Uso isolado:  make -f build/ramfs.mk
# Alvos:        ramfs, ramfs-image

RAMFS_MK := $(abspath $(lastword $(MAKEFILE_LIST)))

include $(dir $(lastword $(MAKEFILE_LIST)))common.mk

# As peças que entram no tar vêm de outros makefiles. Em vez de duplicar suas
# regras aqui, pedimos que o make os construa recursivamente: kernel.mk produz
# bootstrap/dzImage e userland.mk produz os programas e as libs de userspace.
KERNEL_MK   := $(ROOT)/build/kernel.mk
USERLAND_MK := $(ROOT)/build/userland.mk

# Todo arquivo da árvore `ramfs/` é dependência do tar. Sem isto, editar um
# arquivo de `ramfs/` (um script em `etc/`, uma fonte) não disparava a
# reconstrução e a imagem continuava com a versão antiga em cache.
RAMFS_SOURCES := $(shell find $(RAMFS_ROOT) -type f -o -type l 2>/dev/null)

.PHONY: ramfs ramfs-image ramfs-inputs

ramfs:
	@mkdir -p $(addprefix $(RAMFS_ROOT)/,$(RAMFS_DIRS))

# Constrói tudo o que o USTAR empacota antes de montá-lo.
ramfs-inputs: ramfs
	$(MAKE) -f $(KERNEL_MK) bootstrap dzimage
	$(MAKE) -f $(USERLAND_MK) user-programs user-libs \
		$(BUILD_DIR)/user/utilities/hello_c $(BUILD_DIR)/user/utilities/tcc \
		$(TCC_SOURCE)/libtcc1.a $(TCC_SOURCE)/runmain.o

ramfs-image: ramfs-inputs
	$(MAKE) -f $(RAMFS_MK) $(RAMFS_IMAGE)

# A regra do tar só lista entradas que este makefile sabe localizar: os
# artefatos compilados (`bootstrap`, `dzImage`, programas, libs) são garantidos
# por `ramfs-inputs` e usados diretamente nos comandos abaixo. Declará-los como
# pré-requisitos aqui exigiria as regras de kernel.mk/userland.mk, que é
# exatamente o acoplamento que a divisão elimina.
$(BUILD_DIR)/initramfs.tar: ramfs $(RAMFS_SOURCES) \
                $(LIMINE_DIR)/BOOTX64.EFI $(ROOT)/boot/limine.conf \
                $(ROOT)/boot/startup.nsh $(USERLAND_MK) $(KERNEL_MK)
	@test -f $(BOOTSTRAP) || (echo "bootstrap.elf ausente"; exit 1)
	@test -f $(DZ_IMAGE) || (echo "dzImage ausente"; exit 1)
	@test -f $(BUILD_DIR)/user/sbin/init || (echo "programas de userspace ausentes"; exit 1)
	rm -rf $(BUILD_DIR)/ramfs
	mkdir -p $(BUILD_DIR)/ramfs
	cp -a $(RAMFS_ROOT)/. $(BUILD_DIR)/ramfs/
	cp $(FONT_ASSET) $(BUILD_DIR)/ramfs/system/fonts/zap-vga16.psf
	mkdir -p $(BUILD_DIR)/ramfs/usr/include $(BUILD_DIR)/ramfs/usr/lib/tcc
	cp -a $(ROOT)/userland/libc/include/. $(BUILD_DIR)/ramfs/usr/include/
	cp $(BUILD_DIR)/user/libdreamcore.a $(BUILD_DIR)/ramfs/usr/lib/libdreamcore.a
	cp $(BUILD_DIR)/user/libdreamcore.a $(BUILD_DIR)/ramfs/usr/lib/libc.a
	$(AR) rcs $(BUILD_DIR)/user/libm.a $(BUILD_DIR)/user/math.o
	cp $(BUILD_DIR)/user/libm.a $(BUILD_DIR)/ramfs/usr/lib/libm.a
	cp $(BUILD_DIR)/user/crt.o $(BUILD_DIR)/ramfs/usr/lib/crt0.o
	cp $(BUILD_DIR)/user/crt.o $(BUILD_DIR)/ramfs/usr/lib/crt1.o
	cp $(BUILD_DIR)/user/crt_empty.o $(BUILD_DIR)/ramfs/usr/lib/crti.o
	cp $(BUILD_DIR)/user/crt_empty.o $(BUILD_DIR)/ramfs/usr/lib/crtn.o
	cp $(ROOT)/userland/linker.ld $(BUILD_DIR)/ramfs/usr/lib/imagineos.ld
	cp $(TCC_SOURCE)/libtcc1.a $(BUILD_DIR)/ramfs/usr/lib/tcc/libtcc1.a
	cp $(TCC_SOURCE)/runmain.o $(BUILD_DIR)/ramfs/usr/lib/tcc/runmain.o

	# Binários userland
	cp $(BUILD_DIR)/user/sbin/init $(BUILD_DIR)/ramfs/sbin/init
	cp $(BUILD_DIR)/user/sbin/getty $(BUILD_DIR)/ramfs/sbin/getty
	cp $(BUILD_DIR)/user/bin/shell $(BUILD_DIR)/ramfs/bin/shell
	for utility in $(USER_UTILITIES); do \
		cp $(BUILD_DIR)/user/utilities/$$utility $(BUILD_DIR)/ramfs/bin/$$utility; \
	done
	for utility in $(USER_C_UTILITIES); do \
		cp $(BUILD_DIR)/user/utilities/$$utility $(BUILD_DIR)/ramfs/bin/$$utility; \
	done
	# Clientes EDE (protocolo de superfícies). O binário sai como
	# .build/user/bin/ede-<nome>, copiado para /bin/ede-<nome>.
	for program in $(notdir $(EDE_PROGRAMS)); do \
		cp $(BUILD_DIR)/user/bin/$$program $(BUILD_DIR)/ramfs/bin/$$program; \
	done

	# Tar para o instalador do SO
	tar --format=ustar --numeric-owner --owner=0 --group=0 \
		--exclude=bin/distroinstall --exclude='system/install/*' \
		-C $(BUILD_DIR)/ramfs -cf $(RAMFS_INST_IMG) bin dev etc sbin home system tmp usr

	# Adiciona os arquivos do instalador
	mkdir -p $(BUILD_DIR)/ramfs/system/install
	cp $(BOOTSTRAP) $(BUILD_DIR)/ramfs/system/install/bootstrap.elf
	cp $(DZ_IMAGE) $(BUILD_DIR)/ramfs/system/install/dzImage
	cp $(LIMINE_DIR)/BOOTX64.EFI $(BUILD_DIR)/ramfs/system/install/BOOTX64.EFI
	cp $(ROOT)/boot/limine.conf $(BUILD_DIR)/ramfs/system/install/limine.conf
	cp $(ROOT)/boot/startup.nsh $(BUILD_DIR)/ramfs/system/install/startup.nsh
	cp $(RAMFS_INST_IMG) $(BUILD_DIR)/ramfs/system/install/initramfs-installed.tar
	# Tar final do initramfs
	tar --format=ustar --numeric-owner --owner=0 --group=0 \
		-C $(BUILD_DIR)/ramfs -cf $@ bin dev etc sbin home system tmp usr
