# Testes

## Compilação e análise do parser USTAR

Para criar um utilitário de userspace, siga o
[guia de desenvolvimento de programas](userspace.md).

Antes de gerar o arquivo RAMFS, confirme que `ramfs/` existe na raiz do projeto.
Se necessário, crie a pasta com `mkdir -p ramfs`.

```sh
rustup run stable cargo check --target x86_64-unknown-none
rustup run stable cargo build --release --target x86_64-unknown-none
rustup run stable cargo test --manifest-path userland/api/Cargo.toml --target x86_64-unknown-linux-gnu
make user-programs
rustup run stable rustc --edition 2024 --test kernel/src/fs/ramfs.rs -o /tmp/dreamcore-ramfs-tests
/tmp/dreamcore-ramfs-tests
rustup run stable rustc --edition 2024 --test kernel/src/exec/elf.rs -o /tmp/dreamcore-elf-tests
/tmp/dreamcore-elf-tests
rustup run stable rustc --edition 2024 --test kernel/src/config.rs -o /tmp/dreamcore-config-tests
/tmp/dreamcore-config-tests
rustup run stable rustc --edition 2024 --test kernel/src/console/utf8.rs -o /tmp/dreamcore-utf8-tests
/tmp/dreamcore-utf8-tests
rustup run stable rustc --edition 2024 --test kernel/src/fs/permissions.rs -o /tmp/imagineos-permissions-tests
/tmp/imagineos-permissions-tests
rustup run stable rustc --edition 2024 --test kernel/src/crypto.rs -o /tmp/imagineos-crypto-tests
/tmp/imagineos-crypto-tests
rustup run stable rustc --edition 2024 --test kernel/src/tests/keyboard.rs -o /tmp/dreamcore-keyboard-tests
/tmp/dreamcore-keyboard-tests
rustup run stable rustc --edition 2024 --test kernel/src/tests/storage.rs -o /tmp/imagineos-storage-tests
/tmp/imagineos-storage-tests
rustup run stable rustc --edition 2024 --test kernel/src/tests/dfs.rs -o /tmp/imagineos-dfs-tests
/tmp/imagineos-dfs-tests
rustup run stable rustc --edition 2024 --test kernel/src/tests/installer.rs -o /tmp/imagineos-installer-tests
/tmp/imagineos-installer-tests
rustup run stable rustc --edition 2024 --test kernel/src/tests/kernel_log.rs -o /tmp/imagineos-kernel-log-tests
/tmp/imagineos-kernel-log-tests
rustup run stable rustc --edition 2024 --test kernel/src/tests/shutdown.rs -o /tmp/imagineos-shutdown-tests
/tmp/imagineos-shutdown-tests
rustup run stable rustc --edition 2024 --test bootstrap/dzimage.rs -o /tmp/imagineos-dzimage-tests
/tmp/imagineos-dzimage-tests
rustup run stable rustc --edition 2024 --test tools/dzpack.rs -o /tmp/imagineos-dzpack-tests
/tmp/imagineos-dzpack-tests
make .build/ramfs.tar
tar -tf .build/ramfs.tar
```

O tar deve conter `sbin/init`, `sbin/getty`, `bin/shell`, `bin/ls`, `bin/cat`,
`bin/grep`, `bin/globalconf`, `bin/distroinstall`, `bin/vi`, `bin/mkdir`, `bin/rm`, `bin/touch`,
`bin/hello`, `bin/fdtest`, `bin/dmesg`, `bin/uname`, `bin/kill`, `bin/ps`,
`bin/su`, `bin/sudo`,
`home/.global/global.conf` e `system/fonts/zap-vga16.psf`, além dos payloads
em `system/install/`.
O RAMFS live fornece `system/install/bootstrap.elf` e `system/install/dzImage`;
o arquivo instalado não contém os payloads do instalador.

Para criar a ISO, instale `xorriso`, `dosfstools` e `mtools`, depois rode `make iso`.
Para reativar os logs de cada etapa de carregamento: `make KERNEL_FEATURES=kernel-debug iso`.

## Boot manual

Use QEMU com firmware OVMF (UEFI), conecte COM1 ao terminal e inicialize a ISO. Confirme que o kernel carrega `/sbin/init` como PID 1 e que init inicia getty. Entre como `root`/`root` e confirme o prompt de root. Teste `globalconf show`, `globalconf get keyboard`, `globalconf set keyboard us`, `globalconf set keyboard abnt2`, `globalconf get charset`, `vi /home/.global/global.conf` (sair com `:q!`), `pwd`, `cd /bin`, `ls`, `ls /sbin`, `cd /home`, `export X=astrid`, `echo "$X"`, `type ls`, `hello`, `fdtest`, `dmesg -n 10`, `uname -a`, `cat /home/readme.txt`,
`grep ImagineOS /home/readme.txt`, `ps`, `kill -l` e `read NAME`. Na imagem
alvo descartável, configure um usuário não administrador e um administrador
separadamente; verifique login com senha correta/incorreta, acesso negado ao
`/etc/users.db`, criação de arquivos no próprio `/home/usuario`, recusa de
escrita em `/etc`, `sudo uname -a` apenas para o administrador e `su` com senha
da conta de destino. Teste
`kill -STOP PID` seguido de `kill -CONT PID` em outro processo e confirme a
mudança de estado no snapshot do `ps`; Ctrl+C no shell deve encerrar somente a
sessão atual, que o init deve recriar. `fdtest` verifica stdio,
abertura, leitura, escrita, append, fechamento e erro de descritor fechado.
Com o layout ABNT2, teste `ç`, `á`, `ã`, `ê` e as setas no editor; no prompt,
Backspace deve remover o glifo inteiro.

`ls /dev` deve listar `/dev/hda` quando o disco IDE primary-master estiver
presente. Com uma partição DFS montada, confirme que `ls /home` omite
dotfiles, `ls -a /home` os mostra e `ls -l -a /home` também exibe tipo,
tamanho, UID:GID e permissões. Crie um arquivo, reinicie e confirme que seu
conteúdo permanece. Para validar a instalação, inicialize com `make run-installer`,
execute `distroinstall`, confirme digitando exatamente `APAGAR /dev/hda` e
`INSTALAR`, e reinicie com
`make run-disk DISK_IMAGE=.build/installer-target.img`. O utilitário apaga o
disco alvo; use apenas a imagem virtual de teste. A instalação deve chegar ao
shell sem a ISO e o RAMFS instalado não deve conter `/bin/distroinstall`.

Em QEMU/OVMF, teste `shutdown` por último; o comando deve fazer PID 1 consumir
o pedido, registrar a sincronização do disco e desligar a VM via ACPI S5.
Para validar o flush ATA, anexe uma imagem de disco à VM durante esse teste.
O parser de registradores também tem testes para GAS MMIO/validação e para a
preservação dos demais bits do registrador PM1.

Sem QEMU/OVMF ou sem as ferramentas para gerar a ISO, a compilação e os testes locais não comprovam que o sistema inicializa corretamente.

Para validar o boot por uma imagem de disco GPT em vez da ISO, use
`make disk-image` e `make run-disk`. Isso requer `qemu-img`, `sgdisk`,
`mkfs.vfat`, `mtools`, QEMU e OVMF. O teste deve confirmar a montagem do DFS,
o início do shell a partir da raiz persistente e a sobrevivência de arquivos
ao reboot. Os testes locais não substituem essa validação de ponta a ponta.
