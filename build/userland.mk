# ==============================================================================
# ImagineOS - programas de usuário (Rust, C, libc e TinyCC)
# ==============================================================================
#
# Compila os programas de userspace para o alvo x86_64-unknown-none:
#   * apps principais: init, getty, shell      -> .build/user/{sbin,bin}
#   * utilitários Rust: ls, cat, ...           -> .build/user/utilities
#   * utilitários C: hello_c, tcc              -> .build/user/utilities
#   * libc estática (libdreamcore.a), objetos crt e runtime C.
#
# Uso isolado:  make -f build/userland.mk
# Alvos:        user-programs, user-libs, tcc-source, c-app

include $(dir $(lastword $(MAKEFILE_LIST)))common.mk

.PHONY: user-programs user-libs tcc-source c-app

user-programs: $(USER_PROGRAMS)

user-libs: $(BUILD_DIR)/user/libdreamcore.a $(BUILD_DIR)/user/crt.o $(BUILD_DIR)/user/crt_empty.o

# ------------------------------------------------------------------------------
# Libs de userspace (crate API e runtime Rust)
# ------------------------------------------------------------------------------

$(USER_API_RLIB): $(USER_API_MANIFEST) $(wildcard $(ROOT)/userland/api/src/*.rs) \
                  $(ROOT)/shared/abi/Cargo.toml $(ROOT)/shared/abi/src/lib.rs
	$(CARGO) build --manifest-path $(USER_API_MANIFEST) \
		--target $(TARGET) --release --target-dir $(USER_API_TARGET_DIR)

$(USER_RUNTIME_RLIB): $(USER_RUNTIME_MANIFEST) $(ROOT)/userland/runtimes/rust/src/lib.rs $(USER_API_RLIB)
	$(CARGO) build --manifest-path $(USER_RUNTIME_MANIFEST) \
		--target $(TARGET) --release --target-dir $(USER_RUNTIME_TARGET_DIR)

# ------------------------------------------------------------------------------
# Programas Rust de userspace
# ------------------------------------------------------------------------------

# Apps principais (init, getty, shell) em userland/apps/
$(BUILD_DIR)/user/sbin/%: $(ROOT)/userland/apps/%.rs $(ROOT)/userland/linker.ld \
                          $(USER_API_RLIB) $(USER_RUNTIME_RLIB)
	@mkdir -p $(dir $@)
	$(RUSTC) --crate-name $* --edition 2021 --target $(TARGET) \
		--extern imagineos=$(USER_API_RLIB) --extern imagineos_rt=$(USER_RUNTIME_RLIB) \
		-L dependency=$(USER_API_DEPS) -L dependency=$(USER_RUNTIME_DEPS) \
		-C panic=abort -C relocation-model=static \
		-C link-arg=-T$(ROOT)/userland/linker.ld $< -o $@

$(BUILD_DIR)/user/bin/%: $(ROOT)/userland/apps/%.rs $(ROOT)/userland/linker.ld \
                         $(USER_API_RLIB) $(USER_RUNTIME_RLIB)
	@mkdir -p $(dir $@)
	$(RUSTC) --crate-name $* --edition 2021 --target $(TARGET) \
		--extern imagineos=$(USER_API_RLIB) --extern imagineos_rt=$(USER_RUNTIME_RLIB) \
		-L dependency=$(USER_API_DEPS) -L dependency=$(USER_RUNTIME_DEPS) \
		-C panic=abort -C relocation-model=static \
		-C link-arg=-T$(ROOT)/userland/linker.ld $< -o $@

# Utilitários CLI em userland/utilities/
$(BUILD_DIR)/user/utilities/%: $(ROOT)/userland/utilities/%.rs $(ROOT)/userland/utilities/common.rs \
                               $(ROOT)/userland/linker.ld $(USER_API_RLIB) $(USER_RUNTIME_RLIB)
	@mkdir -p $(dir $@)
	$(RUSTC) --crate-name $* --edition 2021 --target $(TARGET) \
		--extern imagineos=$(USER_API_RLIB) --extern imagineos_rt=$(USER_RUNTIME_RLIB) \
		-L dependency=$(USER_API_DEPS) -L dependency=$(USER_RUNTIME_DEPS) \
		-C panic=abort -C relocation-model=static \
		-C link-arg=-T$(ROOT)/userland/linker.ld $< -o $@

# ------------------------------------------------------------------------------
# libc e runtime C
# ------------------------------------------------------------------------------

$(BUILD_DIR)/user/%.o: $(ROOT)/userland/libc/%.c $(wildcard $(ROOT)/userland/libc/include/*.h $(ROOT)/userland/libc/include/sys/*.h)
	@mkdir -p $(dir $@)
	$(CC) $(USER_CFLAGS) -c $< -o $@

# Runtime de C (em Rust): extrai o objeto da crate (ignora compiler_builtins,
# já fornecidos pelo linker) para uso como crt0.o/crt1.o e na linkagem.
$(BUILD_DIR)/user/crt.o: $(USER_CRT_RLIB)
	@mkdir -p $(dir $@) $(BUILD_DIR)/user/crt-extract
	cd $(BUILD_DIR)/user/crt-extract && $(AR) x $(abspath $(USER_CRT_RLIB)) && \
		mv imagineos_crt-*.imagineos_crt.*.rcgu.o $(abspath $(BUILD_DIR)/user/crt.o) && \
		rm -f *.o

# Staticlib da runtime C, construída a partir de userland/runtimes/c.
$(USER_CRT_RLIB): $(USER_CRT_MANIFEST) $(wildcard $(ROOT)/userland/runtimes/c/src/*.rs) $(USER_API_RLIB)
	@mkdir -p $(dir $@)
	$(CARGO) build --manifest-path $(USER_CRT_MANIFEST) \
		--target $(TARGET) --release --target-dir $(USER_CRT_TARGET_DIR)
	cp $(USER_CRT_TARGET_DIR)/$(TARGET)/release/libimagineos_crt.a $@

# Objeto vazio com a nota `.note.GNU-stack`, usado como crti.o/crtn.o pelo TinyCC.
$(BUILD_DIR)/user/crt_empty.o: $(ROOT)/userland/libc/crt_empty.S
	@mkdir -p $(dir $@)
	$(CC) $(USER_CFLAGS) -c $< -o $@

$(BUILD_DIR)/user/libdreamcore.a: $(USER_LIBC_OBJECTS)
	@mkdir -p $(dir $@)
	$(AR) rcs $@ $^

# ------------------------------------------------------------------------------
# Utilitários em C
# ------------------------------------------------------------------------------

$(BUILD_DIR)/user/utilities/hello_c: $(ROOT)/userland/utilities/hello_c.c $(ROOT)/userland/linker.ld \
                                     $(USER_CRT_OBJECTS)
	@mkdir -p $(dir $@)
	$(CC) $(USER_CFLAGS) -nostdlib -static -no-pie \
		-Wl,-T,$(ROOT)/userland/linker.ld -Wl,--build-id=none \
		$< $(USER_CRT_OBJECTS) -o $@

# Compila um app C de ramfs/home/<nome>.c para um ELF do Dreamcore.
# Uso: make c-app APP=<nome>
c-app:
	@test -n "$(APP)" || (echo "uso: make c-app APP=<nome>"; exit 1)
	$(MAKE) -f $(lastword $(MAKEFILE_LIST)) $(BUILD_DIR)/user/c/$(APP).elf

$(BUILD_DIR)/user/c/%.elf: $(ROOT)/ramfs/home/%.c $(ROOT)/userland/libc/include/dreamcore.h \
                           $(USER_CRT_OBJECTS) $(ROOT)/userland/linker.ld
	@mkdir -p $(BUILD_DIR)/user/c
	$(CC) $(USER_CFLAGS) -c $< -o $(BUILD_DIR)/user/c/$*.o
	$(CC) $(USER_CFLAGS) -nostdlib -static -no-pie -Wl,-T$(ROOT)/userland/linker.ld \
		-Wl,--build-id=none $(BUILD_DIR)/user/c/$*.o $(USER_CRT_OBJECTS) -o $@

# ------------------------------------------------------------------------------
# TinyCC (host e versão nativa do ImagineOS)
# ------------------------------------------------------------------------------

$(TCC_SOURCE)/configure:
	@echo "TinyCC source is missing. Run 'make tcc-source' to fetch the pinned upstream revision."
	@false

tcc-source:
	@if [ ! -x "$(TCC_SOURCE)/configure" ]; then \
		git clone https://github.com/TinyCC/tinycc.git "$(TCC_SOURCE)" && \
		git -C "$(TCC_SOURCE)" checkout --detach "$(TCC_SOURCE_REV)"; \
	fi

$(TCC_CONFIG_STAMP): $(TCC_SOURCE)/configure $(ROOT)/Makefile
	@mkdir -p $(BUILD_DIR)/toolchain
	cd $(TCC_SOURCE) && ./configure --prefix=/usr --tccdir=/usr/lib/tcc \
		--sysincludepaths=/usr/include:/usr/include/x86_64-linux-gnu:$(shell $(CC) -print-file-name=include) \
		--libpaths=/usr/lib:/usr/lib/x86_64-linux-gnu \
		--crtprefix=/usr/lib/x86_64-linux-gnu:/usr/lib \
		--config-bcheck=no --config-backtrace=no
	touch $@

$(TCC_PATCH_STAMP): $(ROOT)/userland/patches/tinycc-imagineos.patch $(TCC_SOURCE)/tccrun.c $(TCC_CONFIG_STAMP)
	@if ! grep -Fq 'TCCSYM(gettimeofday)' $(TCC_SOURCE)/tccrun.c; then \
		patch --forward -p1 -d $(TCC_SOURCE) < $<; \
	fi
	touch $@

$(TCC_HOST): $(TCC_CONFIG_STAMP) $(TCC_PATCH_STAMP) $(TCC_SOURCE)/Makefile $(TCC_SOURCE)/tcc.c
	@mkdir -p $(BUILD_DIR)/toolchain
	$(MAKE) -C $(TCC_SOURCE)
	cp $(TCC_SOURCE)/tcc $@

$(TCC_SOURCE)/libtcc1.a: $(TCC_HOST)
	@test -f $@

$(TCC_SOURCE)/runmain.o: $(TCC_HOST)
	@test -f $@

$(BUILD_DIR)/user/tcc/%.o: $(TCC_SOURCE)/%.c $(TCC_HOST) $(TCC_PATCH_STAMP) \
                            $(wildcard $(ROOT)/userland/libc/include/*.h $(ROOT)/userland/libc/include/sys/*.h)
	@mkdir -p $(dir $@)
	$(CC) $(TCC_NATIVE_CFLAGS) -c $< -o $@

$(BUILD_DIR)/user/utilities/tcc: $(TCC_NATIVE_OBJECTS) $(USER_CRT_OBJECTS) $(ROOT)/userland/linker.ld
	@mkdir -p $(dir $@)
	$(CC) $(USER_CFLAGS) -nostdlib -static -no-pie \
		-Wl,-T,$(ROOT)/userland/linker.ld -Wl,--build-id=none \
		$(TCC_NATIVE_OBJECTS) $(USER_CRT_OBJECTS) -o $@
