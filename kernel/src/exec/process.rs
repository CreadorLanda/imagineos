use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::ptr;

use crate::elf::{Elf64, Error as ElfError};
use crate::fpu;
use crate::gdt;
use crate::paging::{self, AddressSpace, MapError};
use crate::syscall::TrapFrame;
pub use imagineos_abi::UserArg;

const MAX_PROCESSES: usize = imagineos_abi::MAX_PROCESSES;
// The synchronous DFS installer nests large journal transaction buffers.
const KERNEL_STACK_SIZE: usize = 64 * 1024;
// A full-featured shell keeps parsed commands, expanded words and redirection
// buffers on the user stack at once. 32 KiB proved too tight (a page fault at
// the top of the stack while running `echo`). After `fork`, the child inherits
// the parent's stack at whatever depth the fork happened, and the path from
// there to `execve` (pipe setup plus command expansion) needs room of its own.
// 128 KiB covers that comfortably; the space is virtual until touched.
const USER_STACK_SIZE: u64 = 32 * 4096;
pub const USER_STACK_TOP: u64 = 0x0000_7fff_ffff_0000;
const PAGE_SIZE: u64 = 4096;
const USER_HEAP_START: u64 = 0x0000_0001_0000_0000;
const USER_HEAP_LIMIT: u64 = 64 * 1024 * 1024;
pub const MAX_EXEC_ARGS: usize = imagineos_abi::MAX_EXEC_ARGS;
pub const MAX_EXEC_ENV: usize = imagineos_abi::MAX_EXEC_ENV;
pub const MAX_OPEN_FDS: usize = imagineos_abi::MAX_OPEN_FDS;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DescriptorKind {
    Closed,
    Stdin,
    Stdout,
    Stderr,
    File,
    /// Extremidade de leitura de um pipe (ver `crate::ipc`).
    PipeRead,
    /// Extremidade de escrita de um pipe.
    PipeWrite,
}

#[derive(Clone, Copy)]
pub struct Descriptor {
    pub kind: DescriptorKind,
    pub path: [u8; imagineos_abi::MAX_MUTABLE_PATH],
    pub path_length: usize,
    pub offset: usize,
    pub flags: u64,
    pub input: [u8; 4],
    pub input_length: usize,
    pub input_offset: usize,
    /// Id do pipe quando `kind` é `PipeRead`/`PipeWrite`.
    pub pipe: crate::ipc::PipeId,
}

impl Descriptor {
    const CLOSED: Self = Self::new(DescriptorKind::Closed);

    const fn new(kind: DescriptorKind) -> Self {
        Self {
            kind,
            path: [0; imagineos_abi::MAX_MUTABLE_PATH],
            path_length: 0,
            offset: 0,
            flags: 0,
            input: [0; 4],
            input_length: 0,
            input_offset: 0,
            pipe: 0,
        }
    }

    /// Cria um descritor que referencia uma extremidade de pipe.
    pub const fn pipe(kind: DescriptorKind, pipe: crate::ipc::PipeId) -> Self {
        let mut descriptor = Self::new(kind);
        descriptor.pipe = pipe;
        descriptor
    }

    pub fn file(path: &str, flags: u64, offset: usize) -> Self {
        let mut descriptor = Self::new(DescriptorKind::File);
        descriptor.path[..path.len()].copy_from_slice(path.as_bytes());
        descriptor.path_length = path.len();
        descriptor.flags = flags;
        descriptor.offset = offset;
        descriptor
    }
}

#[derive(Clone, Copy)]
pub struct DescriptorTable([Descriptor; MAX_OPEN_FDS]);

impl DescriptorTable {
    const fn new() -> Self {
        let mut descriptors = [Descriptor::CLOSED; MAX_OPEN_FDS];
        descriptors[0] = Descriptor::new(DescriptorKind::Stdin);
        descriptors[1] = Descriptor::new(DescriptorKind::Stdout);
        descriptors[2] = Descriptor::new(DescriptorKind::Stderr);
        Self(descriptors)
    }

    fn allocate(&mut self, descriptor: Descriptor) -> Option<usize> {
        let slot = (3..self.0.len()).find(|&index| self.0[index].kind == DescriptorKind::Closed)?;
        self.0[slot] = descriptor;
        Some(slot)
    }
}

struct SharedDescriptorTables(UnsafeCell<[DescriptorTable; MAX_PROCESSES]>);
unsafe impl Sync for SharedDescriptorTables {}
static DESCRIPTOR_TABLES: SharedDescriptorTables =
    SharedDescriptorTables(UnsafeCell::new([DescriptorTable::new(); MAX_PROCESSES]));

#[repr(align(16))]
struct KernelStacks([[u8; KERNEL_STACK_SIZE]; MAX_PROCESSES]);

struct SharedStacks(UnsafeCell<KernelStacks>);
unsafe impl Sync for SharedStacks {}
static KERNEL_STACKS: SharedStacks = SharedStacks(UnsafeCell::new(KernelStacks(
    [[0; KERNEL_STACK_SIZE]; MAX_PROCESSES],
)));

/// Área de estado FPU/SSE (`fxsave`/`fxrstor`) alinhada a 16 bytes.
/// Necessária para que cada processo tenha seus próprios registradores XMM
/// e `mxcsr` preservados nas trocas de contexto.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct FpuState([u8; fpu::FXSAVE_SIZE]);

impl FpuState {
    const EMPTY: Self = Self([0; fpu::FXSAVE_SIZE]);

    const _ALIGN_CHECK: () = assert!(core::mem::align_of::<Self>() >= fpu::FXSAVE_ALIGN);

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.0.as_mut_ptr()
    }

    fn as_ptr(&self) -> *const u8 {
        self.0.as_ptr()
    }
}

#[derive(Clone, Copy)]
struct Process {
    pid: usize,
    active: bool,
    stopped: bool,
    /// Processo que chamou `fork`/`spawn`. `0` significa "sem pai" (PID 1, que é
    /// criado pelo kernel). Usado por `waitpid` e pela coleta após a morte.
    parent: usize,
    /// Processo já terminou, mas o pai ainda não leu o código de saída.
    /// O slot e o `exit_status` ficam reservados até um `waitpid` coletar.
    zombie: bool,
    /// Código de saída registrado quando o processo terminou.
    exit_status: i32,
    /// `pid` que este processo está aguardando em `waitpid` (0 = não espera).
    /// Usado para bloquear o pai sem consumir CPU até o filho terminar.
    waiting_for: usize,
    /// Ticks de timer acumulados por este processo desde a última troca.
    ///
    /// É por processo de propósito: um contador global fazia o quantum reiniciar
    /// a cada troca, e dois processos acabavam se alternando a cada tick (um
    /// milissegundo) em vez de rodar o quantum inteiro — o filho do `fork`
    /// retomava sempre no mesmo `rip` e parecia travado.
    quantum: u64,
    allows_disk_install: bool,
    uid: u32,
    gid: u32,
    is_admin: bool,
    username: [u8; imagineos_abi::ACCOUNT_NAME_SIZE],
    username_length: usize,
    frame: *mut TrapFrame,
    address_space: Option<AddressSpace>,
    heap_end: u64,
    name: [u8; imagineos_abi::PROCESS_NAME_SIZE],
    pending_signals: u64,
    signal_actions: [u64; 32],
    signal_restorer: u64,
    signal_frame: Option<TrapFrame>,
    fpu_state: FpuState,
}

impl Process {
    const EMPTY: Self = Self {
        pid: 0,
        active: false,
        stopped: false,
        parent: 0,
        zombie: false,
        exit_status: 0,
        waiting_for: 0,
        quantum: 0,
        allows_disk_install: false,
        uid: 0,
        gid: 0,
        is_admin: true,
        username: {
            let mut name = [0; imagineos_abi::ACCOUNT_NAME_SIZE];
            name[0] = b'r';
            name[1] = b'o';
            name[2] = b'o';
            name[3] = b't';
            name
        },
        username_length: 4,
        frame: ptr::null_mut(),
        address_space: None,
        heap_end: USER_HEAP_START,
        name: [0; imagineos_abi::PROCESS_NAME_SIZE],
        pending_signals: 0,
        signal_actions: [imagineos_abi::SIGNAL_DEFAULT; 32],
        signal_restorer: 0,
        signal_frame: None,
        fpu_state: FpuState::EMPTY,
    };
}

struct Scheduler {
    processes: [Process; MAX_PROCESSES],
    count: usize,
    current: usize,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            processes: [Process::EMPTY; MAX_PROCESSES],
            count: 0,
            current: 0,
        }
    }
}

struct SharedScheduler(UnsafeCell<Scheduler>);
unsafe impl Sync for SharedScheduler {}
static SCHEDULER: SharedScheduler = SharedScheduler(UnsafeCell::new(Scheduler::new()));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError {
    InvalidElf(ElfError),
    OutOfMemory,
    InvalidMapping,
}

pub fn init(programs: &[(&[u8], usize)]) -> Result<(), LoadError> {
    debug_log("process: initializing scheduler\n");
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    scheduler.processes = [Process::EMPTY; MAX_PROCESSES];
    scheduler.count = 0;
    scheduler.current = 0;
    unsafe {
        *DESCRIPTOR_TABLES.0.get() = [DescriptorTable::new(); MAX_PROCESSES];
    }

    for &(image, pid) in programs.iter().take(MAX_PROCESSES) {
        #[cfg(feature = "kernel-debug")]
        {
            crate::console_write("process: loading PID ");
            crate::console_write_number(pid as u64);
            crate::console_write(" ELF\n");
        }
        let slot = scheduler.count;
        let arguments: [&[u8]; 1] = [b"/sbin/init"];
        let process = load_elf(image, pid, slot, &arguments, &[], false, "/sbin/init")?;
        scheduler.processes[scheduler.count] = process;
        scheduler.count += 1;
    }
    if scheduler.count == 0 {
        return Err(LoadError::InvalidMapping);
    }
    Ok(())
}

/// Onde o `TrapFrame` inicial de um processo novo é escrito.
///
/// O `spawn` usa o topo da pilha de kernel do slot (ninguém mais está usando a
///quele slot). O `execve`, ao contrário, roda no slot do próprio processo, cujo
/// topo está ocupado pelo frame do `int 0x80` em andamento — por isso ele passa
/// um endereço na base da pilha (ver [`execve_frame_scratch`]).
enum FrameLocation {
    /// Topo da pilha de kernel do slot indicado.
    TopOfSlot(usize),
    /// Endereço explícito, escolhido pelo chamador.
    At(*mut TrapFrame),
}

/// Endereço, na base da pilha de kernel de `slot`, onde o `execve` monta o
/// frame do novo programa sem sobrepor o frame de syscall ativo.
fn execve_frame_scratch(slot: usize) -> Option<*mut TrapFrame> {
    let stacks = unsafe { &mut *KERNEL_STACKS.0.get() };
    let base = stacks.0[slot].as_mut_ptr() as usize;
    // Um frame a partir da base: o stub de syscall trabalha no topo, então a
    // base está livre. A folga de um segundo frame protege contra um aninhamento
    // inesperado (sinal durante a syscall, por exemplo).
    let offset = 2 * size_of::<TrapFrame>();
    if offset >= KERNEL_STACK_SIZE {
        return None;
    }
    Some((base + offset) as *mut TrapFrame)
}

fn load_elf(
    image: &[u8],
    pid: usize,
    slot: usize,
    arguments: &[&[u8]],
    environment: &[&[u8]],
    allows_disk_install: bool,
    name: &str,
) -> Result<Process, LoadError> {
    load_elf_at(
        image,
        pid,
        arguments,
        environment,
        allows_disk_install,
        name,
        FrameLocation::TopOfSlot(slot),
    )
}

fn load_elf_at(
    image: &[u8],
    pid: usize,
    arguments: &[&[u8]],
    environment: &[&[u8]],
    allows_disk_install: bool,
    name: &str,
    location: FrameLocation,
) -> Result<Process, LoadError> {
    debug_log("elf: validating header\n");
    let elf = Elf64::parse(image).map_err(LoadError::InvalidElf)?;
    debug_log("elf: creating user page tables\n");
    let address_space = AddressSpace::new_user().ok_or(LoadError::OutOfMemory)?;

    for index in 0..elf.program_header_count() {
        let Some(segment) = elf.segment(index).map_err(LoadError::InvalidElf)? else {
            continue;
        };
        debug_log("elf: mapping LOAD segment\n");
        let segment_end = segment
            .virtual_address
            .checked_add(segment.memory_size)
            .ok_or(LoadError::InvalidMapping)?;
        let first_page = segment.virtual_address & !(PAGE_SIZE - 1);
        let last_page = segment_end
            .checked_add(PAGE_SIZE - 1)
            .ok_or(LoadError::InvalidMapping)?
            & !(PAGE_SIZE - 1);
        let mut page = first_page;
        while page < last_page {
            match address_space.map_user_page(page) {
                Ok(_) | Err(MapError::AlreadyMapped) => {}
                Err(_) => return Err(LoadError::OutOfMemory),
            }
            page += PAGE_SIZE;
        }

        zero_user_range(&address_space, segment.virtual_address, segment.memory_size)?;
        copy_to_user(
            &address_space,
            segment.virtual_address,
            elf.file_bytes(segment),
        )?;
    }

    let stack_start = USER_STACK_TOP
        .checked_sub(USER_STACK_SIZE)
        .ok_or(LoadError::InvalidMapping)?;
    let mut page = stack_start;
    while page < USER_STACK_TOP {
        address_space
            .map_user_page(page)
            .map_err(|_| LoadError::OutOfMemory)?;
        page += PAGE_SIZE;
    }
    let (user_stack, user_argv, user_envp) =
        prepare_user_stack(&address_space, arguments, environment)?;
    debug_log("elf: user stack mapped\n");
    if address_space.translate(elf.entry).is_none() {
        return Err(LoadError::InvalidMapping);
    }

    let frame = match location {
        FrameLocation::At(pointer) => pointer,
        FrameLocation::TopOfSlot(slot) => {
            let stacks = unsafe { &mut *KERNEL_STACKS.0.get() };
            let top = stacks.0[slot].as_mut_ptr() as u64 + KERNEL_STACK_SIZE as u64;
            (top as usize - size_of::<TrapFrame>()) as *mut TrapFrame
        }
    };
    unsafe {
        frame.write(TrapFrame {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            r11: 0,
            r10: 0,
            r9: 0,
            r8: 0,
            rbp: 0,
            rdx: environment.len() as u64,
            rcx: user_envp,
            rbx: 0,
            rax: 0,
            rip: elf.entry,
            cs: gdt::USER_CODE_SELECTOR,
            // IF = 1 so the iretq itself enables interrupts exactly when the CPU
            // lands in ring 3. This is the only safe place to arm the timer: the
            // kernel path that sets up this frame must stay uninterruptible.
            rflags: 0x202,
            rsp: user_stack,
            ss: gdt::USER_DATA_SELECTOR,
            rdi: arguments.len() as u64,
            rsi: user_argv,
        });
    }
    let mut process_name = [0; imagineos_abi::PROCESS_NAME_SIZE];
    let copied_name_length = name.len().min(process_name.len());
    process_name[..copied_name_length].copy_from_slice(&name.as_bytes()[..copied_name_length]);
    let mut fpu_state = FpuState::EMPTY;
    let _ = FpuState::_ALIGN_CHECK;
    unsafe {
        fpu::reset(fpu_state.as_mut_ptr());
    }
    Ok(Process {
        pid,
        active: true,
        stopped: false,
        parent: 0,
        zombie: false,
        exit_status: 0,
        waiting_for: 0,
        quantum: 0,
        allows_disk_install,
        uid: 0,
        gid: 0,
        is_admin: true,
        username: {
            let mut username = [0; imagineos_abi::ACCOUNT_NAME_SIZE];
            username[..4].copy_from_slice(b"root");
            username
        },
        username_length: 4,
        frame,
        address_space: Some(address_space),
        heap_end: USER_HEAP_START,
        name: process_name,
        pending_signals: 0,
        signal_actions: [imagineos_abi::SIGNAL_DEFAULT; 32],
        signal_restorer: 0,
        signal_frame: None,
        fpu_state,
    })
}

fn prepare_user_stack(
    space: &AddressSpace,
    arguments: &[&[u8]],
    environment: &[&[u8]],
) -> Result<(u64, u64, u64), LoadError> {
    if arguments.len() > MAX_EXEC_ARGS || environment.len() > MAX_EXEC_ENV {
        return Err(LoadError::InvalidMapping);
    }
    let mut stack_pointer = USER_STACK_TOP;
    let mut argument_pointers = [0u64; MAX_EXEC_ARGS];
    let mut environment_pointers = [0u64; MAX_EXEC_ENV];
    for (index, argument) in arguments.iter().enumerate() {
        stack_pointer = stack_pointer
            .checked_sub(argument.len() as u64 + 1)
            .ok_or(LoadError::InvalidMapping)?;
        copy_to_user(space, stack_pointer, argument)?;
        copy_to_user(space, stack_pointer + argument.len() as u64, &[0])?;
        argument_pointers[index] = stack_pointer;
    }
    for (index, entry) in environment.iter().enumerate() {
        stack_pointer = stack_pointer
            .checked_sub(entry.len() as u64 + 1)
            .ok_or(LoadError::InvalidMapping)?;
        copy_to_user(space, stack_pointer, entry)?;
        copy_to_user(space, stack_pointer + entry.len() as u64, &[0])?;
        environment_pointers[index] = stack_pointer;
    }

    let environment_vector_size = (environment.len() + 1)
        .checked_mul(size_of::<u64>())
        .ok_or(LoadError::InvalidMapping)? as u64;
    let environment_vector = stack_pointer
        .checked_sub(environment_vector_size)
        .ok_or(LoadError::InvalidMapping)?
        & !0xf;
    for (index, pointer) in environment_pointers
        .iter()
        .take(environment.len())
        .enumerate()
    {
        copy_to_user(
            space,
            environment_vector + (index * size_of::<u64>()) as u64,
            &pointer.to_le_bytes(),
        )?;
    }
    copy_to_user(
        space,
        environment_vector + (environment.len() * size_of::<u64>()) as u64,
        &0u64.to_le_bytes(),
    )?;

    let argument_vector_size = (arguments.len() + 1)
        .checked_mul(size_of::<u64>())
        .ok_or(LoadError::InvalidMapping)? as u64;
    stack_pointer = environment_vector
        .checked_sub(argument_vector_size)
        .ok_or(LoadError::InvalidMapping)?
        & !0xf;
    for (index, pointer) in argument_pointers.iter().take(arguments.len()).enumerate() {
        copy_to_user(
            space,
            stack_pointer + (index * size_of::<u64>()) as u64,
            &pointer.to_le_bytes(),
        )?;
    }
    copy_to_user(
        space,
        stack_pointer + (arguments.len() * size_of::<u64>()) as u64,
        &0u64.to_le_bytes(),
    )?;
    let entry_stack = stack_pointer
        .checked_sub(8)
        .ok_or(LoadError::InvalidMapping)?;
    Ok((entry_stack, stack_pointer, environment_vector))
}

fn zero_user_range(space: &AddressSpace, address: u64, size: u64) -> Result<(), LoadError> {
    let mut current = address;
    let end = address.checked_add(size).ok_or(LoadError::InvalidMapping)?;
    while current < end {
        let destination = space.translate(current).ok_or(LoadError::InvalidMapping)?;
        let available = (PAGE_SIZE - (current & (PAGE_SIZE - 1))).min(end - current);
        unsafe {
            ptr::write_bytes(destination, 0, available as usize);
        }
        current += available;
    }
    Ok(())
}

fn copy_to_user(space: &AddressSpace, address: u64, bytes: &[u8]) -> Result<(), LoadError> {
    let mut copied = 0usize;
    while copied < bytes.len() {
        let current = address
            .checked_add(copied as u64)
            .ok_or(LoadError::InvalidMapping)?;
        let destination = space.translate(current).ok_or(LoadError::InvalidMapping)?;
        let available =
            ((PAGE_SIZE - (current & (PAGE_SIZE - 1))) as usize).min(bytes.len() - copied);
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr().add(copied), destination, available);
        }
        copied += available;
    }
    Ok(())
}

pub fn start() -> ! {
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let process = scheduler.processes[0];
    scheduler.current = 0;
    let address_space = process.address_space.expect("process address space");
    let kernel_stack_top =
        unsafe { (*KERNEL_STACKS.0.get()).0[0].as_ptr() as u64 + KERNEL_STACK_SIZE as u64 };
    gdt::set_kernel_stack(kernel_stack_top);
    paging::switch(address_space);
    unsafe {
        asm!(
            "mov rsp, {frame}",
            "mov ax, 0x33",
            "mov ds, ax",
            "mov es, ax",
            "pop r15", "pop r14", "pop r13", "pop r12", "pop r11",
            "pop r10", "pop r9", "pop r8", "pop rbp", "pop rdi",
            "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
            // The saved RFLAGS has IF=1, so this iretq atomically enables
            // interrupts while entering ring 3, with a user frame already on the
            // stack for the timer handler to preempt.
            "iretq",
            frame = in(reg) process.frame,
            options(noreturn)
        );
    }
}

pub fn current_pid() -> usize {
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    scheduler.processes[scheduler.current].pid
}

/// Salva o estado FPU/SSE do processo atualmente em execução.
///
/// Deve ser chamada ao entrar no kernel (syscall/exceção), antes que código
/// Rust do kernel possa usar registradores XMM.
pub fn save_current_fpu() {
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    unsafe {
        fpu::save(scheduler.processes[scheduler.current].fpu_state.as_mut_ptr());
    }
}

/// Restaura o estado FPU/SSE do processo que vai retomar a execução.
///
/// Deve ser chamada imediatamente antes de retornar para o espaço de usuário,
/// usando o ponteiro de `TrapFrame` que será retomado.
pub fn restore_fpu_for(frame: *mut TrapFrame) {
    if frame.is_null() {
        return;
    }
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let slot = scheduler.current;
    if scheduler.processes[slot].frame == frame {
        unsafe {
            fpu::restore(scheduler.processes[slot].fpu_state.as_ptr());
        }
    }
}

pub fn current_credentials() -> (u32, u32, bool) {
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    let process = scheduler.processes[scheduler.current];
    (process.uid, process.gid, process.is_admin)
}

pub fn yield_current(frame: *mut TrapFrame) -> *mut TrapFrame {
    schedule(frame, false)
}

/// Preempts the running process in response to a timer tick.
///
/// Called from the IRQ0 handler with the interrupt frame of the process that was
/// interrupted. When another runnable process exists, this switches address
/// spaces, kernel stack and FPU/SSE state (via [`schedule`]) and returns the
/// frame of the process that should resume. When nothing else is runnable it
/// returns `frame` unchanged, so the current process continues without noticing
/// the tick beyond the lost time slice.
pub fn timer_tick(frame: *mut TrapFrame) -> *mut TrapFrame {
    if frame.is_null() {
        return frame;
    }
    // Só preempta processos que estavam em **ring 3**.
    //
    // Um tick que interrompe o kernel (uma syscall em andamento, `cs & 3 == 0`)
    // não deve trocar de contexto: a syscall ficaria pendurada no meio, com a
    // pilha de kernel daquele processo "congelada" em um ponto arbitrário. Era
    // isto que impedia o `fork` de funcionar com o timer ligado — o filho era
    // preemptado dentro da própria largada e retomava sempre do mesmo `rip`,
    // sem nunca executar código de usuário.
    unsafe {
        let interrupted = &*frame;
        if interrupted.cs & 3 != 3 {
            return frame;
        }
    }
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    let current = scheduler.current;
    if !scheduler.processes[current].active {
        return frame;
    }
    // Enquanto houver alguém bloqueado em `waitpid`, não pede troca: o processo
    // que está rodando precisa consumir o tempo para produzir o evento que
    // destrava quem espera (um `exit` de filho, por exemplo). Sem isto, dois
    // `waitpid` concorrentes reescalavam um ao outro a cada tick e nenhum dos
    // dois progredia.
    if any_waiting() {
        return frame;
    }
    let has_other_runnable = scheduler
        .processes
        .iter()
        .enumerate()
        .any(|(index, process)| {
            index != current
                && process.active
                && !process.stopped
                && !process.zombie
                && process.waiting_for == 0
        });
    if !has_other_runnable {
        return frame;
    }

    // Quantum **por processo**: só troca depois de `TIMER_QUANTUM_TICKS` ticks
    // deste processo. Preemptar a cada tick (10 ms) custa caro — cada troca
    // salva e restaura o estado FPU e recarrega o CR3 — e deixava pouco tempo
    // útil para o processo em execução, a ponto de um filho recém-criado por
    // `fork` não conseguir executar sua primeira instrução.
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    scheduler.processes[current].quantum += 1;
    if scheduler.processes[current].quantum < crate::time::TIMER_QUANTUM_TICKS {
        return frame;
    }
    scheduler.processes[current].quantum = 0;
    schedule(frame, false)
}

/// Indica se algum processo está bloqueado em `waitpid`.
///
/// O `waitpid` bloqueia o pai e o `schedule` escolhe o filho. Mas quando **o
/// próprio filho** também espera (por um neto) e o avô espera o filho, o
/// escalonador pode ficar num ciclo em que cada `waitpid` recém-retomado
/// reescala imediatamente, sem deixar o processo em execução consumir o seu
/// quantum. Enquanto houver alguém bloqueado, o timer deixa de pedir troca,
/// para que quem estiver rodando progrida até o evento que destrava os demais.
pub fn any_waiting() -> bool {
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    scheduler
        .processes
        .iter()
        .any(|process| process.active && process.waiting_for != 0)
}

/// Encerra o processo atual com `status`, registrando o código para que o pai
/// possa coletá-lo em `waitpid`.
///
/// O slot vira um *zumbi*: `active` continua verdadeiro e `exit_status` fica
/// guardado até o pai chamar `waitpid`. Só então o slot é realmente liberado.
/// Isto é o comportamento do UNIX e o que permite que `$?` funcione depois que
/// o filho já morreu.
pub fn exit_current(frame: *mut TrapFrame, status: i32) -> *mut TrapFrame {
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    scheduler.processes[scheduler.current].exit_status = status;
    schedule(frame, true)
}

pub fn terminate_faulting_process() -> *mut TrapFrame {
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    let frame = scheduler.processes[scheduler.current].frame;
    if frame.is_null() {
        crate::kernel_halt();
    }
    schedule(frame, true)
}

pub fn send_signal(pid: usize, signal: u64) -> i64 {
    if !valid_signal(signal) {
        return -22;
    }
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let sender_uid = scheduler.processes[scheduler.current].uid;
    let Some(process) = scheduler
        .processes
        .iter_mut()
        .find(|process| process.active && process.pid == pid)
    else {
        return -3;
    };
    if sender_uid != 0 && sender_uid != process.uid {
        return -1;
    }
    if process.pid == 1 {
        return -1;
    }
    process.pending_signals |= 1u64 << signal;
    if signal == imagineos_abi::SIGNAL_CONT || signal == imagineos_abi::SIGNAL_KILL {
        process.stopped = false;
    }
    0
}

pub fn set_signal_action(signal: u64, handler: u64, restorer: u64) -> i64 {
    if !valid_signal(signal)
        || signal == imagineos_abi::SIGNAL_KILL
        || signal == imagineos_abi::SIGNAL_STOP
        || (handler != imagineos_abi::SIGNAL_DEFAULT
            && handler != imagineos_abi::SIGNAL_IGNORE
            && (restorer == 0
                || with_current_space(|space| {
                    space.translate(handler).is_none() || space.translate(restorer).is_none()
                })))
    {
        return -22;
    }
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let process = &mut scheduler.processes[scheduler.current];
    process.signal_actions[signal as usize] = handler;
    if handler > imagineos_abi::SIGNAL_IGNORE {
        process.signal_restorer = restorer;
    }
    0
}

pub fn restore_signal_context(frame: *mut TrapFrame) -> bool {
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let process = &mut scheduler.processes[scheduler.current];
    let Some(saved) = process.signal_frame.take() else {
        return false;
    };
    unsafe {
        frame.write(saved);
    }
    true
}

pub fn deliver_pending_signal(frame: *mut TrapFrame) -> *mut TrapFrame {
    if frame.is_null() {
        return frame;
    }
    loop {
        let scheduler = unsafe { &mut *SCHEDULER.0.get() };
        let slot = scheduler.current;
        let process = &mut scheduler.processes[slot];
        if !process.active {
            return frame;
        }
        let Some(signal) = next_pending_signal(process.pending_signals) else {
            return frame;
        };
        if process.signal_frame.is_some()
            && signal != imagineos_abi::SIGNAL_KILL
            && signal != imagineos_abi::SIGNAL_STOP
        {
            return frame;
        }
        process.pending_signals &= !(1u64 << signal);
        let handler = process.signal_actions[signal as usize];
        match handler {
            imagineos_abi::SIGNAL_IGNORE
                if signal != imagineos_abi::SIGNAL_KILL && signal != imagineos_abi::SIGNAL_STOP =>
            {
                continue;
            }
            _ if signal == imagineos_abi::SIGNAL_KILL
                || (handler == imagineos_abi::SIGNAL_DEFAULT && is_terminating_signal(signal)) =>
            {
                return schedule(frame, true);
            }
            _ if signal == imagineos_abi::SIGNAL_STOP => {
                process.stopped = true;
                return schedule(frame, false);
            }
            _ if signal == imagineos_abi::SIGNAL_CONT
                && handler <= imagineos_abi::SIGNAL_IGNORE =>
            {
                continue;
            }
            _ if handler > imagineos_abi::SIGNAL_IGNORE => {
                let Some(space) = process.address_space else {
                    return schedule(frame, true);
                };
                let interrupted = unsafe { *frame };
                // Enter the handler with the SysV stack alignment and a return stub.
                let stack_top = interrupted.rsp & !0xf;
                let Some(stack_slot) = stack_top.checked_sub(8) else {
                    return schedule(frame, true);
                };
                let Some(destination) = space.translate(stack_slot) else {
                    return schedule(frame, true);
                };
                unsafe {
                    ptr::write_unaligned(destination.cast::<u64>(), process.signal_restorer);
                }
                process.signal_frame = Some(interrupted);
                unsafe {
                    (*frame).rip = handler;
                    (*frame).rdi = signal;
                    (*frame).rsp = stack_slot;
                }
                return frame;
            }
            _ => continue,
        }
    }
}

pub fn snapshot() -> ([imagineos_abi::ProcessInfo; MAX_PROCESSES], usize) {
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    let mut result = [imagineos_abi::ProcessInfo::default(); MAX_PROCESSES];
    let mut count = 0;
    for process in scheduler.processes.iter().filter(|process| process.active) {
        result[count] = imagineos_abi::ProcessInfo {
            pid: process.pid as u64,
            state: if process.stopped {
                imagineos_abi::PROCESS_STOPPED
            } else {
                imagineos_abi::PROCESS_RUNNING
            },
            pending_signals: process.pending_signals as u32,
            name: process.name,
        };
        count += 1;
    }
    (result, count)
}

/// `schedule` que também informa se houve troca de processo.
///
/// `switched == false` só acontece quando o próprio processo atual foi o
/// escolhido (nenhum outro estava elegível). Nesse caso quem chamou continua
/// rodando e pode reavaliar a condição que o levou a ceder.
fn schedule_ex(frame: *mut TrapFrame, exiting: bool) -> (*mut TrapFrame, bool) {
    let before = unsafe { (*SCHEDULER.0.get()).current };
    let next = schedule(frame, exiting);
    let after = unsafe { (*SCHEDULER.0.get()).current };
    (next, before != after)
}

fn schedule(frame: *mut TrapFrame, exiting: bool) -> *mut TrapFrame {
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let current = scheduler.current;
    scheduler.processes[current].frame = frame;
    // Preserva os registradores FPU/SSE do processo que está saindo da CPU,
    // antes de qualquer coisa que possa tocar em XMM (o próprio kernel Rust).
    unsafe {
        fpu::save(scheduler.processes[current].fpu_state.as_mut_ptr());
    }
    if exiting {
        // O processo deixa de rodar, mas o slot fica reservado como zumbi até o
        // pai coletar o status com `waitpid`. Sem isto o código de saída se
        // perderia (e `$?` não funcionaria).
        scheduler.processes[current].zombie = true;
        scheduler.processes[current].stopped = false;
        scheduler.processes[current].waiting_for = 0;
        // Fecha os descritores do processo que terminou. Sem isto as pontas de
        // pipe entregues a um filho continuavam contadas para sempre, e um
        // leitor jamais veria EOF (`cat` girava em EAGAIN indefinidamente em
        // `ls | cat`). Também evita vazar os descritores do slot reutilizado.
        release_descriptors(current);
        // Libera superfícies registradas por quem morre: um painel/terminal
        // externo que termine não pode deixar uma janela fantasma no
        // compositor (o snapshot checa o dono, e aqui o slot já está vazio).
        let finished_pid = scheduler.processes[current].pid;
        crate::surface::release_pid(finished_pid);
        // Se o processo que terminou era o dono da tela (servidor de display),
        // devolve o console: sem isto o último frame desenhado pelo WM ficava
        // congelado na VGA, pois o console tinha o desenho desativado e nenhum
        // novo processo redicionava a tela. Limpar dá ao shell um console em
        // branco, e a entrada volta para o caminho de teclado do kernel.
        if crate::input::is_captured() {
            crate::input::release();
            crate::framebuffer::set_active(true);
            crate::framebuffer::clear();
        }
        // Um processo que esperava por este filho pode voltar a rodar.
        let finished = scheduler.processes[current].pid;
        for index in 0..MAX_PROCESSES {
            if scheduler.processes[index].waiting_for == finished {
                scheduler.processes[index].waiting_for = 0;
            }
        }
    }

    for distance in 1..=MAX_PROCESSES {
        let candidate = (current + distance) % MAX_PROCESSES;
        // Um zumbi não é escalonável (espera ser coletado), e um processo
        // bloqueado em `waitpid` também não: se ele fosse escolhido, gastaria o
        // seu quantum reavaliando o mesmo `waitpid`, e o filho que ele espera
        // nunca seria escalonado no meio disso. O `waitpid` limpa `waiting_for`
        // quando o alvo termina, então o pai volta a ser elegível nesse ponto.
        if scheduler.processes[candidate].active
            && !scheduler.processes[candidate].stopped
            && !scheduler.processes[candidate].zombie
            && scheduler.processes[candidate].waiting_for == 0
        {
            scheduler.current = candidate;
            let next = scheduler.processes[candidate];
            let address_space = next.address_space.expect("process address space");
            let kernel_stack_top = unsafe {
                (*KERNEL_STACKS.0.get()).0[candidate].as_ptr() as u64 + KERNEL_STACK_SIZE as u64
            };
            gdt::set_kernel_stack(kernel_stack_top);
            paging::switch(address_space);
            // Restaura o estado FPU/SSE do processo que vai entrar na CPU.
            unsafe {
                fpu::restore(scheduler.processes[candidate].fpu_state.as_ptr());
            }
            return next.frame;
        }
    }

    crate::kernel_halt()
}

pub fn spawn_current(
    path: &str,
    arguments: &[&[u8]],
    environment: &[&[u8]],
    frame: *mut TrapFrame,
) -> *mut TrapFrame {
    // Plain `exec` keeps the default stdio: it is the historical behaviour and
    // what init/getty rely on.
    spawn_current_with_io(path, arguments, environment, &imagineos_abi::SpawnIo::default(), frame)
}

/// `execve(2)`: substitui a imagem do processo atual.
///
/// O `pid` e o slot são preservados, mas memória, pilha e frame de execução são
/// trocados pelo programa em `path`. Os descritores **não** são modificados
/// (o shell já instalou as pontas de pipe antes de chamar `execve`).
///
/// Só retorna em caso de erro: em sucesso, o `TrapFrame` volta apontando para a
/// entrada do novo programa e o processo continua a execução do zero, agora em
/// ring 3. Este é o par que falta para `fork` + `exec` virar um `exec` POSIX de
/// verdade.
pub fn execve_current(
    path: &str,
    arguments: &[&[u8]],
    environment: &[&[u8]],
    frame: *mut TrapFrame,
) -> *mut TrapFrame {
    if !has_access(path, 1) {
        unsafe {
            (*frame).rax = (-13i64) as u64;
        }
        return frame;
    }
    let Some(image) = crate::ramfs::read(path) else {
        unsafe {
            (*frame).rax = (-2i64) as u64;
        }
        return frame;
    };

    let (slot, pid, process) = {
        let scheduler = unsafe { &*SCHEDULER.0.get() };
        let slot = scheduler.current;
        let process = scheduler.processes[slot];
        (slot, process.pid, process)
    };

    // `load_elf` monta o novo `TrapFrame` no **topo** da pilha de kernel do
    // slot. Isso colide com o frame do `int 0x80` que está ativo agora, porque
    // o `execve` roda no mesmo slot do processo que vai ser substituído
    // (diferente do `spawn`, que usa outro slot). Escrevemos o frame novo na
    // **base** da pilha de kernel, longe o bastante do topo para não sobrepor
    // o frame ativo do stub de syscall.
    let Some(execve_frame) = execve_frame_scratch(slot) else {
        unsafe {
            (*frame).rax = (-12i64) as u64;
        }
        return frame;
    };
    let replacement = match load_elf_at(
        image,
        pid,
        arguments,
        environment,
        path == "/bin/distroinstall",
        path,
        FrameLocation::At(execve_frame),
    ) {
        Ok(process) => process,
        Err(_) => {
            unsafe {
                (*frame).rax = (-8i64) as u64;
            }
            return frame;
        }
    };

    // Preserva identidade e credenciais; só a imagem muda.
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let mut replacement = replacement;
    replacement.parent = process.parent;
    replacement.uid = process.uid;
    replacement.gid = process.gid;
    replacement.is_admin = process.is_admin;
    replacement.username = process.username;
    replacement.username_length = process.username_length;
    replacement.allows_disk_install = process.allows_disk_install;
    replacement.signal_actions = process.signal_actions;
    replacement.signal_restorer = process.signal_restorer;
    let new_frame = replacement.frame;
    let new_space = replacement.address_space.expect("execve address space");
    scheduler.processes[slot] = replacement;
    // Troca o espaço de endereço **agora**: o `iretq` do stub volta direto para
    // o ponto de entrada do novo programa, sem passar pelo `schedule`. Sem
    // esta troca a CPU continuaria no espaço antigo, onde o novo programa não
    // está mapeado — o processo girava falhando instruções sem nunca chamar
    // uma syscall.
    // O frame do novo programa precisa ficar registrado no processo: se o
    // `execve` for seguido de outro `schedule` (preempção imediata, por
    // exemplo), o escalonador restauraria o frame antigo e o processo voltaria
    // a executar o código anterior — do lado de fora isso parecia um `ls`
    // girando sem nunca chamar syscall.
    scheduler.processes[slot].frame = new_frame;
    paging::switch(new_space);
    let _ = pid;
    new_frame
}

/// Creates a new process, optionally rewiring its standard descriptors.
///
/// `io` holds descriptor numbers owned by the caller: each is duplicated into
/// the child (stdin/stdout/stderr respectively), so a shell can hand a pipe end
/// to a child without a real `fork`. Fields set to
/// [`imagineos_abi::SpawnIo::INHERIT`] keep the caller's descriptor.
pub fn spawn_current_with_io(
    path: &str,
    arguments: &[&[u8]],
    environment: &[&[u8]],
    io: &imagineos_abi::SpawnIo,
    frame: *mut TrapFrame,
) -> *mut TrapFrame {
    if !has_access(path, 1) {
        unsafe {
            (*frame).rax = (-13i64) as u64;
        }
        return frame;
    }
    let Some(image) = crate::ramfs::read(path) else {
        unsafe {
            (*frame).rax = (-2i64) as u64;
        }
        return frame;
    };

    let (slot, pid, parent_slot) = {
        let scheduler = unsafe { &*SCHEDULER.0.get() };
        let Some(slot) = scheduler
            .processes
            .iter()
            .position(|process| !process.active)
        else {
            unsafe {
                (*frame).rax = (-11i64) as u64;
            }
            return frame;
        };
        let pid = scheduler
            .processes
            .iter()
            .map(|process| process.pid)
            .max()
            .unwrap_or(0)
            + 1;
        (slot, pid, scheduler.current)
    };

    // O pai é quem chamou o `spawn`, para que `waitpid` funcione no shell.
    let parent_pid = {
        let scheduler = unsafe { &*SCHEDULER.0.get() };
        scheduler.processes[parent_slot].pid
    };
    let process = match load_elf(
        image,
        pid,
        slot,
        arguments,
        environment,
        path == "/bin/distroinstall",
        path,
    ) {
        Ok(process) => process,
        Err(_) => {
            unsafe {
                (*frame).rax = (-8i64) as u64;
            }
            return frame;
        }
    };
    // O filho começa com stdio padrão e recebe as redireções pedidas em `io`.
    //
    // Deliberadamente *não* herdamos o resto da tabela de descritores do pai:
    // herdar fds altos fazia cada filho segurar referências extras de pipe
    // (uma cópia em fd alto + a instalada em stdio), e a contagem nunca chegava
    // a zero quando o produtor terminava — daí `ls | cat` ficar preso em
    // EAGAIN. Fechar tudo além de 0/1/2 no `spawn` é o comportamento POSIX
    // (close-on-exec) e torna a contagem de pontas de pipe previsível.
    let tables = unsafe { &mut *DESCRIPTOR_TABLES.0.get() };
    // Copia a tabela do pai: `apply_spawn_io` precisa ler os descritores dela
    // enquanto escreve na do filho, e as duas vivem no mesmo array.
    let parent_table = tables[parent_slot];
    tables[slot] = DescriptorTable::new();
    apply_spawn_io(&mut tables[slot], io, &parent_table);
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let parent = scheduler.processes[parent_slot];
    let mut process = process;
    process.uid = parent.uid;
    process.gid = parent.gid;
    process.is_admin = parent.is_admin;
    process.username = parent.username;
    process.username_length = parent.username_length;
    process.parent = parent_pid;
    scheduler.processes[slot] = process;
    scheduler.count = scheduler.count.max(slot + 1);
    unsafe {
        (*frame).rax = pid as u64;
    }
    schedule(frame, false)
}

/// `fork(2)`: cria um processo que é uma cópia do atual.
///
/// O filho recebe:
///
/// * uma **cópia** do espaço de endereço (sem copy-on-write: cada página é
///   duplicada em um frame novo);
/// * uma cópia do `TrapFrame`, com `rax = 0` — é assim que o filho distingue-se
///   do pai, que vê o pid do filho em `rax`;
/// * uma cópia da tabela de descritores, com as referências de pipe contadas;
/// * as mesmas credenciais e o registro do pai para `waitpid`.
///
/// Devolve o frame a retomar. O escalonador continua no pai (o filho entra na
/// fila e rodará no próximo `yield`), então o valor de retorno é o frame do
/// próprio pai com `rax` ajustado.
pub fn fork_current(frame: *mut TrapFrame) -> *mut TrapFrame {
    if frame.is_null() {
        return frame;
    }
    let (slot, pid, parent_slot, parent_pid) = {
        let scheduler = unsafe { &*SCHEDULER.0.get() };
        let Some(slot) = scheduler.processes.iter().position(|process| !process.active) else {
            unsafe {
                (*frame).rax = (-11i64) as u64; // EAGAIN
            }
            return frame;
        };
        let pid = scheduler
            .processes
            .iter()
            .map(|process| process.pid)
            .max()
            .unwrap_or(0)
            + 1;
        let parent = scheduler.processes[scheduler.current];
        (slot, pid, scheduler.current, parent.pid)
    };

    let (parent_space, parent_frame) = {
        let scheduler = unsafe { &*SCHEDULER.0.get() };
        let parent = scheduler.processes[parent_slot];
        (parent.address_space, unsafe { *frame })
    };
    let Some(parent_space) = parent_space else {
        unsafe {
            (*frame).rax = (-12i64) as u64; // ENOMEM
        }
        return frame;
    };
    let Some(child_space) = parent_space.clone_user() else {
        unsafe {
            (*frame).rax = (-12i64) as u64; // ENOMEM
        }
        return frame;
    };

    // O filho retoma exatamente onde o pai estava, mas com `rax = 0`.
    let stacks = unsafe { &mut *KERNEL_STACKS.0.get() };
    let kernel_stack_top = stacks.0[slot].as_mut_ptr() as u64 + KERNEL_STACK_SIZE as u64;
    let child_frame = (kernel_stack_top as usize - size_of::<TrapFrame>()) as *mut TrapFrame;
    unsafe {
        child_frame.write(TrapFrame {
            rax: 0,
            ..parent_frame
        });
    }


    // Cópia da tabela de descritores: cada ponta de pipe compartilhada ganha
    // mais uma referência, exatamente como `dup` faria em cada fd.
    let tables = unsafe { &mut *DESCRIPTOR_TABLES.0.get() };
    let parent_table = tables[parent_slot];
    tables[slot] = parent_table;
    for descriptor in tables[slot].0.iter() {
        match descriptor.kind {
            DescriptorKind::PipeRead => {
                let _ = crate::ipc::add_reader(descriptor.pipe);
            }
            DescriptorKind::PipeWrite => {
                let _ = crate::ipc::add_writer(descriptor.pipe);
            }
            _ => {}
        }
    }

    let mut fpu_state = FpuState::EMPTY;
    unsafe {
        fpu::reset(fpu_state.as_mut_ptr());
    }

    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let parent = scheduler.processes[parent_slot];
    scheduler.processes[slot] = Process {
        pid,
        active: true,
        stopped: false,
        parent: parent_pid,
        zombie: false,
        exit_status: 0,
        waiting_for: 0,
        quantum: 0,
        allows_disk_install: parent.allows_disk_install,
        uid: parent.uid,
        gid: parent.gid,
        is_admin: parent.is_admin,
        username: parent.username,
        username_length: parent.username_length,
        frame: child_frame,
        address_space: Some(child_space),
        heap_end: parent.heap_end,
        name: parent.name,
        pending_signals: 0,
        signal_actions: parent.signal_actions,
        signal_restorer: parent.signal_restorer,
        signal_frame: None,
        fpu_state,
    };
    scheduler.count = scheduler.count.max(slot + 1);
    // O filho começa com o quantum cheio (o campo já é inicializado em 0),
    // garantindo que ele execute antes de o timer pedir a troca.
    scheduler.processes[slot].quantum = 0;
    unsafe {
        (*frame).rax = pid as u64;
    }
    frame
}

/// `waitpid(pid, &status, flags)`: espera um filho terminar e coleta o status.
///
/// Comportamento implementado:
///
/// * `pid > 0` — espera especificamente aquele filho;
/// * `pid < 0` — espera qualquer filho;
/// * `pid == 0` — igual a `-1` (só filhos do mesmo processo).
///
/// Devolve o `pid` coletado e escreve `status << 8` no ponteiro do usuário,
/// como o POSIX (o código de saída ocupa os 8 bits altos). Enquanto o filho não
/// termina, o kernel marca `waiting_for` e cede a CPU — sem busy-wait, o pai só
/// volta a rodar quando o filho morre (ou por preempção normal).
///
/// Se não houver filhos correspondentes, devolve `-ECHILD` (`-10`).
/// `waitpid(pid, &status, options)`: espera um filho e coleta o status.
///
/// Suporta a flag POSIX `WNOHANG`: sem ela, o comportamento mantém o ciclo
/// EAGAIN + repetição já validado; com ela, devolve **0** (pid 0) imediatamente
/// quando nenhum filho terminou, sem ceder a CPU — o padrão que supervisores
/// de serviços (Horust) e shells de trabalho usam para varrer filhos.
pub fn waitpid_current(frame: *mut TrapFrame, pid: i64, status_pointer: u64, options: i64) -> *mut TrapFrame {
    // O slot do pai é capturado **uma vez**. Usar `scheduler.current` a cada
    // volta do laço era o bug: depois de `schedule` trocar de processo, o
    // `current` apontava para outro processo, e `waiting_for` era escrito nele.
    // Isso marcava o init e o próprio filho como "esperando pelo pid 3",
    // deixando todos inelegíveis e travando o escalonador.
    let (parent_slot, parent_pid, wanted) = {
        let scheduler = unsafe { &*SCHEDULER.0.get() };
        let parent_slot = scheduler.current;
        (parent_slot, scheduler.processes[parent_slot].pid, pid)
    };

    // Primeiro procura um corpo já pronto (zumbi) para não bloquear sem motivo.
    loop {
        let (reaped, found_any) = reap_child(parent_pid, wanted, status_pointer);
        if let Some(child_pid) = reaped {
            unsafe {
                (*frame).rax = child_pid as u64;
            }
            return frame;
        }
        if !found_any {
            // Nenhum filho corresponde ao pedido.
            unsafe {
                (*frame).rax = (-10i64) as u64; // ECHILD
            }
            return frame;
        }

        // WNOHANG: nunca bloqueia. Com filho vivo e nenhum terminado, o
        // resultado é 0 ("pid 0") conforme POSIX, sem yield.
        if options & imagineos_abi::WNOHANG != 0 {
            unsafe {
                (*frame).rax = 0;
            }
            return frame;
        }

        // Há filho vivo: marca quem esperamos e cede a CPU. O `schedule`
        // remove `waiting_for` quando o filho termina, e este laço é
        // reavaliado quando o pai for escalonado de novo. A marcação vai sempre
        // no slot do pai, capturado no início.
        {
            let scheduler = unsafe { &mut *SCHEDULER.0.get() };
            // `wanted <= 0`: espera qualquer filho; `usize::MAX` sinaliza isso e
            // o `schedule` limpa o campo quando qualquer filho termina.
            scheduler.processes[parent_slot].waiting_for = if wanted > 0 {
                wanted as usize
            } else {
                usize::MAX
            };
        }
        // O filho ainda está vivo. Devolve `EAGAIN` ao userspace e **deixa o
        // pai continuar rodando** (sem trocar de contexto aqui).
        //
        // Trocar de processo dentro do `waitpid` não funciona: o stub do
        // `int 0x80` faz `iretq` para o frame devolvido, o userspace retoma
        // logo **após** o `int 0x80` — ou seja, o laço do `waitpid` no kernel
        // seria abandonado com a pilha de kernel suja, e o pai acharia que o
        // `waitpid` terminou sem ter coletado o filho (foi assim que os zumbis
        // se acumularam até esgotar os slots).
        //
        // O caminho correto é do POSIX: o pai repete a chamada. O `yield`
        // abaixo cede a CPU para o filho progredir, e o usuário chama de novo.
        unsafe {
            (*frame).rax = (-11i64) as u64; // EAGAIN
        }
        let (next, _) = schedule_ex(frame, false);
        return next;
    }
}

/// Procura um filho de `parent_pid` (zumbi ou não) que satisfaça `wanted`.
///
/// Devolve `(pid_coletado, existe_algum_filho)`.
fn reap_child(parent_pid: usize, wanted: i64, status_pointer: u64) -> (Option<usize>, bool) {
    let mut found_any = false;
    let mut collected: Option<(usize, i32)> = None;
    {
        let scheduler = unsafe { &*SCHEDULER.0.get() };
        for process in scheduler.processes.iter() {
            if !process.active || process.parent != parent_pid {
                continue;
            }
            let matches = match wanted {
                value if value > 0 => process.pid == value as usize,
                // Qualquer filho (o POSIX distingue grupos de processo em
                // `pid == 0`, que este kernel ainda não modela).
                _ => true,
            };
            if !matches {
                continue;
            }
            found_any = true;
            if process.zombie && collected.is_none() {
                collected = Some((process.pid, process.exit_status));
            }
        }
    }


    let Some((child_pid, exit_status)) = collected else {
        return (None, found_any);
    };

    // Escreve o status no formato POSIX: código nos 8 bits altos.
    let status_value = ((exit_status as u32) & 0xff) << 8;
    if status_pointer != 0 && !copy_to_current_user(status_pointer, &status_value.to_le_bytes()) {
        // Falha ao escrever o status (ponteiro inválido): não coleta o filho,
        // para que o código de saída não se perca. O chamador recebe EFAULT.
        return (None, found_any);
    }

    // Libera o slot do zumbi.
    {
        let scheduler = unsafe { &mut *SCHEDULER.0.get() };
        if let Some(slot) = scheduler
            .processes
            .iter()
            .position(|process| process.active && process.pid == child_pid)
        {
            scheduler.processes[slot] = Process::EMPTY;
        }
    }
    (Some(child_pid), found_any)
}

/// Instala em `child` (slots 0/1/2) os descritores de `parent` indicados em
/// `io`. Um campo `INHERIT` deixa o slot no valor padrão.
///
/// Cada ponta de pipe instalada ganha **uma** referência na contagem, que é
/// liberada quando o processo termina ou fecha o descritor.
fn apply_spawn_io(
    child: &mut DescriptorTable,
    io: &imagineos_abi::SpawnIo,
    parent: &DescriptorTable,
) {
    let slots = [
        (0usize, io.stdin_fd),
        (1usize, io.stdout_fd),
        (2usize, io.stderr_fd),
    ];
    for (target, source) in slots {
        if source == imagineos_abi::SpawnIo::INHERIT {
            continue;
        }
        let Ok(source) = usize::try_from(source) else {
            continue;
        };
        if source >= MAX_OPEN_FDS {
            continue;
        }
        let descriptor = parent.0[source];
        if descriptor.kind == DescriptorKind::Closed {
            continue;
        }
        match descriptor.kind {
            DescriptorKind::PipeRead => {
                let _ = crate::ipc::add_reader(descriptor.pipe);
            }
            DescriptorKind::PipeWrite => {
                let _ = crate::ipc::add_writer(descriptor.pipe);
            }
            _ => {}
        }
        child.0[target] = descriptor;
    }
}

fn has_access(path: &str, requested: u16) -> bool {
    let (uid, gid, _) = current_credentials();
    if uid == 0 {
        return true;
    }
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    let mut prefix = [0u8; imagineos_abi::MAX_MUTABLE_PATH];
    let mut length = 0usize;
    for component in parent.trim_matches('/').split('/') {
        if component.is_empty() {
            continue;
        }
        if length != 0 {
            prefix[length] = b'/';
            length += 1;
        }
        if length + component.len() > prefix.len() {
            return false;
        }
        prefix[length..length + component.len()].copy_from_slice(component.as_bytes());
        length += component.len();
        let directory = core::str::from_utf8(&prefix[..length]).unwrap_or("");
        let Some(metadata) = crate::ramfs::metadata(directory) else {
            return false;
        };
        if !metadata.is_directory
            || !crate::permissions::allows(metadata.mode, metadata.uid, metadata.gid, uid, gid, 1)
        {
            return false;
        }
    }
    let Some(metadata) = crate::ramfs::metadata(path) else {
        return false;
    };
    crate::permissions::allows(
        metadata.mode,
        metadata.uid,
        metadata.gid,
        uid,
        gid,
        requested,
    )
}

pub fn authenticate_current(username: &[u8], password: &[u8], target_uid: u32) -> i64 {
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let current = &mut scheduler.processes[scheduler.current];
    let is_self = username == &current.username[..current.username_length];
    if target_uid == 0 && is_self && current.uid != 0 {
        // sudo authenticates the caller, then elevates only an explicitly enabled account.
        if !current.is_admin || crate::accounts::authenticate(username, password).is_none() {
            return -1;
        }
        current.uid = 0;
        current.gid = 0;
        current.is_admin = true;
        current.username = [0; imagineos_abi::ACCOUNT_NAME_SIZE];
        current.username[..4].copy_from_slice(b"root");
        current.username_length = 4;
        return 0;
    }
    let Some(account) = crate::accounts::authenticate(username, password) else {
        return -1;
    };
    if target_uid != u32::MAX && account.uid != target_uid {
        return -1;
    }
    current.uid = account.uid;
    current.gid = account.gid;
    current.is_admin = account.administrator;
    current.username = account.username;
    current.username_length = account.username_length;
    0
}

pub fn current_identity() -> imagineos_abi::UserIdentity {
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    let process = scheduler.processes[scheduler.current];
    let mut identity = imagineos_abi::UserIdentity {
        uid: process.uid,
        gid: process.gid,
        is_admin: u32::from(process.is_admin),
        username_length: process.username_length as u32,
        username: process.username,
        ..imagineos_abi::UserIdentity::default()
    };
    if let Some(hostname) = crate::ramfs::read("/etc/hostname") {
        let hostname = hostname.strip_suffix(b"\n").unwrap_or(hostname);
        let length = hostname.len().min(identity.hostname.len());
        identity.hostname[..length].copy_from_slice(&hostname[..length]);
        identity.hostname_length = length as u32;
    } else {
        identity.hostname[..9].copy_from_slice(b"imagineos");
        identity.hostname_length = 9;
    }
    identity
}

fn valid_signal(signal: u64) -> bool {
    matches!(
        signal,
        imagineos_abi::SIGNAL_HUP
            | imagineos_abi::SIGNAL_INT
            | imagineos_abi::SIGNAL_KILL
            | imagineos_abi::SIGNAL_SEGV
            | imagineos_abi::SIGNAL_TERM
            | imagineos_abi::SIGNAL_CONT
            | imagineos_abi::SIGNAL_STOP
    )
}

fn next_pending_signal(pending: u64) -> Option<u64> {
    [
        imagineos_abi::SIGNAL_KILL,
        imagineos_abi::SIGNAL_STOP,
        imagineos_abi::SIGNAL_CONT,
        imagineos_abi::SIGNAL_HUP,
        imagineos_abi::SIGNAL_INT,
        imagineos_abi::SIGNAL_SEGV,
        imagineos_abi::SIGNAL_TERM,
    ]
    .into_iter()
    .find(|signal| pending & (1u64 << signal) != 0)
}

fn is_terminating_signal(signal: u64) -> bool {
    matches!(
        signal,
        imagineos_abi::SIGNAL_HUP
            | imagineos_abi::SIGNAL_INT
            | imagineos_abi::SIGNAL_SEGV
            | imagineos_abi::SIGNAL_TERM
    )
}

#[cfg(test)]
mod tests {
    use super::{is_terminating_signal, next_pending_signal, valid_signal};
    use imagineos_abi::{
        SIGNAL_HUP, SIGNAL_INT, SIGNAL_KILL, SIGNAL_SEGV, SIGNAL_STOP, SIGNAL_TERM,
    };

    #[test]
    fn accepts_only_signals_implemented_by_the_kernel() {
        for signal in [
            SIGNAL_HUP,
            SIGNAL_INT,
            SIGNAL_KILL,
            SIGNAL_SEGV,
            SIGNAL_TERM,
            imagineos_abi::SIGNAL_CONT,
            SIGNAL_STOP,
        ] {
            assert!(valid_signal(signal));
        }
        assert!(!valid_signal(0));
        assert!(!valid_signal(3));
    }

    #[test]
    fn uncatchable_termination_signals_have_priority_when_queued() {
        let pending = (1u64 << SIGNAL_TERM)
            | (1u64 << SIGNAL_STOP)
            | (1u64 << SIGNAL_KILL)
            | (1u64 << SIGNAL_INT);
        assert_eq!(next_pending_signal(pending), Some(SIGNAL_KILL));
        assert_eq!(next_pending_signal(1u64 << SIGNAL_STOP), Some(SIGNAL_STOP));
    }

    #[test]
    fn termination_defaults_match_the_supported_signal_contract() {
        assert!(is_terminating_signal(SIGNAL_HUP));
        assert!(is_terminating_signal(SIGNAL_INT));
        assert!(is_terminating_signal(SIGNAL_SEGV));
        assert!(is_terminating_signal(SIGNAL_TERM));
        assert!(!is_terminating_signal(SIGNAL_STOP));
        assert!(!is_terminating_signal(imagineos_abi::SIGNAL_CONT));
    }
}

pub fn can_install_to_disk() -> bool {
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    scheduler.processes[scheduler.current].allows_disk_install
}

pub fn descriptor(fd: usize) -> Option<Descriptor> {
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    (unsafe { &*DESCRIPTOR_TABLES.0.get() })[scheduler.current]
        .0
        .get(fd)
        .copied()
        .filter(|descriptor| descriptor.kind != DescriptorKind::Closed)
}

pub fn allocate_descriptor(descriptor: Descriptor) -> Option<usize> {
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    (unsafe { &mut *DESCRIPTOR_TABLES.0.get() })[scheduler.current].allocate(descriptor)
}

pub fn update_descriptor(fd: usize, descriptor: Descriptor) -> bool {
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let Some(slot) = (unsafe { &mut *DESCRIPTOR_TABLES.0.get() })[scheduler.current]
        .0
        .get_mut(fd)
    else {
        return false;
    };
    if slot.kind == DescriptorKind::Closed {
        return false;
    }
    *slot = descriptor;
    true
}

/// Fecha todos os descritores de um slot e libera as referências de pipe.
fn release_descriptors(slot: usize) {
    let tables = unsafe { &mut *DESCRIPTOR_TABLES.0.get() };
    for descriptor in tables[slot].0.iter_mut() {
        release_pipe_reference(descriptor);
        *descriptor = Descriptor::CLOSED;
    }
}

pub fn close_descriptor(fd: usize) -> bool {
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let Some(slot) = (unsafe { &mut *DESCRIPTOR_TABLES.0.get() })[scheduler.current]
        .0
        .get_mut(fd)
    else {
        return false;
    };
    let descriptor = *slot;
    if descriptor.kind == DescriptorKind::Closed {
        return false;
    }
    // Release the pipe reference before dropping the descriptor, otherwise the
    // end count would leak and a pipeline stage would never see EOF.
    release_pipe_reference(&descriptor);
    *slot = Descriptor::CLOSED;
    true
}

/// Adjusts the reader/writer counts of a pipe when a descriptor that points at
/// it disappears from a table.
fn release_pipe_reference(descriptor: &Descriptor) {
    match descriptor.kind {
        DescriptorKind::PipeRead => crate::ipc::close_reader(descriptor.pipe),
        DescriptorKind::PipeWrite => crate::ipc::close_writer(descriptor.pipe),
        _ => {}
    }
}

/// `dup`: devolve um novo descritor que referencia o mesmo objeto do `fd`.
///
/// Para pipes isso incrementa a contagem da ponta, exatamente como `dup(2)`.
pub fn duplicate_descriptor(fd: usize) -> Result<usize, i64> {
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    let Some(descriptor) = (unsafe { &*DESCRIPTOR_TABLES.0.get() })[scheduler.current]
        .0
        .get(fd)
        .copied()
    else {
        return Err(-9);
    };
    if descriptor.kind == DescriptorKind::Closed {
        return Err(-9);
    }
    match descriptor.kind {
        DescriptorKind::PipeRead => {
            crate::ipc::add_reader(descriptor.pipe).map_err(|error| error.to_errno())?
        }
        DescriptorKind::PipeWrite => {
            crate::ipc::add_writer(descriptor.pipe).map_err(|error| error.to_errno())?
        }
        _ => {}
    }
    allocate_descriptor(descriptor).ok_or_else(|| {
        release_pipe_reference(&descriptor);
        -24 // EMFILE
    })
}

/// `dup2`: faz `target` referenciar o mesmo objeto de `source`, fechando o que
/// houvesse em `target`. É a peça que o shell usa para instalar as pontas de um
/// pipe em `stdin`/`stdout`.
pub fn duplicate_descriptor_to(source: usize, target: usize) -> Result<usize, i64> {
    if target as u64 >= MAX_OPEN_FDS as u64 {
        return Err(-9);
    }
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    let Some(descriptor) = (unsafe { &*DESCRIPTOR_TABLES.0.get() })[scheduler.current]
        .0
        .get(source)
        .copied()
    else {
        return Err(-9);
    };
    if descriptor.kind == DescriptorKind::Closed {
        return Err(-9);
    }
    if source == target {
        return Ok(target);
    }
    // Take the extra pipe reference first: if the table entry cannot be
    // replaced we must not have changed any counts.
    match descriptor.kind {
        DescriptorKind::PipeRead => {
            crate::ipc::add_reader(descriptor.pipe).map_err(|error| error.to_errno())?
        }
        DescriptorKind::PipeWrite => {
            crate::ipc::add_writer(descriptor.pipe).map_err(|error| error.to_errno())?
        }
        _ => {}
    }
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let slot = &mut (unsafe { &mut *DESCRIPTOR_TABLES.0.get() })[scheduler.current].0[target];
    release_pipe_reference(slot);
    *slot = descriptor;
    Ok(target)
}

pub fn with_current_space<T>(operation: impl FnOnce(AddressSpace) -> T) -> T {
    let scheduler = unsafe { &*SCHEDULER.0.get() };
    operation(
        scheduler.processes[scheduler.current]
            .address_space
            .expect("process address space"),
    )
}

pub fn copy_from_current_user(address: u64, output: &mut [u8]) -> bool {
    with_current_space(|space| {
        let mut copied = 0usize;
        while copied < output.len() {
            let Some(current) = address.checked_add(copied as u64) else {
                return false;
            };
            let Some(source) = space.translate(current) else {
                return false;
            };
            let available =
                ((PAGE_SIZE - (current & (PAGE_SIZE - 1))) as usize).min(output.len() - copied);
            unsafe {
                ptr::copy_nonoverlapping(source, output.as_mut_ptr().add(copied), available);
            }
            copied += available;
        }
        true
    })
}

pub fn copy_to_current_user(address: u64, input: &[u8]) -> bool {
    with_current_space(|space| {
        let mut copied = 0usize;
        while copied < input.len() {
            let Some(current) = address.checked_add(copied as u64) else {
                return false;
            };
            let Some(destination) = space.translate(current) else {
                return false;
            };
            let available =
                ((PAGE_SIZE - (current & (PAGE_SIZE - 1))) as usize).min(input.len() - copied);
            unsafe {
                ptr::copy_nonoverlapping(input.as_ptr().add(copied), destination, available);
            }
            copied += available;
        }
        true
    })
}

pub fn grow_heap(increment: i64) -> Result<u64, i64> {
    if increment < 0 {
        return Err(-22);
    }
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let process = &mut scheduler.processes[scheduler.current];
    let previous = process.heap_end;
    let new_end = previous
        .checked_add(increment as u64)
        .filter(|end| *end - USER_HEAP_START <= USER_HEAP_LIMIT)
        .ok_or(-12)?;
    let first_page = previous
        .checked_add(PAGE_SIZE - 1)
        .ok_or(-12)?
        & !(PAGE_SIZE - 1);
    let page_end = new_end.checked_add(PAGE_SIZE - 1).ok_or(-12)? & !(PAGE_SIZE - 1);
    let address_space = process.address_space.ok_or(-12)?;
    let mut page = first_page;
    while page < page_end {
        match address_space.map_user_page(page) {
            Ok(_) | Err(MapError::AlreadyMapped) => {}
            Err(_) => return Err(-12),
        }
        page = page.checked_add(PAGE_SIZE).ok_or(-12)?;
    }
    process.heap_end = new_end;
    Ok(previous)
}

fn debug_log(message: &str) {
    #[cfg(feature = "kernel-debug")]
    crate::console_write(message);
    #[cfg(not(feature = "kernel-debug"))]
    let _ = message;
}
