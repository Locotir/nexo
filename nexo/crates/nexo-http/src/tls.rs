//! The one place an HTTP client is built. The platform root store comes
//! first (so corporate CAs, `SSL_CERT_FILE` and `SSL_CERT_DIR` keep working);
//! when it is empty or cannot be read, as in a bare container image, the
//! bundled Mozilla roots stand in. A failure is an error, never a panic.

use reqwest::{Certificate, Client, ClientBuilder};

/// Builds the client `make` describes. `make` runs again when the first
/// build fails, because a [`ClientBuilder`] cannot be reused.
pub fn build_client(make: impl Fn() -> ClientBuilder) -> Result<Client, String> {
    let native = match make().build() {
        Ok(c) => return Ok(c),
        Err(e) => e,
    };
    tracing::warn!("no usable system CA roots ({native}); using the bundled Mozilla roots");
    let bundled = webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().filter_map(|c| Certificate::from_der(c).ok());
    make().tls_certs_only(bundled).build().map_err(|e| {
        format!("cannot set up TLS: the system CA store failed ({native}) and so did the bundled roots ({e})")
    })
}
