use crate::errors::{exit, usage_error, MagineError, Result};
use crate::output::write_err;
use crate::term;
use std::io::{BufRead, Read};
use std::sync::mpsc;
use std::time::Duration;

/// A line from the terminal (the question goes to stderr).
pub fn ask(question: &str) -> Result<String> {
    write_err(question);
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).map_err(|e| MagineError::failed(format!("Could not read the answer: {e}")))?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

pub fn confirm(question: &str, default_yes: bool) -> Result<bool> {
    let a = ask(&format!("{question} {} ", if default_yes { "[Y/n]" } else { "[y/N]" }))?.trim().to_ascii_lowercase();
    if a.is_empty() {
        return Ok(default_yes);
    }
    Ok(a == "y" || a == "yes")
}

/// Typed without echo, for secrets. Ctrl+C cancels and restores the terminal.
pub fn ask_hidden(question: &str) -> Result<String> {
    if !term::stdin_is_tty() {
        return Err(MagineError::new("No terminal to type into.", exit::USAGE, "usage"));
    }
    write_err(question);
    let line = term::without_echo(|| {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).map(|_| line)
    });
    write_err("\n");
    let line = line.map_err(|e| MagineError::failed(format!("Could not read the answer: {e}")))?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Everything piped on stdin, trimmed; empty when nothing arrives within `idle` (some agents leave stdin open).
pub fn read_stdin(max_bytes: usize, idle: Duration) -> Result<String> {
    Ok(String::from_utf8_lossy(&read_stdin_bytes(max_bytes, idle)?).trim().to_string())
}

/// The raw bytes piped on stdin, read the same way as `read_stdin`.
pub fn read_stdin_bytes(max_bytes: usize, idle: Duration) -> Result<Vec<u8>> {
    let (tx, rx) = mpsc::channel::<std::io::Result<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Ok(Vec::new()));
                    return;
                }
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
        }
    });
    let mut data = Vec::new();
    loop {
        match rx.recv_timeout(idle) {
            Ok(Ok(chunk)) if chunk.is_empty() => break,
            Ok(Ok(chunk)) => {
                data.extend_from_slice(&chunk);
                if data.len() > max_bytes {
                    return Err(usage_error("Too much input on stdin."));
                }
            }
            Ok(Err(e)) => return Err(MagineError::failed(format!("Could not read stdin: {e}"))),
            // Quiet for a while: what arrived is the input (a writer that never closes stdin).
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(data)
}
