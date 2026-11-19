use crate::errors::{exit, exit_for_api, MagineError, Result};
use crate::http::{user_agent, Http, Request};
use crate::term;
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// One path segment, encoded like encodeURIComponent: ids and names are never parsed as paths.
pub fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Default)]
pub struct Call<'a> {
    query: Vec<(String, String)>,
    json: Option<Value>,
    body: Option<Vec<u8>>,
    content_type: Option<String>,
    no_auth: bool,
    timeout: Option<Duration>,
    sink: Option<&'a mut dyn Write>,
    max_bytes: Option<u64>,
}

impl<'a> Call<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// A query parameter; left out when there is no value.
    pub fn q(mut self, key: &str, value: Option<&str>) -> Self {
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            self.query.push((key.to_string(), v.to_string()));
        }
        self
    }

    pub fn json(mut self, v: Value) -> Self {
        self.json = Some(v);
        self
    }

    pub fn body(mut self, bytes: Vec<u8>, content_type: &str) -> Self {
        self.body = Some(bytes);
        self.content_type = Some(content_type.to_string());
        self
    }

    pub fn no_auth(mut self) -> Self {
        self.no_auth = true;
        self
    }

    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout = Some(Duration::from_millis(ms));
        self
    }

    pub fn sink(mut self, w: &'a mut dyn Write) -> Self {
        self.sink = Some(w);
        self
    }

    pub fn max_bytes(mut self, n: u64) -> Self {
        self.max_bytes = Some(n);
        self
    }
}

pub struct ApiResult {
    pub status: u16,
    pub data: Value,
}

pub struct Api {
    pub base_url: String,
    key: Option<String>,
    http: Arc<Http>,
    timeout: Duration,
    pub server_version: RefCell<Option<String>>,
}

impl Api {
    pub fn new(base_url: &str, key: Option<String>, http: Arc<Http>, timeout_ms: u64) -> Self {
        Api { base_url: base_url.to_string(), key, http, timeout: Duration::from_millis(timeout_ms), server_version: RefCell::new(None) }
    }

    pub fn get(&self, path: &str) -> Result<ApiResult> {
        self.call("GET", path, Call::new())
    }

    pub fn call(&self, method: &str, path: &str, c: Call) -> Result<ApiResult> {
        let mut url = Url::parse(&self.base_url)
            .and_then(|b| b.join(&format!("/api/v1/{}", path.trim_start_matches('/'))))
            .map_err(|_| MagineError::config(format!("\"{}\" is not a URL.", self.base_url)))?;
        if !c.query.is_empty() {
            let mut q = url.query_pairs_mut();
            for (k, v) in &c.query {
                q.append_pair(k, v);
            }
        }
        let mut headers = vec![
            ("accept".to_string(), "application/json".to_string()),
            ("user-agent".to_string(), user_agent()),
            ("x-magine-client".to_string(), format!("cli/{}", env!("CARGO_PKG_VERSION"))),
        ];
        if !c.no_auth {
            let Some(key) = &self.key else {
                return Err(MagineError::new("You are not signed in.", exit::AUTH, "unauthorized").hint("Run \"magine login\" (or set MAGINE_API_KEY)."));
            };
            headers.push(("authorization".to_string(), format!("Bearer {key}")));
        }
        let body = if let Some(j) = &c.json {
            headers.push(("content-type".to_string(), "application/json".to_string()));
            Some(serde_json::to_vec(j).unwrap_or_default())
        } else if let Some(b) = c.body {
            headers.push(("content-type".to_string(), c.content_type.unwrap_or_else(|| "application/octet-stream".into())));
            Some(b)
        } else {
            None
        };

        // Reads are retried on a dropped connection or a busy server; writes never are, so nothing runs or charges twice.
        let mut sink = c.sink;
        let retries = if method == "GET" && sink.is_none() { 2 } else { 0 };
        let timeout = c.timeout.unwrap_or(self.timeout);
        let mut attempt = 0u32;
        loop {
            let res = self.http.request(Request {
                url: &url,
                method,
                headers: headers.clone(),
                body: body.clone(),
                timeout,
                sink: sink.as_mut().map(|w| &mut **w as &mut dyn Write),
                max_bytes: c.max_bytes,
            });
            let res = match res {
                Ok(r) => r,
                Err(e) if attempt < retries && e.exit_code == exit::NETWORK && e.code == "network" => {
                    term::sleep_ms(500 * 3u64.pow(attempt))?;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e),
            };
            if let Some(v) = res.headers.get("x-magine-version") {
                *self.server_version.borrow_mut() = Some(v.clone());
            }
            if attempt < retries && matches!(res.status, 502..=504) {
                term::sleep_ms(500 * 3u64.pow(attempt))?;
                attempt += 1;
                continue;
            }
            if attempt < retries && res.status == 429 {
                let wait = res.headers.get("retry-after").and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(2).clamp(1, 10);
                term::sleep_ms(wait * 1000)?;
                attempt += 1;
                continue;
            }
            return parse(res.status, &res.headers, &res.body, &url);
        }
    }
}

fn parse(status: u16, headers: &HashMap<String, String>, body: &[u8], url: &Url) -> Result<ApiResult> {
    let host = url.host_str().unwrap_or("the server");
    let host = match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    };
    let is_json = headers.get("content-type").is_some_and(|t| t.contains("application/json"));
    let mut data = Value::Null;
    if !body.is_empty() && is_json {
        data = serde_json::from_slice(body)
            .map_err(|_| MagineError::new(format!("{host} sent an answer that is not valid JSON."), exit::NETWORK, "bad_response"))?;
    }
    if (300..400).contains(&status) {
        return Err(MagineError::new(format!("{host} redirected the request (HTTP {status}). Check the API URL."), exit::CONFIG, "redirect"));
    }
    if status >= 400 {
        if let Some(e) = data.get("error").and_then(Value::as_object) {
            if let Some(message) = e.get("message").and_then(Value::as_str) {
                let code = e.get("code").and_then(Value::as_str).unwrap_or("");
                let mut details = Map::new();
                for (k, v) in e {
                    if k != "code" && k != "message" && k != "hint" {
                        details.insert(k.clone(), v.clone());
                    }
                }
                let mut err = MagineError::new(message, exit_for_api(code, status), if code.is_empty() { "failed" } else { code }).details(details);
                if let Some(h) = e.get("hint").and_then(Value::as_str) {
                    err = err.hint(h);
                }
                return Err(err);
            }
        }
        if status == 429 {
            return Err(MagineError::new("Too many requests. Wait a moment and try again.", exit::RATE_LIMITED, "rate_limited"));
        }
        return Err(MagineError::new(format!("{host} answered HTTP {status}."), exit_for_api("", status), format!("http_{status}")));
    }
    if data.is_null() && !body.is_empty() {
        return Err(MagineError::config(format!("{host} did not answer like the Magine API. Check the API URL.")).details(Map::new()));
    }
    Ok(ApiResult { status, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_encoded_like_encode_uri_component() {
        assert_eq!(seg("news-bot"), "news-bot");
        assert_eq!(seg("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(seg("../x"), "..%2Fx");
        assert_eq!(seg("caf\u{e9}"), "caf%C3%A9");
    }

    #[test]
    fn errors_carry_the_api_code_hint_and_details() {
        let url = Url::parse("https://magine.cloud/api/v1/agents").unwrap();
        let h: HashMap<String, String> = [("content-type".to_string(), "application/json".to_string())].into();
        let body = br#"{"error":{"code":"payment_required","message":"Buy Cats","hint":"magine wallet buy 1","required":2}}"#;
        let e = parse(402, &h, body, &url).err().unwrap();
        assert_eq!(e.exit_code, exit::PAYMENT);
        assert_eq!(e.code, "payment_required");
        assert_eq!(e.hint.as_deref(), Some("magine wallet buy 1"));
        assert_eq!(e.details["required"], 2);
        let html: HashMap<String, String> = [("content-type".to_string(), "text/html".to_string())].into();
        assert_eq!(parse(200, &html, b"<html>", &url).err().unwrap().exit_code, exit::CONFIG);
        assert_eq!(parse(301, &html, b"", &url).err().unwrap().code, "redirect");
        assert_eq!(parse(503, &html, b"busy", &url).err().unwrap().exit_code, exit::NETWORK);
    }
}
