//! HTTPS to the Magine API: rustls with the Mozilla roots plus the operating system's,
//! proxies through a CONNECT tunnel, and errors in plain words.

use crate::env::Env;
use crate::errors::{exit, MagineError, Result};
use crate::output::Output;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::CertificateDer;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;

const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const LOCAL: [&str; 4] = ["localhost", "127.0.0.1", "::1", "[::1]"];
/// Files of extra trusted certificates (PEM): company proxies that inspect TLS.
pub const CA_ENV: [&str; 3] = ["MAGINE_CA_CERTS", "SSL_CERT_FILE", "NODE_EXTRA_CA_CERTS"];

pub struct Request<'a> {
    pub url: &'a Url,
    pub method: &'a str,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
    /// Receives a 2xx body as it arrives instead of buffering it (downloads).
    pub sink: Option<&'a mut dyn Write>,
    pub max_bytes: Option<u64>,
}

pub struct Response {
    pub status: u16,
    /// Lower-case names.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct Http {
    tls: Arc<rustls::ClientConfig>,
    env: Env,
    agents: Mutex<HashMap<String, ureq::Agent>>,
}

fn host_port(u: &Url) -> String {
    match (u.host_str(), u.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => u.as_str().to_string(),
    }
}

/// The proxy for this URL from HTTPS_PROXY / HTTP_PROXY / ALL_PROXY, honouring NO_PROXY; never for this computer.
pub fn proxy_for(url: &Url, env: &Env) -> Option<Url> {
    let host = url.host_str().unwrap_or("").trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if LOCAL.contains(&host.as_str()) {
        return None;
    }
    let port = url.port_or_known_default().map(|p| p.to_string()).unwrap_or_default();
    for raw in env.first(&["NO_PROXY", "no_proxy"]).unwrap_or("").split(',') {
        let entry = raw.trim().to_ascii_lowercase();
        if entry.is_empty() {
            continue;
        }
        if entry == "*" {
            return None;
        }
        let (name, entry_port) = match entry.rsplit_once(':') {
            Some((n, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (n.to_string(), Some(p.to_string())),
            _ => (entry.clone(), None),
        };
        let name = name.trim_start_matches('*').trim_start_matches('.').to_string();
        if entry_port.as_deref().is_some_and(|p| p != port) {
            continue;
        }
        if host == name || host.ends_with(&format!(".{name}")) {
            return None;
        }
    }
    let raw = if url.scheme() == "https" {
        env.first(&["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"])
    } else {
        env.first(&["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"])
    }?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let p = Url::parse(&if raw.contains("://") { raw.to_string() } else { format!("http://{raw}") }).ok()?;
    if p.scheme() == "http" || p.scheme() == "https" {
        Some(p)
    } else {
        None
    }
}

fn tls_config(env: &Env, out: Option<&Output>) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // Company CAs installed on this computer are trusted too, as browsers do.
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    for var in CA_ENV {
        let Some(path) = env.non_empty(var) else { continue };
        let mut added = 0;
        if let Ok(iter) = CertificateDer::pem_file_iter(path) {
            for cert in iter.flatten() {
                if roots.add(cert).is_ok() {
                    added += 1;
                }
            }
        }
        if added == 0 {
            if let Some(o) = out {
                o.warn(&format!("{var} is set to {path}, but no certificate could be read from it."));
            }
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS 1.2 and 1.3 are supported")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

fn tls_problem(err: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(r) = e.downcast_ref::<rustls::Error>() {
            return Some(r.to_string());
        }
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if let Some(inner) = io.get_ref() {
                if let Some(r) = inner.downcast_ref::<rustls::Error>() {
                    return Some(r.to_string());
                }
            }
        }
        cur = e.source();
    }
    None
}

fn io_kind(err: &(dyn std::error::Error + 'static)) -> Option<std::io::ErrorKind> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
        cur = e.source();
    }
    None
}

fn transport_error(t: &ureq::Transport, target: &Url, proxy: Option<&Url>) -> MagineError {
    use std::io::ErrorKind as Io;
    use ureq::ErrorKind as K;
    let host = host_port(target);
    let proxy_host = proxy.map(host_port).unwrap_or_default();
    match t.kind() {
        K::Dns => return MagineError::network(format!("Could not reach {host}. Check your internet connection.")),
        K::ProxyUnauthorized => {
            return MagineError::new(format!("The proxy {proxy_host} refused the tunnel (HTTP 407)."), exit::NETWORK, "proxy")
                .hint("Check the user name and password in HTTPS_PROXY.")
        }
        K::ProxyConnect => return MagineError::new(format!("The proxy {proxy_host} refused the tunnel."), exit::NETWORK, "proxy"),
        K::InvalidProxyUrl => return MagineError::config("HTTPS_PROXY is not a valid proxy address (use http://host:port)."),
        _ => {}
    }
    if let Some(problem) = tls_problem(t) {
        return MagineError::new(format!("The TLS certificate of {host} could not be verified ({problem})."), exit::NETWORK, "tls")
            .hint("Behind a company proxy that inspects TLS? Point MAGINE_CA_CERTS at its certificate file (PEM).");
    }
    let what = match (io_kind(t), proxy) {
        (Some(Io::ConnectionRefused), Some(_)) => format!("The proxy {proxy_host} refused the connection."),
        (Some(Io::ConnectionRefused), None) => format!("{host} refused the connection."),
        (Some(Io::TimedOut) | Some(Io::WouldBlock), _) => format!("The connection to {host} timed out. Try again."),
        (Some(Io::ConnectionReset) | Some(Io::ConnectionAborted) | Some(Io::BrokenPipe) | Some(Io::UnexpectedEof), _) => {
            format!("The connection to {host} dropped. Try again.")
        }
        _ => format!("Request to {host} failed: {t}"),
    };
    MagineError::network(what)
}

impl Http {
    pub fn new(env: &Env, out: Option<&Output>) -> Self {
        Http { tls: tls_config(env, out), env: env.clone(), agents: Mutex::new(HashMap::new()) }
    }

    fn agent(&self, proxy: Option<&Url>) -> Result<ureq::Agent> {
        let id = proxy.map(|p| p.as_str().to_string()).unwrap_or_default();
        let mut agents = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = agents.get(&id) {
            return Ok(a.clone());
        }
        let mut b = ureq::AgentBuilder::new().tls_config(self.tls.clone()).redirects(0).timeout_connect(Duration::from_secs(20));
        if let Some(p) = proxy {
            if p.scheme() != "http" {
                return Err(MagineError::config("Only http:// proxies are supported in HTTPS_PROXY (the tunnel to Magine is still encrypted)."));
            }
            let auth = if p.username().is_empty() {
                String::new()
            } else {
                let dec = |s: &str| url::form_urlencoded::parse(format!("x={s}").as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or_default();
                format!("{}:{}@", dec(p.username()), dec(p.password().unwrap_or("")))
            };
            let spec = format!("http://{auth}{}", host_port(p));
            b = b.proxy(ureq::Proxy::new(&spec).map_err(|_| MagineError::config("HTTPS_PROXY is not a valid proxy address (use http://host:port)."))?);
        }
        let a = b.build();
        agents.insert(id, a.clone());
        Ok(a)
    }

    pub fn request(&self, r: Request) -> Result<Response> {
        let proxy = proxy_for(r.url, &self.env);
        let agent = self.agent(proxy.as_ref())?;
        let mut req = agent.request(r.method, r.url.as_str()).timeout(r.timeout);
        for (k, v) in &r.headers {
            req = req.set(k, v);
        }
        let sent = match &r.body {
            Some(b) => req.send_bytes(b),
            None => req.call(),
        };
        let resp = match sent {
            Ok(resp) => resp,
            Err(ureq::Error::Status(_, resp)) => resp,
            Err(ureq::Error::Transport(t)) => return Err(transport_error(&t, r.url, proxy.as_ref())),
        };
        let status = resp.status();
        let mut headers = HashMap::new();
        for name in resp.headers_names() {
            if let Some(v) = resp.header(&name) {
                headers.insert(name.to_ascii_lowercase(), v.to_string());
            }
        }
        let max = r.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
        let too_big = || MagineError::new(format!("The answer from {} is larger than {} MB.", host_port(r.url), max / 1024 / 1024), exit::NETWORK, "too_large");
        let read_err = |e: std::io::Error| transport_like(e, r.url);
        let mut reader = resp.into_reader();
        if let (Some(sink), true) = (r.sink, (200..300).contains(&status)) {
            let mut buf = vec![0u8; 64 * 1024];
            let mut size = 0u64;
            loop {
                let n = reader.read(&mut buf).map_err(read_err)?;
                if n == 0 {
                    break;
                }
                size += n as u64;
                if size > max {
                    return Err(too_big());
                }
                sink.write_all(&buf[..n]).map_err(|e| MagineError::failed(format!("Could not write the file: {e}")))?;
            }
            sink.flush().map_err(|e| MagineError::failed(format!("Could not write the file: {e}")))?;
            return Ok(Response { status, headers, body: Vec::new() });
        }
        let mut body = Vec::new();
        reader.take(max + 1).read_to_end(&mut body).map_err(read_err)?;
        if body.len() as u64 > max {
            return Err(too_big());
        }
        Ok(Response { status, headers, body })
    }
}

fn transport_like(e: std::io::Error, target: &Url) -> MagineError {
    let host = host_port(target);
    if let Some(p) = tls_problem(&e) {
        return MagineError::new(format!("The TLS connection to {host} failed ({p})."), exit::NETWORK, "tls");
    }
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => MagineError::network(format!("The connection to {host} timed out. Try again.")),
        _ => MagineError::network(format!("The connection to {host} dropped ({e}). Try again.")),
    }
}

pub fn user_agent() -> String {
    format!("magine-cli/{} ({} {}; rust)", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(u: &str, env: &Env) -> Option<String> {
        proxy_for(&Url::parse(u).unwrap(), env).map(|x| host_port(&x))
    }

    #[test]
    fn proxy_selection_honours_no_proxy_and_never_proxies_this_computer() {
        let env = Env::from_pairs(&[("HTTPS_PROXY", "http://proxy.corp:8080"), ("NO_PROXY", "internal.corp,.example.org,other.net:8443")]);
        assert_eq!(p("https://magine.cloud", &env).as_deref(), Some("proxy.corp:8080"));
        assert_eq!(p("https://api.internal.corp", &env), None);
        assert_eq!(p("https://a.example.org", &env), None);
        assert_eq!(p("https://other.net", &env).as_deref(), Some("proxy.corp:8080"));
        assert_eq!(p("https://other.net:8443", &env), None);
        assert_eq!(p("https://localhost:3000", &env), None);
        assert_eq!(p("https://127.0.0.1", &env), None);
        assert_eq!(p("https://magine.cloud", &Env::from_pairs(&[("https_proxy", "proxy:3128")])).as_deref(), Some("proxy:3128"));
        assert_eq!(p("https://magine.cloud", &Env::from_pairs(&[("HTTPS_PROXY", "http://p:1"), ("NO_PROXY", "*")])), None);
        assert_eq!(p("https://magine.cloud", &Env::from_pairs(&[])), None);
        assert_eq!(p("https://magine.cloud", &Env::from_pairs(&[("HTTPS_PROXY", "socks5://p:1")])), None);
    }

    #[test]
    fn user_agent_names_the_cli_and_version() {
        assert!(user_agent().starts_with(&format!("magine-cli/{} (", env!("CARGO_PKG_VERSION"))));
    }
}
