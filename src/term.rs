//! The terminal: colours, width, hidden input and Ctrl+C.

use crate::errors::{MagineError, Result};
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static WAITING: AtomicUsize = AtomicUsize::new(0);
static JSON_ERRORS: AtomicBool = AtomicBool::new(false);
static ECHO_OFF: Mutex<Option<EchoState>> = Mutex::new(None);

pub fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

pub fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

pub fn stderr_is_tty() -> bool {
    std::io::stderr().is_terminal()
}

/// Errors printed by the Ctrl+C handler follow the command's --json choice.
pub fn set_json_errors(on: bool) {
    JSON_ERRORS.store(on, Ordering::SeqCst);
}

/// Ctrl+C: during a wait the wait ends cleanly (the run goes on); anywhere else the CLI stops at once.
pub fn install_interrupt_handler() {
    let _ = ctrlc::set_handler(|| {
        if WAITING.load(Ordering::SeqCst) > 0 && !INTERRUPTED.swap(true, Ordering::SeqCst) {
            return;
        }
        restore_echo();
        let mut err = std::io::stderr();
        if JSON_ERRORS.load(Ordering::SeqCst) {
            let _ = writeln!(err, "{{\"error\":{{\"code\":\"interrupted\",\"message\":\"Cancelled.\"}}}}");
        } else {
            let _ = writeln!(err, "\nerror: Cancelled.");
        }
        let _ = err.flush();
        std::process::exit(130);
    });
}

/// Marks a wait that Ctrl+C should end with a message rather than a hard stop.
pub struct Waiting;

impl Waiting {
    pub fn start() -> Self {
        WAITING.fetch_add(1, Ordering::SeqCst);
        Waiting
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        WAITING.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn check_interrupt() -> Result<()> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        Err(MagineError::interrupted())
    } else {
        Ok(())
    }
}

/// Sleeps, but wakes within 50 ms of Ctrl+C.
pub fn sleep_ms(ms: u64) -> Result<()> {
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        check_interrupt()?;
        let now = Instant::now();
        if now >= end {
            return Ok(());
        }
        std::thread::sleep((end - now).min(Duration::from_millis(50)));
    }
}

/// Terminal width for stdout, or COLUMNS, or 100.
pub fn columns() -> usize {
    os::columns().or_else(|| std::env::var("COLUMNS").ok().and_then(|c| c.trim().parse().ok())).filter(|c| *c >= 20).unwrap_or(100)
}

/// Lets Windows consoles show ANSI colours; false when they cannot.
pub fn enable_ansi() -> bool {
    os::enable_ansi()
}

#[derive(Clone, Copy)]
struct EchoState(#[allow(dead_code)] os::Saved);

fn restore_echo() {
    if let Ok(mut g) = ECHO_OFF.lock() {
        if let Some(EchoState(saved)) = g.take() {
            os::restore(saved);
        }
    }
}

/// Runs `f` with the terminal's echo off, restoring it even on Ctrl+C.
pub fn without_echo<T>(f: impl FnOnce() -> T) -> T {
    if let Some(saved) = os::echo_off() {
        if let Ok(mut g) = ECHO_OFF.lock() {
            *g = Some(EchoState(saved));
        }
    }
    let out = f();
    restore_echo();
    out
}

#[cfg(windows)]
mod os {
    use std::ffi::c_void;

    type Handle = *mut c_void;
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    #[repr(C)]
    struct Coord {
        x: i16,
        y: i16,
    }
    #[repr(C)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }
    #[repr(C)]
    struct ScreenBufferInfo {
        size: Coord,
        cursor: Coord,
        attributes: u16,
        window: SmallRect,
        max_window: Coord,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n: u32) -> Handle;
        fn GetConsoleMode(h: Handle, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: Handle, mode: u32) -> i32;
        fn GetConsoleScreenBufferInfo(h: Handle, info: *mut ScreenBufferInfo) -> i32;
    }

    #[derive(Clone, Copy)]
    pub struct Saved(u32);

    pub fn enable_ansi() -> bool {
        let mut ok = true;
        for n in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            unsafe {
                let h = GetStdHandle(n);
                let mut mode = 0u32;
                if h.is_null() || GetConsoleMode(h, &mut mode) == 0 {
                    continue; // not a console (a pipe or file): nothing to switch on
                }
                if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING == 0 && SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) == 0 {
                    ok = false;
                }
            }
        }
        ok
    }

    pub fn columns() -> Option<usize> {
        unsafe {
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut info: ScreenBufferInfo = std::mem::zeroed();
            if h.is_null() || GetConsoleScreenBufferInfo(h, &mut info) == 0 {
                return None;
            }
            let w = info.window.right as i32 - info.window.left as i32 + 1;
            if w > 0 {
                Some(w as usize)
            } else {
                None
            }
        }
    }

    pub fn echo_off() -> Option<Saved> {
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0u32;
            if h.is_null() || GetConsoleMode(h, &mut mode) == 0 {
                return None;
            }
            if SetConsoleMode(h, mode & !ENABLE_ECHO_INPUT) == 0 {
                return None;
            }
            Some(Saved(mode))
        }
    }

    pub fn restore(saved: Saved) {
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            if !h.is_null() {
                SetConsoleMode(h, saved.0);
            }
        }
    }
}

#[cfg(unix)]
mod os {
    #[derive(Clone, Copy)]
    pub struct Saved(libc::termios);

    pub fn enable_ansi() -> bool {
        true
    }

    pub fn columns() -> Option<usize> {
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
                Some(ws.ws_col as usize)
            } else {
                None
            }
        }
    }

    pub fn echo_off() -> Option<Saved> {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                return None;
            }
            let saved = t;
            t.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t) != 0 {
                return None;
            }
            Some(Saved(saved))
        }
    }

    pub fn restore(saved: Saved) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved.0);
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod os {
    #[derive(Clone, Copy)]
    pub struct Saved;
    pub fn enable_ansi() -> bool {
        true
    }
    pub fn columns() -> Option<usize> {
        None
    }
    pub fn echo_off() -> Option<Saved> {
        None
    }
    pub fn restore(_: Saved) {}
}
