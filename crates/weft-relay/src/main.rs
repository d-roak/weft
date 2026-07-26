//! `weft-relay` — run your own relay server for a weft fabric.
//!
//! A relay is the rendezvous point and fallback data path between two nodes:
//! peers reach each other through it immediately, then hole-punch a direct
//! path in the background. Running your own means the fabric depends on no
//! third-party infrastructure.
//!
//! Two modes:
//!
//! - **Plain HTTP** (default) — no certificates. Correct behind a TLS-
//!   terminating proxy/load balancer, on a private network, or for testing.
//!   Point nodes at `http://<host>:<port>`.
//! - **TLS** — pass `--tls-cert`/`--tls-key` to serve HTTPS directly and
//!   enable QUIC address discovery. Point nodes at `https://<host>`.
//!
//! ponytail: no ACME/Let's Encrypt mode — use certs from your existing issuer
//! (certbot, cert-manager, your CA), or upstream's `iroh-relay` binary if you
//! want the relay itself to do ACME.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::Parser;
use iroh_relay::server::{
    AllowAll, CertConfig, QuicConfig, RelayConfig, Server, ServerConfig, TlsConfig,
};

/// Default HTTP port. Unprivileged so the relay runs as a normal user;
/// override with `--http-bind` (use `:80` if you want the bare-host URL form).
const DEFAULT_HTTP_BIND: &str = "[::]:8080";
const DEFAULT_HTTPS_BIND: &str = "[::]:443";

#[derive(Parser, Debug)]
#[command(name = "weft-relay", version, about = "Self-hosted relay server for weft")]
struct Cli {
    /// Address to serve the relay (HTTP) on.
    #[arg(long, env = "WEFT_RELAY_HTTP_BIND", default_value = DEFAULT_HTTP_BIND)]
    http_bind: SocketAddr,

    /// TLS certificate chain (PEM). Enables HTTPS + QUIC address discovery.
    #[arg(long, env = "WEFT_RELAY_TLS_CERT", requires = "tls_key")]
    tls_cert: Option<PathBuf>,

    /// TLS private key (PEM).
    #[arg(long, env = "WEFT_RELAY_TLS_KEY", requires = "tls_cert")]
    tls_key: Option<PathBuf>,

    /// Address to serve HTTPS on (only with TLS).
    #[arg(long, env = "WEFT_RELAY_HTTPS_BIND", default_value = DEFAULT_HTTPS_BIND)]
    https_bind: SocketAddr,

    /// Address for the QUIC address-discovery service (only with TLS).
    #[arg(long, env = "WEFT_RELAY_QUIC_BIND", default_value = "[::]:7842")]
    quic_bind: SocketAddr,

    /// Serve Prometheus metrics on this address.
    #[arg(long, env = "WEFT_RELAY_METRICS_BIND")]
    metrics_bind: Option<SocketAddr>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    // rustls needs a process-wide crypto provider before building any config.
    rustls::crypto::ring::default_provider().install_default().ok();

    let tls = match (&cli.tls_cert, &cli.tls_key) {
        (Some(cert), Some(key)) => Some(load_tls(cert, key)?),
        _ => None,
    };

    // These config structs are `#[non_exhaustive]`, so build via `new()` and
    // then set the fields we care about.
    let mut relay = RelayConfig::new(cli.http_bind);
    relay.key_cache_capacity = Some(1024);
    // ponytail: open relay. Add an allowlist here if you need to restrict
    // which endpoints may use your infrastructure.
    relay.access = Arc::new(AllowAll);
    if let Some(server_config) = &tls {
        relay.tls = Some(TlsConfig::new(
            cli.https_bind,
            CertConfig::Manual { server_config: server_config.clone() },
        ));
    }

    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    config.metrics_addr = cli.metrics_bind;
    // QUIC address discovery helps peers learn their public address; it needs
    // TLS, so it's only enabled when certs are supplied.
    if let Some(server_config) = tls {
        let mut quic = QuicConfig::new(cli.quic_bind);
        quic.server_config = Some(server_config);
        config.quic = Some(quic);
    }

    let server = Server::spawn(config).await.context("starting relay server")?;

    if let Some(addr) = server.http_addr() {
        println!("weft-relay: http  {addr}");
    }
    if let Some(addr) = server.https_addr() {
        println!("weft-relay: https {addr}");
    }
    if let Some(addr) = server.quic_addr() {
        println!("weft-relay: quic  {addr}");
    }
    println!("point nodes at this relay with: weft start --relay <url>");

    tokio::signal::ctrl_c().await?;
    println!("\nshutting down");
    server.shutdown().await.context("shutting down relay")?;
    Ok(())
}

/// Build a rustls server config from PEM cert-chain and key files.
fn load_tls(cert_path: &PathBuf, key_path: &PathBuf) -> Result<rustls::ServerConfig> {
    let cert_pem =
        std::fs::read(cert_path).with_context(|| format!("reading {}", cert_path.display()))?;
    let key_pem =
        std::fs::read(key_path).with_context(|| format!("reading {}", key_path.display()))?;

    let certs = rustls_pemfile::certs(&mut &cert_pem[..]).collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        bail!("no certificates found in {}", cert_path.display());
    }
    let key = rustls_pemfile::private_key(&mut &key_pem[..])?
        .with_context(|| format!("no private key found in {}", key_path.display()))?;

    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("building TLS config")
}
