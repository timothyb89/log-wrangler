use std::path::PathBuf;
use std::sync::Arc;

use color_eyre::eyre::eyre;
use color_eyre::Result;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Raw JSON output from `tsh app config <name> --format=json`.
#[derive(serde::Deserialize)]
pub struct TshAppConfig {
    pub name: String,
    pub uri: url::Url,
    pub ca: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Ready-to-use TLS assets for a Teleport app-proxy connection.
///
/// Both fields are cheaply cloneable (backed by `Arc` internally).
#[derive(Clone)]
pub struct TeleportTlsConfig {
    /// Teleport app name, used for display in the source list.
    pub app_name: String,
    /// Pre-built reqwest client with mTLS + Teleport CA.
    pub http_client: reqwest::Client,
    /// Pre-built rustls config for WSS connections to the same app.
    pub rustls_config: Arc<rustls::ClientConfig>,
}

/// Shell out to `tsh app config <app_name> --format=json` and parse the result.
///
/// Returns a descriptive error (with a `tsh app login` hint) if credentials
/// have expired or the app is not found.
pub fn fetch_tsh_app_config(app_name: &str) -> Result<TshAppConfig> {
    let output = std::process::Command::new("tsh")
        .args(["app", "config", app_name, "--format=json"])
        .output()
        .map_err(|e| {
            eyre!(
                "Failed to run `tsh app config {}`: {}. Is `tsh` installed and on PATH?",
                app_name,
                e
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(eyre!(
            "`tsh app config {}` failed (exit {}):\n  {}\n\nRun `tsh app login {}` to renew credentials.",
            app_name,
            output.status,
            stderr,
            app_name,
        ));
    }

    let config: TshAppConfig = serde_json::from_slice(&output.stdout)
        .map_err(|e| eyre!("Failed to parse `tsh app config` JSON output: {}", e))?;

    Ok(config)
}

/// Build a [`TeleportTlsConfig`] from parsed `tsh app config` output.
///
/// Reads the cert, key, and CA PEM files from disk and constructs both the
/// reqwest HTTP client (mTLS + custom CA) and a rustls `ClientConfig` for
/// WebSocket (WSS) connections to the same app.
pub fn build_tls_config(tsh: &TshAppConfig) -> Result<TeleportTlsConfig> {
    let cert_pem = std::fs::read(&tsh.cert)
        .map_err(|e| eyre!("Failed to read Teleport cert {:?}: {}", tsh.cert, e))?;
    let key_pem = std::fs::read(&tsh.key)
        .map_err(|e| eyre!("Failed to read Teleport key {:?}: {}", tsh.key, e))?;
    let ca_pem = std::fs::read(&tsh.ca)
        .map_err(|e| eyre!("Failed to read Teleport CA {:?}: {}", tsh.ca, e))?;

    // --- reqwest client with mTLS ---
    // reqwest::Identity::from_pem expects cert and key concatenated in one buffer.
    let mut identity_pem = cert_pem.clone();
    identity_pem.extend_from_slice(&key_pem);
    let identity = reqwest::Identity::from_pem(&identity_pem)
        .map_err(|e| eyre!("Failed to build reqwest identity from Teleport cert/key: {}", e))?;
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem)
        .map_err(|e| eyre!("Failed to parse Teleport CA cert for reqwest: {}", e))?;
    let http_client = reqwest::ClientBuilder::new()
        .use_rustls_tls()
        .identity(identity)
        .add_root_certificate(ca_cert)
        .build()
        .map_err(|e| eyre!("Failed to build Teleport HTTP client: {}", e))?;

    // --- rustls ClientConfig for WSS ---
    // Start with native system roots so publicly-signed Teleport proxy certs
    // (e.g. Let's Encrypt on Teleport Cloud) are trusted, then add the
    // cluster's internal CA so self-signed clusters are also accepted.
    let native_roots = rustls_native_certs::load_native_certs();
    for err in &native_roots.errors {
        tracing::warn!("Failed to load a native root certificate: {}", err);
    }
    let mut root_store = rustls::RootCertStore::empty();
    for cert in native_roots.certs {
        let _ = root_store.add(cert);
    }
    for cert_result in rustls_pemfile::certs(&mut ca_pem.as_slice()) {
        let cert = cert_result
            .map_err(|e| eyre!("Failed to parse CA cert PEM: {}", e))?;
        root_store
            .add(cert.into_owned())
            .map_err(|e| eyre!("Failed to add CA cert to root store: {}", e))?;
    }

    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_pem.as_slice())
            .map(|r| r.map(|c| c.into_owned()))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| eyre!("Failed to parse client cert PEM: {}", e))?;

    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut key_pem.as_slice())
            .map_err(|e| eyre!("Failed to parse private key PEM: {}", e))?
            .ok_or_else(|| eyre!("No private key found in {:?}", tsh.key))?;

    let rustls_client_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_client_auth_cert(certs, key)
        .map_err(|e| eyre!("Failed to build rustls ClientConfig: {}", e))?;

    Ok(TeleportTlsConfig {
        app_name: tsh.name.clone(),
        http_client,
        rustls_config: Arc::new(rustls_client_config),
    })
}
