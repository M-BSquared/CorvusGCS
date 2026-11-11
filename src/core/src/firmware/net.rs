//! The network the firmware catalogue reaches out to, behind a trait so a test
//! can stand a stub server in for GitHub and for ArduPilot's server.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

/// One answer being read.
pub trait Reply: Send {
    /// Up to `n` bytes; an empty vector is the end of the body.
    fn read(&mut self, n: usize) -> Result<Vec<u8>, String>;
    /// The `Content-Length` the server sent, when it sent one.
    fn content_length(&self) -> Option<i64>;
}

/// A `GET`. Errors carry the text the operator reads inside the parentheses of
/// "could not reach ... (text)".
pub trait Net: Send + Sync {
    fn get(&self, url: &str, accept: &str, timeout: f64) -> Result<Box<dyn Reply>, String>;
}

/// Reads a whole body up to `limit` bytes plus one, so a caller can tell a body
/// that is too long without holding it.
pub fn read_up_to(reply: &mut dyn Reply, limit: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    while out.len() <= limit {
        let chunk = reply.read(64 * 1024)?;
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    out.truncate(limit + 1);
    Ok(out)
}

type Rewrite = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// HTTP and HTTPS through `ureq`. `rewrite` maps a URL before it is requested,
/// so a test can point the allow listed hosts at a server on this computer.
#[derive(Default, Clone)]
pub struct HttpNet {
    rewrite: Option<Rewrite>,
}

impl HttpNet {
    pub fn new() -> HttpNet {
        HttpNet::default()
    }

    pub fn with_rewrite(rewrite: impl Fn(&str) -> String + Send + Sync + 'static) -> HttpNet {
        HttpNet {
            rewrite: Some(Arc::new(rewrite)),
        }
    }
}

struct HttpReply {
    reader: Box<dyn Read + Send>,
    length: Option<i64>,
}

impl Reply for HttpReply {
    fn read(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; n.max(1)];
        let got = self.reader.read(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(got);
        Ok(buf)
    }

    fn content_length(&self) -> Option<i64> {
        self.length
    }
}

impl Net for HttpNet {
    fn get(&self, url: &str, accept: &str, timeout: f64) -> Result<Box<dyn Reply>, String> {
        let url = match &self.rewrite {
            Some(f) => f(url),
            None => url.to_string(),
        };
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs_f64(timeout.max(0.001))))
            .build()
            .into();
        let response = agent
            .get(&url)
            .header("User-Agent", &format!("CorvusGCS/{}", crate::version::version()))
            .header("Accept", accept)
            .call()
            .map_err(|e| e.to_string())?;
        let length = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<i64>().ok());
        Ok(Box::new(HttpReply {
            reader: Box::new(response.into_body().into_reader()),
            length,
        }))
    }
}
