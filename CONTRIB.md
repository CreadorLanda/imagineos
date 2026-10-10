# Contribuindo

- Kernel: Rust stable, `no_std`, target `x86_64-unknown-none`.
- Valide com `rustup run stable cargo check --target x86_64-unknown-none` e `rustup run stable cargo build --release --target x86_64-unknown-none`.
- O build é dividido por etapa em `build/`: `common.mk` (variáveis), `kernel.mk`
  (kernel, bootstrap, dzImage), `userland.mk` (apps, utilitários, libc, TCC),
  `ramfs.mk` (empacotamento USTAR) e `iso.mk` (ISO e QEMU). O `Makefile` raiz só
  delega a cada um com `make -f`. Rode uma etapa isolada, por exemplo
  `make -f build/userland.mk user-programs`; ao mexer em uma etapa, mantenha as
  variáveis compartilhadas em `build/common.mk`, nunca duplicadas.
- Para testar o parser RAMFS junto com a camada VFS:
  `rustup run stable rustc --edition 2021 --test kernel/src/tests/vfs.rs -o /tmp/vfs-tests && /tmp/vfs-tests`.
  O alvo isolado `kernel/src/fs/ramfs.rs` não compila sozinho (depende de
  `crate::vfs`); use o teste de integração em `kernel/src/tests/`.
- Antes de gerar a imagem, garanta que exista `ramfs/` na raiz do projeto. Se a pasta não existir, crie-a com `mkdir -p ramfs`; ela é a árvore de origem do sistema de arquivos da imagem.
- O sistema de arquivos raiz da imagem vem de `ramfs/`; binários Rust de usuário são empacotados em `/bin` pelo alvo `make iso`.
- O kernel fica em `kernel/src/`; o loader fica em `bootstrap/`; aplicativos de userspace, incluindo `init` e `getty`, ficam em `userland/apps/`. Utilitários em `userland/utilities/` são copiados pelo build para `/bin`; `init` e `getty` são instalados em `/sbin` sem sufixo no nome.
- Preserve os comandos internos (built-ins) e a busca por `PATH` no shell; não adicione comandos de escrita enquanto o RAMFS for somente leitura.
- Mudanças em boot, memória ou interrupções também precisam de teste da ISO em QEMU/OVMF antes de serem consideradas validadas.
- Não copie executáveis de usuário como arquivos independentes no ESP; o kernel os carrega do arquivo `ramfs.tar`.
