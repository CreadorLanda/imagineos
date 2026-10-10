# ==============================================================================
# ImagineOS - ISO inicializável e execução no QEMU
# ==============================================================================
#
# Monta a ISO UEFI (ESP FAT64 + Limine + bootstrap + dzImage + ramfs.tar) e
# fornece os alvos de execução no QEMU, incluindo disco GPT e instalador.
#
# Uso isolado:  make -f build/iso.mk iso
# Alvos:        iso, run, disk-image, run-disk, installer-disk, run-installer

include $(dir $(lastword $(MAKEFILE_LIST)))common.mk

# Cada etapa tem seu próprio makefile, então a ISO depende dos *alvos* das
# outras etapas (que o make resolve recursivamente) em vez de tentar recriar
# as regras de arquivo que vivem em kernel.mk/ramfs.mk.
KERNEL_MK := $(ROOT)/build/kernel.mk
RAMFS_MK  := $(ROOT)/build/ramfs.mk

.PHONY: iso run disk-image run-disk installer-disk run-installer \
        image-payloads

# Payloads que a ISO consome: bootstrap + dzImage (kernel.mk) e ramfs.tar
# (ramfs.mk). O ramfs, por sua vez, já puxa o kernel e o userland.
image-payloads:
	$(MAKE) -f $(KERNEL_MK) bootstrap dzimage
	$(MAKE) -f $(RAMFS_MK) ramfs-image

# ------------------------------------------------------------------------------
# ISO
# ------------------------------------------------------------------------------

iso: image-payloads
	@# Confirma que os artefatos existem antes de montar a ISO.
	@test -f $(BOOTSTRAP) || (echo "bootstrap.elf ausente"; exit 1)
	@test -f $(DZ_IMAGE) || (echo "dzImage ausente"; exit 1)
	@test -f $(RAMFS_IMAGE) || (echo "ramfs.tar ausente"; exit 1)
	@command -v xorriso >/dev/null || (echo "xorriso não encontrado"; exit 1)
	@command -v mkfs.vfat >/dev/null || (echo "mkfs.vfat não encontrado"; exit 1)
	@command -v mcopy >/dev/null || (echo "mcopy não encontrado"; exit 1)
	@command -v mmd >/dev/null || (echo "mmd não encontrado"; exit 1)
	rm -rf $(ISO_DIR)
	mkdir -p $(ISO_DIR)/EFI/BOOT $(ISO_DIR)/boot $(DISTRO_DIR)
	cp $(BOOTSTRAP) $(ISO_DIR)/boot/bootstrap.elf
	cp $(DZ_IMAGE) $(ISO_DIR)/boot/dzImage
	cp $(RAMFS_IMAGE) $(ISO_DIR)/boot/ramfs.tar
	cp $(ROOT)/boot/limine.conf $(ISO_DIR)/limine.conf
	cp $(LIMINE_DIR)/BOOTX64.EFI $(ISO_DIR)/EFI/BOOT/BOOTX64.EFI
	dd if=/dev/zero of=$(ISO_DIR)/efi.img bs=1M count=64
	mkfs.vfat $(ISO_DIR)/efi.img
	mmd -i $(ISO_DIR)/efi.img ::/EFI ::/EFI/BOOT ::/boot
	mcopy -i $(ISO_DIR)/efi.img $(ISO_DIR)/EFI/BOOT/BOOTX64.EFI ::/EFI/BOOT/
	mcopy -i $(ISO_DIR)/efi.img $(ISO_DIR)/limine.conf ::/limine.conf
	mcopy -i $(ISO_DIR)/efi.img $(ISO_DIR)/boot/bootstrap.elf ::/boot/bootstrap.elf
	mcopy -i $(ISO_DIR)/efi.img $(ISO_DIR)/boot/dzImage ::/boot/dzImage
	mcopy -i $(ISO_DIR)/efi.img $(ISO_DIR)/boot/ramfs.tar ::/boot/ramfs.tar
	xorriso -as mkisofs -R -r -J -V DREAMCORE \
		--efi-boot efi.img -efi-boot-part --efi-boot-image \
		--protective-msdos-label $(ISO_DIR) -o $(ISO_IMAGE)
	@printf 'ISO gerada com sucesso: %s\n' '$(ISO_IMAGE)'

# ------------------------------------------------------------------------------
# Execução no QEMU
# ------------------------------------------------------------------------------

run: iso
	@test -f "$(OVMF_CODE)" || (echo "OVMF firmware não encontrado em $(OVMF_CODE)"; exit 1)
	$(QEMU) -machine q35 -m 512M -serial stdio \
		-drive if=pflash,format=raw,unit=0,readonly=on,file="$(OVMF_CODE)" \
		-cdrom $(ISO_IMAGE)

disk-image: iso
	$(MAKE) -f $(KERNEL_MK) $(DFS_IMAGE_BIN)
	@if [ -e "$(DISK_IMAGE)" ]; then \
		printf 'Usando imagem de disco existente: %s\n' "$(DISK_IMAGE)"; \
	else \
		sh $(ROOT)/tools/install-disk.sh "$(DISK_IMAGE)" \
			"$(ISO_DIR)/boot/bootstrap.elf" "$(ISO_DIR)/boot/dzImage" \
			"$(RAMFS_INST_IMG)" \
			"$(ISO_DIR)/EFI/BOOT/BOOTX64.EFI" "$(ISO_DIR)/limine.conf" \
			"$(ROOT)/boot/startup.nsh" "$(DFS_IMAGE_BIN)"; \
	fi

run-disk: disk-image
	@test -f "$(OVMF_CODE)" || (echo "OVMF firmware não encontrado em $(OVMF_CODE)"; exit 1)
	$(QEMU) -machine pc -m 512M -serial stdio \
		-drive if=pflash,format=raw,unit=0,readonly=on,file="$(OVMF_CODE)" \
		-drive if=ide,index=0,format=raw,file="$(DISK_IMAGE)"

installer-disk:
	@mkdir -p $(BUILD_DIR)
	@if [ ! -e "$(INSTALL_TARGET)" ]; then \
		qemu-img create -f raw "$(INSTALL_TARGET)" 1G; \
	fi

run-installer: iso installer-disk
	@test -f "$(OVMF_CODE)" || (echo "OVMF firmware não encontrado em $(OVMF_CODE)"; exit 1)
	$(QEMU) -machine pc -m 512M -serial stdio -boot order=d \
		-drive if=pflash,format=raw,unit=0,readonly=on,file="$(OVMF_CODE)" \
		-drive if=ide,index=0,format=raw,file="$(INSTALL_TARGET)" \
		-cdrom $(ISO_IMAGE)
