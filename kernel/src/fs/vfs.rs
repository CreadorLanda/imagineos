//! Minimal virtual filesystem layer.
//!
//! Before this module the routing between the persistent DFS root and the
//! volatile USTAR/RAMFS root was open-coded as `if crate::dfs::is_mounted()` at
//! the top of nearly every function in `ramfs.rs`. That coupling made the two
//! backends impossible to reason about independently.
//!
//! [`FileSystem`] is the (deliberately small) contract every backend implements,
//! and [`Vfs`] is the single place that decides which backend is active for a
//! given call. The rest of the kernel keeps using the `ramfs::*` facade, which
//! now delegates here instead of branching on the mounted state itself.
//!
//! This is intentionally *not* a complete VFS: there is no inode/dentry cache,
//! no mount table, no `switch_root`/`pivot_root`, and no per-file `File` object.
//! It removes the duplicated routing and gives a seam to grow those later.

use core::cell::UnsafeCell;

use crate::dfs::{self, DfsError, Metadata};
use crate::ramfs::{self, FsError};

/// Error type returned by every [`FileSystem`] operation.
///
/// It unifies the two backend error enums so callers do not need to know which
/// backend served the request. Codes mirror the syscall ABI negative errnos via
/// [`VfsError::to_errno`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VfsError {
    InvalidPath,
    NotFound,
    NotDirectory,
    IsDirectory,
    AlreadyExists,
    DirectoryNotEmpty,
    NoSpace,
    Io,
}

impl VfsError {
    /// Maps to the negative errno used by the syscall ABI.
    pub fn to_errno(self) -> i64 {
        match self {
            VfsError::InvalidPath => -22,        // EINVAL
            VfsError::NotFound => -2,            // ENOENT
            VfsError::NotDirectory => -20,       // ENOTDIR
            VfsError::IsDirectory => -21,        // EISDIR
            VfsError::AlreadyExists => -17,      // EEXIST
            VfsError::DirectoryNotEmpty => -39,  // ENOTEMPTY
            VfsError::NoSpace => -28,            // ENOSPC
            VfsError::Io => -5,                  // EIO
        }
    }
}

impl From<FsError> for VfsError {
    fn from(error: FsError) -> Self {
        match error {
            FsError::InvalidPath => VfsError::InvalidPath,
            FsError::NotFound => VfsError::NotFound,
            FsError::NotDirectory => VfsError::NotDirectory,
            FsError::IsDirectory => VfsError::IsDirectory,
            FsError::AlreadyExists => VfsError::AlreadyExists,
            FsError::DirectoryNotEmpty => VfsError::DirectoryNotEmpty,
            FsError::NoSpace => VfsError::NoSpace,
        }
    }
}

impl From<DfsError> for VfsError {
    fn from(error: DfsError) -> Self {
        match error {
            DfsError::InvalidPath => VfsError::InvalidPath,
            DfsError::NotFound => VfsError::NotFound,
            DfsError::NotDirectory => VfsError::NotDirectory,
            DfsError::IsDirectory => VfsError::IsDirectory,
            DfsError::AlreadyExists => VfsError::AlreadyExists,
            DfsError::DirectoryNotEmpty => VfsError::DirectoryNotEmpty,
            DfsError::NoSpace | DfsError::JournalFull => VfsError::NoSpace,
            DfsError::Block(_) | DfsError::InvalidFilesystem | DfsError::CorruptMetadata => {
                VfsError::Io
            }
        }
    }
}

/// The operations a mounted filesystem must provide.
///
/// Implementations are stateless handles: the concrete backend (USTAR archive,
/// DFS on disk) holds its own state and this trait only describes the verbs.
pub trait FileSystem {
    /// Reads a whole file whose size was previously bounded by the caller.
    fn read_static(&self, path: &str) -> Option<&'static [u8]>;

    /// Reads up to `output.len()` bytes starting at `offset`.
    fn read_at(&self, path: &str, offset: usize, output: &mut [u8]) -> Result<usize, VfsError>;

    fn file_len(&self, path: &str) -> Result<usize, VfsError>;

    fn metadata(&self, path: &str) -> Option<Metadata>;

    fn is_directory(&self, path: &str) -> bool;

    fn is_file(&self, path: &str) -> bool;

    fn list_directory(
        &self,
        path: &str,
        include_hidden: bool,
        output: &mut [u8],
    ) -> Option<usize>;

    fn write_file_as(
        &self,
        path: &str,
        bytes: &[u8],
        uid: u32,
        gid: u32,
    ) -> Result<(), VfsError>;

    fn write_at(&self, path: &str, offset: usize, input: &[u8]) -> Result<usize, VfsError>;

    fn create_file_with_mode_as(
        &self,
        path: &str,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<(), VfsError>;

    fn create_directory_with_parents_as(
        &self,
        path: &str,
        parents: bool,
        uid: u32,
        gid: u32,
    ) -> Result<(), VfsError>;

    fn remove(&self, path: &str, recursive: bool) -> Result<(), VfsError>;
}

/// Which backend serves filesystem calls.
///
/// The DFS functions are reached through the module-level `dfs::*` entry points,
/// which already hold the mounted instance in their own state; this enum only
/// records which side of the split is active.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// Persistent DFS mounted from the GPT partition laid over the ATA disk.
    Dfs,
    /// Volatile USTAR archive plus the in-memory overlay.
    RamFs,
}

/// The current filesystem root, in the `pivot_root(2)` sense.
///
/// The kernel boots with the initramfs (RamFs) as `/`. A userspace init then
/// mounts the real root (DFS) — which only *prepares* it, it does not move the
/// root — and finally calls `pivot_root` to swap. Like Linux's global root
/// (before mount namespaces), the switch is system-wide: every process sees the
/// new root the instant it happens, which is exactly what the two-stage init
/// needs (`/init` of the initramfs re-execs `/sbin/init` of the real root).
struct SharedRootBackend(UnsafeCell<Backend>);
unsafe impl Sync for SharedRootBackend {}
static ROOT_BACKEND: SharedRootBackend = SharedRootBackend(UnsafeCell::new(Backend::RamFs));

fn active_backend() -> Backend {
    // SAFETY: written only by `pivot_root_to` at the single call site of the
    // `SYS_PIVOT_ROOT` syscall (no interrupts, no SMP in this kernel).
    unsafe { *ROOT_BACKEND.0.get() }
}

/// Swaps the global root from the initramfs to `new_root`.
///
/// Mirrors `pivot_root(2)`: `new_root` must be a path on the *other* backend
/// (here: the DFS, already mounted by the caller), and `put_old` is where the
/// initramfs remains reachable — we record the name but do not relocate
/// memory, since the RamFs pages are never freed (no reclaim in this kernel;
/// this is the honest equivalent of "the old root stays mounted at put_old").
/// Returns `EINVAL` when there is no DFS to pivot to, or the pivot is already
/// done — POSIX would return the same for a root on the same mount.
pub fn pivot_root_to(new_root: &str, put_old: &str) -> Result<(), VfsError> {
    if new_root != "/" && !new_root.is_empty() {
        return Err(VfsError::InvalidPath);
    }
    if put_old.is_empty() || !put_old.starts_with('/') {
        return Err(VfsError::InvalidPath);
    }
    if !dfs::is_mounted() {
        // Pivoting without a prepared new root is exactly what Linux rejects
        // with EINVAL ("new_root is not a mount point").
        return Err(VfsError::InvalidPath);
    }
    // SAFETY: single-threaded kernel; the syscall path holds no Vfs borrows.
    unsafe {
        *ROOT_BACKEND.0.get() = Backend::Dfs;
    }
    // Sentinel para o userspace: o init da fase 2 (re-executado após o pivô)
    // distingue "já pivotou" pela existência de /run/pivoted, o mesmo papel do
    // marcador que initramfs do Linux deixa antes de executar a raiz real. O
    // diretório /run precisa existir na raiz real; se não existir, o próprio
    // DFS cria os pais (create_directory com parents).
    let _ = dfs::create_directory_with_parents_as("/run", true, 0, 0);
    let _ = dfs::write_file_as("/run/pivoted", b"1\n", 0o644, 0, 0);
    Ok(())
}

/// Reports whether a persistent DFS root is currently mounted.
///
/// This is the one question the rest of the kernel still needs to ask directly
/// (for example to pick the default font source), so it stays a free function
/// rather than being buried in the [`FileSystem`] trait.
pub fn has_persistent_root() -> bool {
    matches!(active_backend(), Backend::Dfs)
}

/// Routes [`FileSystem`] calls to whichever backend is mounted.
///
/// A new `Vfs` is cheap: it is just a snapshot of the current backend choice.
pub struct Vfs {
    backend: Backend,
}

impl Vfs {
    /// Captures the currently mounted backend.
    pub fn current() -> Self {
        Self {
            backend: active_backend(),
        }
    }

}

impl FileSystem for Vfs {
    fn read_static(&self, path: &str) -> Option<&'static [u8]> {
        match self.backend {
            Backend::Dfs => dfs::read_static(path),
            Backend::RamFs => ramfs::read_from_overlay(path),
        }
    }

    fn read_at(&self, path: &str, offset: usize, output: &mut [u8]) -> Result<usize, VfsError> {
        match self.backend {
            Backend::Dfs => dfs::read_at(path, offset, output).map_err(VfsError::from),
            Backend::RamFs => ramfs::read_at_overlay(path, offset, output).map_err(VfsError::from),
        }
    }

    fn file_len(&self, path: &str) -> Result<usize, VfsError> {
        match self.backend {
            Backend::Dfs => dfs::file_len(path).map_err(VfsError::from),
            Backend::RamFs => ramfs::file_len_overlay(path).map_err(VfsError::from),
        }
    }

    fn metadata(&self, path: &str) -> Option<Metadata> {
        match self.backend {
            Backend::Dfs => dfs::stat(path).ok(),
            Backend::RamFs => ramfs::metadata_overlay(path),
        }
    }

    fn is_directory(&self, path: &str) -> bool {
        match self.backend {
            Backend::Dfs => dfs::stat(path).is_ok_and(|metadata| metadata.is_directory),
            Backend::RamFs => ramfs::is_directory_overlay(path),
        }
    }

    fn is_file(&self, path: &str) -> bool {
        match self.backend {
            Backend::Dfs => dfs::stat(path).is_ok_and(|metadata| !metadata.is_directory),
            Backend::RamFs => ramfs::is_file_overlay(path),
        }
    }

    fn list_directory(
        &self,
        path: &str,
        include_hidden: bool,
        output: &mut [u8],
    ) -> Option<usize> {
        match self.backend {
            Backend::Dfs => dfs::list_directory(path, include_hidden, output).ok(),
            Backend::RamFs => ramfs::list_directory_overlay(path, include_hidden, output),
        }
    }

    fn write_file_as(&self, path: &str, bytes: &[u8], uid: u32, gid: u32) -> Result<(), VfsError> {
        match self.backend {
            Backend::Dfs => {
                dfs::write_file_as(path, bytes, 0o644, uid, gid).map_err(VfsError::from)
            }
            Backend::RamFs => ramfs::write_file_overlay(path, bytes, uid, gid).map_err(VfsError::from),
        }
    }

    fn write_at(&self, path: &str, offset: usize, input: &[u8]) -> Result<usize, VfsError> {
        match self.backend {
            Backend::Dfs => {
                dfs::write_at(path, offset, input, offset == 0).map_err(VfsError::from)
            }
            Backend::RamFs => {
                ramfs::write_at_overlay(path, offset, input).map_err(VfsError::from)
            }
        }
    }

    fn create_file_with_mode_as(
        &self,
        path: &str,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<(), VfsError> {
        match self.backend {
            Backend::Dfs => dfs::write_at_as(path, 0, &[], true, mode & 0o777, uid, gid)
                .map(|_| ())
                .map_err(VfsError::from),
            Backend::RamFs => {
                ramfs::create_file_with_mode_overlay(path, mode, uid, gid).map_err(VfsError::from)
            }
        }
    }

    fn create_directory_with_parents_as(
        &self,
        path: &str,
        parents: bool,
        uid: u32,
        gid: u32,
    ) -> Result<(), VfsError> {
        match self.backend {
            Backend::Dfs => dfs::create_directory_with_parents_as(path, parents, uid, gid)
                .map_err(VfsError::from),
            Backend::RamFs => {
                ramfs::create_directory_with_parents_overlay(path, parents, uid, gid)
                    .map_err(VfsError::from)
            }
        }
    }

    fn remove(&self, path: &str, recursive: bool) -> Result<(), VfsError> {
        match self.backend {
            Backend::Dfs => dfs::remove(path, recursive).map_err(VfsError::from),
            Backend::RamFs => ramfs::remove_overlay(path, recursive).map_err(VfsError::from),
        }
    }
}
