use serde_json::{Map, Value};
use std::fmt;
use std::ops::{Deref, DerefMut};

/// Exit codes: stable, documented in `magine guide`.
pub mod exit {
    pub const OK: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const NOT_FOUND: i32 = 3;
    pub const AUTH: i32 = 4;
    pub const PAYMENT: i32 = 5;
    pub const CONFLICT: i32 = 6;
    pub const RATE_LIMITED: i32 = 7;
    pub const NETWORK: i32 = 8;
    pub const WAITING: i32 = 9;
    pub const CONFIG: i32 = 10;
    pub const INTERRUPTED: i32 = 130;
}

/// What went wrong: for people (message, hint) and for scripts (exit code, code, details).
#[derive(Debug, Clone)]
pub struct ErrorInfo {
    pub message: String,
    pub exit_code: i32,
    pub code: String,
    pub hint: Option<String>,
    pub details: Map<String, Value>,
}

/// Boxed, so every `Result` stays small: errors are rare and need not slow the path without one.
#[derive(Debug, Clone)]
pub struct MagineError(Box<ErrorInfo>);

impl Deref for MagineError {
    type Target = ErrorInfo;
    fn deref(&self) -> &ErrorInfo {
        &self.0
    }
}

impl DerefMut for MagineError {
    fn deref_mut(&mut self) -> &mut ErrorInfo {
        &mut self.0
    }
}

pub type Result<T> = std::result::Result<T, MagineError>;

impl MagineError {
    pub fn new(message: impl Into<String>, exit_code: i32, code: impl Into<String>) -> Self {
        MagineError(Box::new(ErrorInfo { message: message.into(), exit_code, code: code.into(), hint: None, details: Map::new() }))
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn details(mut self, details: Map<String, Value>) -> Self {
        self.details = details;
        self
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(message, exit::FAILED, "failed")
    }

    pub fn interrupted() -> Self {
        Self::new("Cancelled.", exit::INTERRUPTED, "interrupted")
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(message, exit::CONFIG, "config")
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::new(message, exit::NETWORK, "network")
    }
}

impl fmt::Display for MagineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

pub fn usage_error(message: impl Into<String>) -> MagineError {
    MagineError::new(message, exit::USAGE, "usage")
}

/// Maps an API error code (or HTTP status) to an exit code.
pub fn exit_for_api(code: &str, status: u16) -> i32 {
    match code {
        "unauthorized" | "key_expired" | "access_denied" | "expired_token" | "forbidden" => return exit::AUTH,
        "payment_required" => return exit::PAYMENT,
        "not_found" => return exit::NOT_FOUND,
        "conflict" => return exit::CONFLICT,
        "rate_limited" | "slow_down" => return exit::RATE_LIMITED,
        "bad_request" | "too_large" => return exit::FAILED,
        "server_error" | "unavailable" => return exit::NETWORK,
        _ => {}
    }
    match status {
        401 | 403 => exit::AUTH,
        402 => exit::PAYMENT,
        404 => exit::NOT_FOUND,
        409 => exit::CONFLICT,
        429 => exit::RATE_LIMITED,
        s if s >= 500 => exit::NETWORK,
        _ => exit::FAILED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_errors_map_to_stable_exit_codes() {
        assert_eq!(exit_for_api("unauthorized", 401), exit::AUTH);
        assert_eq!(exit_for_api("key_expired", 401), exit::AUTH);
        assert_eq!(exit_for_api("payment_required", 402), exit::PAYMENT);
        assert_eq!(exit_for_api("not_found", 404), exit::NOT_FOUND);
        assert_eq!(exit_for_api("conflict", 409), exit::CONFLICT);
        assert_eq!(exit_for_api("rate_limited", 429), exit::RATE_LIMITED);
        assert_eq!(exit_for_api("server_error", 502), exit::NETWORK);
        assert_eq!(exit_for_api("", 503), exit::NETWORK);
        assert_eq!(exit_for_api("", 418), exit::FAILED);
    }
}
