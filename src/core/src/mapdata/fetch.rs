//! The one way the map data area talks to the network: a blocking GET or POST
//! with a timeout and a body cap, answering the status instead of failing on
//! it (what `urllib` raising `HTTPError` is to the Python callers).
//!
//! No thread, socket or file handle outlives a call, so there is nothing for
//! the shutdown path to join; a caller that wants to stop early checks its own
//! stop flag between calls.

use std::time::Duration;

/// What the server answered.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub status: u16,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl Fetched {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A request that never got an answer (no route, refused, timed out, a body
/// over the cap, a protocol error).
#[derive(Debug, Clone)]
pub struct NetError(pub String);

/// The text of the error a body past the cap makes.
pub const TOO_LARGE: &str = "the answer is larger than the cap";

impl NetError {
    /// The answer was longer than `Req::max_body`.
    pub fn is_too_large(&self) -> bool {
        self.0 == TOO_LARGE
    }
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One outgoing request.
#[derive(Debug, Clone)]
pub struct Req<'a> {
    pub url: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: Option<&'a [u8]>,
    pub timeout: Duration,
    /// The most bytes of body read; more is an error, as a body that is not
    /// the thing asked for.
    pub max_body: usize,
}

impl<'a> Req<'a> {
    pub fn get(url: &'a str, timeout: Duration) -> Req<'a> {
        Req {
            url,
            headers: &[],
            body: None,
            timeout,
            max_body: 64 * 1024 * 1024,
        }
    }
}

/// The network, behind a trait so a test can stand a stub in for it.
pub trait Fetch: Send + Sync {
    fn send(&self, req: &Req<'_>) -> Result<Fetched, NetError>;
}

/// The real network: HTTP and HTTPS through `ureq`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Http;

impl Fetch for Http {
    fn send(&self, req: &Req<'_>) -> Result<Fetched, NetError> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(req.timeout))
            .build()
            .into();
        let result = match req.body {
            None => {
                let mut r = agent.get(req.url);
                for (k, v) in req.headers {
                    r = r.header(*k, *v);
                }
                r.call()
            }
            Some(body) => {
                let mut r = agent.post(req.url);
                for (k, v) in req.headers {
                    r = r.header(*k, *v);
                }
                r.send(body)
            }
        };
        let mut resp = result.map_err(|e| NetError(e.to_string()))?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body = resp
            .body_mut()
            .with_config()
            .limit(req.max_body as u64 + 1)
            .read_to_vec()
            .map_err(|e| NetError(e.to_string()))?;
        if body.len() > req.max_body {
            return Err(NetError(TOO_LARGE.into()));
        }
        Ok(Fetched { status, body, headers })
    }
}
