# ==============================================================================
# ImagineOS - kernel, bootstrap e dzImage
# ==============================================================================
#
# Compila o kernel `dreamcore`, o carregador `bootstrap.elf` e empacota o kernel
# como `.build/dzImage` (payload LZ4 com tamanho e CRC32).
#
# Uso isolado:  make -f build/kernel.mk
# Alvos:        kernel, bootstrap, dzimage, tools

include $(dir $(lastword $(MAKEFILE_LIST)))common.mk

.PHONY: kernel bootstrap dzimage tools

# ------------------------------------------------------------------------------
# Kernel
# ------------------------------------------------------------------------------

kernel: $(KERNEL)

$(KERNEL): $(wildcard $(ROOT)/kernel/src/**/*.rs) $(ROOT)/kernel/linker.ld $(FONT_ASSET)
	$(CARGO) build --release --target $(TARGET) --bin dreamcore \
		--manifest-path $(ROOT)/Cargo.toml \
		--config 'target.x86_64-unknown-none.rustflags=["-C","link-arg=-Tkernel/linker.ld","-C","relocation-model=static"]' \
		$(if $(KERNEL_FEATURES),--features $(KERNEL_FEATURES),)

# ------------------------------------------------------------------------------
# Bootstrap (carregador Limine)
# ------------------------------------------------------------------------------

bootstrap: $(BOOTSTRAP)

$(BOOTSTRAP): $(wildcard $(ROOT)/bootstrap/*.rs) \
              $(ROOT)/kernel/src/time.rs $(ROOT)/kernel/src/console/framebuffer.rs \
              $(FONT_ASSET) $(ROOT)/boot/bootstrap.ld $(ROOT)/Makefile
	$(CARGO) build --release --target $(TARGET) --bin bootstrap \
		--manifest-path $(ROOT)/Cargo.toml \
		--features bootstrap \
		--target-dir $(BUILD_DIR)/bootstrap-target \
		--config 'target.x86_64-unknown-none.rustflags=["-C","link-arg=-Tboot/bootstrap.ld","-C","relocation-model=static"]'
	@mkdir -p $(BUILD_DIR)
	cp $(BUILD_DIR)/bootstrap-target/$(TARGET)/release/bootstrap $@

# ------------------------------------------------------------------------------
# Ferramentas do host usadas pelo kernel/ISO
# ------------------------------------------------------------------------------

tools: $(DZPACK_BIN) $(DFS_IMAGE_BIN)

$(DZPACK_BIN): $(ROOT)/tools/dzpack.rs $(ROOT)/bootstrap/dzimage.rs
	@mkdir -p $(BUILD_DIR)
	$(RUSTC) --edition 2021 $< -o $@

$(DFS_IMAGE_BIN): $(ROOT)/tools/dfs-image.rs $(ROOT)/kernel/src/fs/dfs.rs \
                  $(ROOT)/kernel/src/fs/gpt.rs $(ROOT)/kernel/src/drivers/block.rs
	@mkdir -p $(BUILD_DIR)
	$(RUSTC) --edition 2021 $< -o $@

# ------------------------------------------------------------------------------
# dzImage
# ------------------------------------------------------------------------------

dzimage: $(DZ_IMAGE)

$(DZ_IMAGE): $(KERNEL) $(DZPACK_BIN)
	$(DZPACK_BIN) $(KERNEL) $@
