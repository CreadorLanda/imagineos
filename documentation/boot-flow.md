# Fluxo de boot e processo init

## Fluxo atual do ImagineOS/DNU

```text
UEFI
  -> Limine
  -> bootstrap ELF + dzImage e ramfs.tar como módulos
  -> descompressão LZ4 e carga dos segmentos ELF do kernel
  -> repasse das respostas Limine e reservas de memória
  -> montagem do USTAR como initramfs
  -> montagem/recovery do DFS GPT como raiz persistente, se disponível
  -> /sbin/init (PID 1, ring 3; supervisiona pedidos de desligamento e getty)
  -> /sbin/getty
  -> /bin/shell
```

1. O firmware UEFI inicia o Limine. A configuração [`limine.conf`](../boot/limine.conf)
   aponta para `bootstrap.elf` e carrega `dzImage` e `ramfs.tar` como módulos.
2. O bootstrap pede ao Limine o mapa de memória, HHDM, framebuffer e módulos.
   Ele verifica o cabeçalho dzImage, descomprime o ELF do kernel com LZ4,
   confere o CRC32 e atualiza `Unpacking Kernel - N%/100%` na mesma linha.
3. O bootstrap valida os segmentos ELF64 `PT_LOAD`, aloca páginas físicas,
   copia segmentos, zera BSS e mapeia os endereços virtuais do kernel nas
   tabelas Limine. Também reserva a árvore de page tables ativa do Limine:
   processos compartilham esses mapeamentos de kernel, então o frame allocator
   não pode reutilizar as páginas que guardam essas tabelas. Passa ao kernel o
   mapa, framebuffer, módulos e as regiões físicas reservadas.
4. O kernel inicializa as estruturas de CPU e memória, prepara o console com
   `tools/assets/zap-vga16.psf` como fallback, usa o RSDP do Limine para
   descobrir os registradores ACPI e o estado S5, e monta o USTAR fornecido pelo Limine como
   initramfs. A leitura das tabelas ACPI mapeia no HHDM as páginas de firmware
   que ainda não estavam mapeadas.
5. Se detectar o disco e encontrar uma partição DFS pela GPT, o kernel monta o
   filesystem e reproduz qualquer transação de journal comprometida. Nesse
   caso, a fachada de arquivos encaminha operações e leituras de `/sbin/init`
   para a raiz persistente. O USTAR continua montado como fallback; sem DFS
   válido, ele também fornece a raiz ativa. Não há ainda a operação genérica
   `switch_root`/`pivot_root` de um VFS completo. Fontes PSF em
   `system/fonts/` podem vir da árvore persistente ou do USTAR; sem fonte
   válida, permanece a fonte 8x16 `tools/assets/zap-vga16.psf`.
6. O kernel procura `/sbin/init` na raiz ativa e inicia esse ELF em ring 3
   com PID 1. `init` e `getty` são compilados dos fontes em `userland/apps/`; o nome
   final dos arquivos no USTAR não tem sufixo `.elf`, embora seus conteúdos
   sejam executáveis ELF.
   Se `/sbin/init` não existir, o kernel para com uma mensagem `KERNEL PANIC`.
7. O programa `init` inicia `/sbin/getty` e continua em loop cooperativo,
   verificando o mailbox de desligamento e reiniciando o getty quando sua
   sessão termina. Getty autentica o usuário e inicia `/bin/shell`;
   `shutdown` envia um pedido IPC a PID 1, aceito somente para UID 0. Quando
   recebe o pedido, init solicita o flush do armazenamento e o
   poweroff via PM1 (I/O ou MMIO) ou Sleep Control no modo ACPI reduzido.
   Os utilitários e o shell ficam em `/bin`.

## Relação com o modelo Linux

O USTAR fornecido como módulo atua como initramfs: permite iniciar o sistema e
serve de fallback caso não exista uma partição DFS válida. Quando o DFS monta,
a camada de arquivos o usa como raiz persistente e reproduz o journal antes de
executar o init. A implementação ainda não tem um VFS genérico nem a operação
`switch_root`/`pivot_root` do Linux; o USTAR permanece montado em memória como
fallback. Assim, a mensagem de montagem do RAMFS é esperada mesmo quando o
DFS é a raiz ativa para as operações do sistema.

As linhas de log do kernel começam com `[segundos.milissegundos]`, medidos a
partir do entry point por TSC calibrado pelo PIT quando possível. Saída de
programas userspace não recebe esses prefixos. `dmesg` lê o buffer circular de
4 KiB dessas mensagens de kernel. Saída do firmware UEFI, Limine e do bootstrap
acontece antes do entry point e ainda não é incluída no buffer.

O início em ring 3 e o uso de um ELF para PID 1 seguem a mesma separação
fundamental entre kernel e userspace. O modelo de processos ainda é muito menor
que o Linux: a chamada `exec` do DNU cria outro processo, e o chamador volta a
executar quando for agendado novamente; ela não substitui a imagem do processo
chamador como `execve` faz no Linux. Também não há chamadas de `wait`, coleta
de processos filhos, reinício de serviços ou recuperação das páginas de um
processo encerrado. `init` reinicia apenas o getty da sessão. Sinais básicos
podem encerrar ou suspender processos, mas ainda não há supervisão configurável
de serviços nem coleta de processos filhos. Esse fluxo é intencionalmente
inicial, não um gerenciador de serviços completo.

## Organização do repositório

- `kernel/src/`: fontes do kernel e seus subsistemas; o ponto de entrada é
  `kernel/src/main.rs`.
- `bootstrap/`: binário loader, estruturas de boot e empacotamento dzImage.
- `userland/apps/`: fontes de init, getty e shell; utilitários ficam em
  `userland/utilities/` e a API/runtime/libc também ficam em `userland/`.
- `third_party/limine/`: arquivos do Limine necessários para gerar a ISO.
- `tools/assets/`: recursos compartilhados, como a fonte PSF.
- `ramfs/`: arquivos de dados de origem, como fontes e diretórios iniciais.
  Durante o staging o build acrescenta os programas compilados em `/sbin` e
  `/bin`.
- `.build/distro/`: imagens ISO geradas.
- `documentation/`: arquitetura, build, fluxo de boot e guias de contribuição.

Os manifests e as regras de build ficam na raiz: `Cargo.toml` define os bins
`dreamcore` e `bootstrap`, e `Makefile` compila o kernel, cria `dzImage`,
compila o bootstrap e empacota os programas de `userland/` no USTAR.

## Recursos ainda não implementados

O dzImage é um formato próprio de payload LZ4 com tamanho e CRC32, não é
compatível com o protocolo de boot do Linux nem substitui o Limine. Ainda não
há descoberta abrangente de hardware por ACPI/Device Tree, KMS ou raiz
persistente.
