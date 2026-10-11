/*
    Implementação da imagineos-rt, uma runtime para o ImagineOS

    userland/runtime/src/lib.rs
*/

#![no_std]

use core::fmt::{self, Write};
use core::marker::PhantomData;

pub use imagineos;

use imagineos::{Error, Result};

pub trait IntoExitCode {
    fn into_exit_code(self) -> i32;
}

impl IntoExitCode for i32 {
    fn into_exit_code(self) -> i32 {
        self
    }
}

impl IntoExitCode for Result<()> {
    fn into_exit_code(self) -> i32 {
        self.map_or_else(Error::code, |_| 0)
    }
}

impl IntoExitCode for core::result::Result<(), i32> {
    fn into_exit_code(self) -> i32 {
        self.map_or_else(|code| code, |_| 0)
    }
}

pub struct Arguments<'a> {
    vector: *const *const u8,
    count: usize,
    index: usize,
    _lifetime: PhantomData<&'a [u8]>,
}

impl<'a> Arguments<'a> {
    fn from_raw(vector: *const *const u8, count: usize) -> Self {
        Self {
            vector,
            count,
            index: 1,
            _lifetime: PhantomData,
        }
    }
}

impl<'a> Iterator for Arguments<'a> {
    type Item = Result<&'a str>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.count {
            return None;
        }
        let index = self.index;
        self.index += 1;
        let argument = unsafe { imagineos::args::argument(self.vector, self.count, index) }?;
        Some(core::str::from_utf8(argument).map_err(|_| Error::INVALID_ARGUMENT))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.count.saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for Arguments<'_> {}

pub struct Environment<'a> {
    vector: *const *const u8,
    count: usize,
    _lifetime: PhantomData<&'a [u8]>,
}

impl<'a> Environment<'a> {
    fn from_raw(vector: *const *const u8, count: usize) -> Self {
        Self {
            vector,
            count,
            _lifetime: PhantomData,
        }
    }

    pub fn get(&self, name: &str) -> Result<Option<&'a str>> {
        let Some(value) = (unsafe {
            imagineos::args::environment_value(self.vector, self.count, name.as_bytes())
        }) else {
            return Ok(None);
        };
        core::str::from_utf8(value)
            .map(Some)
            .map_err(|_| Error::INVALID_ARGUMENT)
    }
}

pub fn start<R: IntoExitCode>(entry: fn() -> R) -> ! {
    if imagineos::syscall::abi_version() != Ok(imagineos::abi::ABI_VERSION) {
        imagineos::process::exit(2);
    }
    imagineos::process::exit(entry().into_exit_code())
}

/// # Safety
/// The kernel must supply valid argument and environment vectors for the
/// lifetime of the entry function.
pub unsafe fn start_with_args<R: IntoExitCode>(
    argc: usize,
    argv: *const *const u8,
    envc: usize,
    envp: *const *const u8,
    entry: for<'a, 'b> fn(Arguments<'a>, Environment<'b>) -> R,
) -> ! {
    if imagineos::syscall::abi_version() != Ok(imagineos::abi::ABI_VERSION) {
        imagineos::process::exit(2);
    }
    let arguments = Arguments::from_raw(argv, argc);
    let environment = Environment::from_raw(envp, envc);
    imagineos::process::exit(entry(arguments, environment).into_exit_code())
}

struct Console {
    error: Option<Error>,
}

impl Write for Console {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        match imagineos::console::write_all(text.as_bytes()) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.error = Some(error);
                Err(fmt::Error)
            }
        }
    }
}

pub fn print(arguments: fmt::Arguments<'_>) -> Result<()> {
    let mut console = Console { error: None };
    match console.write_fmt(arguments) {
        Ok(()) => Ok(()),
        Err(_) => Err(console.error.unwrap_or_else(|| Error::from_errno(5))),
    }
}

#[macro_export]
macro_rules! main {
    ($entry:path) => {
        #[unsafe(no_mangle)]
        extern "C" fn _start(
            _argc: usize,
            _argv: *const *const u8,
            _envc: usize,
            _envp: *const *const u8,
        ) -> ! {
            $crate::start($entry)
        }

        #[panic_handler]
        fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
            $crate::imagineos::process::exit(127)
        }
    };
    (with_args $entry:path) => {
        #[unsafe(no_mangle)]
        extern "C" fn _start(
            argc: usize,
            argv: *const *const u8,
            envc: usize,
            envp: *const *const u8,
        ) -> ! {
            unsafe { $crate::start_with_args(argc, argv, envc, envp, $entry) }
        }

        #[panic_handler]
        fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
            $crate::imagineos::process::exit(127)
        }
    };
}

#[macro_export]
macro_rules! print {
    ($($argument:tt)*) => {
        $crate::print(format_args!($($argument)*))
    };
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::print(format_args!("\n"))
    };
    ($($argument:tt)*) => {
        $crate::print(format_args!("{}\n", format_args!($($argument)*)))
    };
}
