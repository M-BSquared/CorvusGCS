//! TLS to a TAK server: the system's certificate store plus the CA the operator
//! names, and the client certificate a TAK server asks for. The server is always
//! checked; there is no switch that turns that off.

use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use super::config::Connection;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(6);

fn certs(path: &str, what: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let list: Result<Vec<_>, _> = CertificateDer::pem_file_iter(path)
        .map_err(|e| format!("Could not read the {what} {path}: {e}"))?
        .collect();
    let list = list.map_err(|e| format!("The {what} {path} is not a PEM certificate: {e}"))?;
    if list.is_empty() {
        return Err(format!(
            "The {what} {path} holds no certificate. It must be a PEM file."
        ));
    }
    Ok(list)
}

pub fn config(spec: &Connection) -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    if !spec.ca_file.is_empty() {
        for c in certs(&spec.ca_file, "CA file")? {
            roots
                .add(c)
                .map_err(|e| format!("The CA file {} was refused: {e}", spec.ca_file))?;
        }
    }
    if roots.is_empty() {
        return Err("No trusted certificates: name the server's CA file in the connection.".into());
    }
    let builder = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots);
    let config = match (spec.cert_file.is_empty(), spec.key_file.is_empty()) {
        (true, true) => builder.with_no_client_auth(),
        (false, false) => {
            let chain = certs(&spec.cert_file, "client certificate")?;
            let key = PrivateKeyDer::from_pem_file(&spec.key_file)
                .map_err(|e| format!("Could not read the client key {}: {e}", spec.key_file))?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|e| format!("The client certificate and key do not fit: {e}"))?
        }
        _ => return Err("A client certificate needs both the certificate file and the key file.".into()),
    };
    Ok(Arc::new(config))
}

/// Runs the handshake on `tcp` and returns the stream, or why it failed.
pub fn connect(spec: &Connection, mut tcp: TcpStream) -> Result<StreamOwned<ClientConnection, TcpStream>, String> {
    let name =
        ServerName::try_from(spec.host.clone()).map_err(|_| format!("{} is not a valid server name", spec.host))?;
    let mut conn = ClientConnection::new(config(spec)?, name).map_err(|e| e.to_string())?;
    let _ = tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let _ = tcp.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp)
            .map_err(|e| format!("The TLS handshake with {} failed: {e}", spec.host))?;
    }
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(3)));
    Ok(StreamOwned::new(conn, tcp))
}
