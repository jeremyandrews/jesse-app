//! **THE PER-INSTALL CA** — the certificate authority the egress proxy terminates TLS with.
//!
//! On `allowlist` and `open-logged` turns the egress proxy reads every request before it
//! leaves (the read/send split and the content check need the method, the URL and the body),
//! so a `CONNECT` tunnel is terminated rather than spliced: the child's TLS client completes
//! its handshake with the PROXY, against a leaf certificate for the host it asked for, and the
//! proxy opens its own TLS connection to the origin. The child accepts that leaf only because
//! it was told to trust this CA (its trust variables name [`ProxyCa::cert_path`]).
//!
//! ---- WHY PER INSTALL, AND WHY ON DISK ---------------------------------------------
//!
//! A CA that can mint a certificate for any host is the most dangerous key the bridge holds:
//! anyone with it can impersonate any site to anything that trusts it. So it is minted HERE,
//! on first start, and never shipped, shared or copied: one Studio's CA is worth nothing
//! against another Studio's children. It lives under the state directory because a CA that
//! changed on every restart would invalidate the trust every running child was configured
//! with, and because a child's trust file has to name a stable path.
//!
//! ---- WHAT IS WHERE ----------------------------------------------------------------
//!
//!   * `<state_dir>/proxy-ca/` (`0700`): the CA's own directory.
//!   * `<state_dir>/proxy-ca/ca.key` (`0600`): the PKCS#8 private key, ECDSA P-256. It never
//!     leaves this directory, is never logged, and [`ProxyCa`]'s `Debug` prints none of it.
//!   * `<state_dir>/proxy-ca/ca.pem`: the CA certificate, the bridge's own copy.
//!   * `<state_dir>/proxy-ca-cert.pem` (`0644`): the SAME certificate, outside the private
//!     directory, so a child running as another user can read it. The certificate is public
//!     by nature; only the key is secret, and the key is not in this file.
//!
//! ---- RELOADING WITHOUT A CERTIFICATE PARSER -----------------------------------------
//!
//! rcgen 0.13 can only re-read a CA certificate with its `x509-parser` feature, which would
//! add a parser to the graph for one call. It does not need to: signing a leaf uses the
//! issuer's distinguished name, key-identifier method and key usages (all fixed constants in
//! [`ca_params`]) and the issuer's KEY (on disk). So a restart rebuilds the issuer from the
//! same constants and the same key, and leaves keep chaining to the certificate in `ca.pem`,
//! which is what the chain served to the client carries. The stored certificate is never
//! re-minted, so its bytes (and every child's trust in them) survive restarts.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, SanType, PKCS_ECDSA_P256_SHA256,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// The CA's common name: what an operator sees in a certificate viewer.
pub const CA_COMMON_NAME: &str = "Jesse bridge egress CA";

/// How many per-host server configurations are kept. A turn reaches tens of hosts; a full
/// cache is evicted oldest-first, and an evicted host simply gets a fresh leaf next time.
pub const LEAF_CACHE_MAX: usize = 1024;

/// How long a minted leaf is valid. Short, because a leaf is cheap to mint again and a long
/// one is a longer-lived impersonation if it ever escaped the process (it never touches disk).
const LEAF_VALIDITY_DAYS: i64 = 7;

/// How long the CA is valid. Long, because rotating it means re-trusting it everywhere.
const CA_VALIDITY_DAYS: i64 = 3650;

/// The per-install CA, loaded or minted under the state directory.
pub struct ProxyCa {
    /// The CA's private key. Never printed.
    key: KeyPair,
    /// The issuer rebuilt from [`ca_params`] and `key`: what leaves are signed against.
    issuer: rcgen::Certificate,
    /// The CA certificate as stored in `ca.pem`: what the chain carries.
    ca_der: CertificateDer<'static>,
    ca_pem: String,
    /// `<state_dir>/proxy-ca-cert.pem`, the child-readable copy.
    cert_path: PathBuf,
    cache: Mutex<LeafCache>,
}

#[derive(Default)]
struct LeafCache {
    by_host: HashMap<String, Arc<rustls::ServerConfig>>,
    order: VecDeque<String>,
}

impl fmt::Debug for ProxyCa {
    /// The certificate's path and nothing else: no key, no key-derived bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyCa")
            .field("cert_path", &self.cert_path)
            .finish_non_exhaustive()
    }
}

/// The CA's parameters. CONSTANT apart from the validity: the issuer rebuilt on a restart has
/// to carry the same distinguished name and key-identifier method as the stored certificate.
fn ca_params() -> CertificateParams {
    let mut p = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, CA_COMMON_NAME);
    dn.push(DnType::OrganizationName, "Jesse bridge");
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    set_validity(&mut p, -1, CA_VALIDITY_DAYS);
    p
}

/// Valid from midnight UTC `from` days from today to midnight `to` days from today. Day
/// granularity is all a certificate needs, and building the dates with rcgen's own helper
/// from chrono's calendar keeps the `time` crate out of the direct dependencies.
fn set_validity(p: &mut CertificateParams, from: i64, to: i64) {
    use chrono::Datelike;
    let day = |offset: i64| {
        let d = chrono::Utc::now().date_naive() + chrono::Duration::days(offset);
        rcgen::date_time_ymd(d.year(), d.month() as u8, d.day() as u8)
    };
    p.not_before = day(from);
    p.not_after = day(to);
}

fn other(e: impl fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl ProxyCa {
    /// Load the CA under `state_dir`, minting it on first start. Also (re)writes the
    /// child-readable certificate copy, so a deleted or stale copy is repaired on boot.
    pub fn load_or_create(state_dir: &Path) -> io::Result<ProxyCa> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let dir = state_dir.join("proxy-ca");
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)?;
        }
        // A directory made by hand or by an older build is brought to 0700 too.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let key_path = dir.join("ca.key");
        let pem_path = dir.join("ca.pem");

        let (key, ca_pem) = if key_path.exists() && pem_path.exists() {
            let key_pem = std::fs::read_to_string(&key_path)?;
            // The parse error names no key bytes; rcgen's errors are enum variants.
            let key = KeyPair::from_pem(&key_pem).map_err(other)?;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
            (key, std::fs::read_to_string(&pem_path)?)
        } else {
            let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(other)?;
            let cert = ca_params().self_signed(&key).map_err(other)?;
            write_private(&key_path, key.serialize_pem().as_bytes())?;
            let pem = cert.pem();
            write_mode(&pem_path, pem.as_bytes(), 0o644)?;
            (key, pem)
        };
        let ca_der = pem_to_der(&ca_pem)?;
        let issuer = ca_params().self_signed(&key).map_err(other)?;
        let cert_path = state_dir.join("proxy-ca-cert.pem");
        write_mode(&cert_path, ca_pem.as_bytes(), 0o644)?;
        Ok(ProxyCa {
            key,
            issuer,
            ca_der,
            ca_pem,
            cert_path,
            cache: Mutex::new(LeafCache::default()),
        })
    }

    /// The child-readable certificate file: what a child's trust variables name.
    pub fn cert_path(&self) -> &Path {
        &self.cert_path
    }

    /// The CA certificate, PEM.
    pub fn cert_pem(&self) -> &str {
        &self.ca_pem
    }

    /// The CA certificate, DER.
    pub fn cert_der(&self) -> &CertificateDer<'static> {
        &self.ca_der
    }

    /// The TLS server configuration presenting a leaf for `host` (a DNS name, or an IP
    /// literal, which gets an IP SAN). Cached per host; the cache is bounded at
    /// [`LEAF_CACHE_MAX`] and evicts the oldest host.
    pub fn server_config_for(&self, host: &str) -> Result<Arc<rustls::ServerConfig>, String> {
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if host.is_empty() {
            return Err("an empty host has no certificate".into());
        }
        {
            let cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(c) = cache.by_host.get(&host) {
                return Ok(c.clone());
            }
        }
        let config = Arc::new(self.mint(&host)?);
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if !cache.by_host.contains_key(&host) {
            while cache.order.len() >= LEAF_CACHE_MAX {
                if let Some(old) = cache.order.pop_front() {
                    cache.by_host.remove(&old);
                }
            }
            cache.order.push_back(host.clone());
            cache.by_host.insert(host, config.clone());
        }
        Ok(config)
    }

    /// Whether a leaf for `host` has been minted (and is still cached): a test's evidence that
    /// a refused `CONNECT` terminated nothing.
    #[cfg(test)]
    pub(crate) fn has_leaf(&self, host: &str) -> bool {
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .by_host
            .contains_key(host)
    }

    fn mint(&self, host: &str) -> Result<rustls::ServerConfig, String> {
        let mut p = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host);
        p.distinguished_name = dn;
        p.subject_alt_names = vec![match host.parse::<IpAddr>() {
            Ok(ip) => SanType::IpAddress(ip),
            Err(_) => SanType::DnsName(
                host.to_string()
                    .try_into()
                    .map_err(|_| format!("{host:?} is not a DNS name"))?,
            ),
        }];
        p.is_ca = IsCa::ExplicitNoCa;
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.use_authority_key_identifier_extension = true;
        set_validity(&mut p, -1, LEAF_VALIDITY_DAYS);
        let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
        let leaf = p
            .signed_by(&leaf_key, &self.issuer, &self.key)
            .map_err(|e| e.to_string())?;
        let chain = vec![leaf.der().clone(), self.ca_der.clone()];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|e| e.to_string())?;
        // HTTP/1.1 only: the proxy serves the inner stream with hyper's HTTP/1 server, so a
        // client must not be allowed to negotiate h2 with it.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(config)
    }
}

fn pem_to_der(pem: &str) -> io::Result<CertificateDer<'static>> {
    use base64::Engine;
    let body: String = pem
        .lines()
        .skip_while(|l| !l.starts_with("-----BEGIN CERTIFICATE-----"))
        .skip(1)
        .take_while(|l| !l.starts_with("-----END CERTIFICATE-----"))
        .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ca.pem is not a certificate"))?;
    if der.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ca.pem holds no certificate",
        ));
    }
    Ok(CertificateDer::from(der))
}

/// Write a secret: created `0600` from the first byte (never world-readable for an instant).
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

fn write_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, bytes)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub(crate) fn tmp(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "jesse-{tag}-{}-{}-{}",
            std::process::id(),
            crate::activity::now_ms(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn client_trusting(der: Option<&CertificateDer<'static>>) -> rustls::ClientConfig {
        let mut roots = rustls::RootCertStore::empty();
        if let Some(d) = der {
            roots.add(d.clone()).unwrap();
        }
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth()
    }

    /// An in-memory handshake: the server presents `ca`'s leaf for `host`; the client trusts
    /// what `client` trusts. Returns whether the client completed and read the server's byte.
    async fn handshake(ca: &ProxyCa, host: &str, client: rustls::ClientConfig) -> bool {
        let (c, s) = tokio::io::duplex(64 * 1024);
        let acceptor = tokio_rustls::TlsAcceptor::from(ca.server_config_for(host).unwrap());
        let server = tokio::spawn(async move {
            let mut tls = acceptor.accept(s).await.ok()?;
            tls.write_all(b"k").await.ok()?;
            tls.flush().await.ok()
        });
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
        let name = rustls::pki_types::ServerName::try_from(host.to_string()).unwrap();
        let ok = match connector.connect(name, c).await {
            Ok(mut tls) => {
                let mut b = [0u8; 1];
                tls.read_exact(&mut b).await.is_ok() && &b == b"k"
            }
            Err(_) => false,
        };
        let _ = server.await;
        ok
    }

    #[test]
    fn a_reload_is_the_same_ca_and_the_files_are_private() {
        let dir = tmp("ca");
        let a = ProxyCa::load_or_create(&dir).unwrap();
        let b = ProxyCa::load_or_create(&dir).unwrap();
        assert_eq!(a.cert_der(), b.cert_der());
        assert_eq!(a.cert_pem(), b.cert_pem());
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir.join("proxy-ca")), 0o700);
        assert_eq!(mode(&dir.join("proxy-ca/ca.key")), 0o600);
        assert_eq!(mode(a.cert_path()), 0o644);
        let copy = std::fs::read_to_string(a.cert_path()).unwrap();
        assert!(copy.contains("BEGIN CERTIFICATE"));
        assert!(!copy.contains("PRIVATE KEY"), "the child copy holds no key");
        let dbg = format!("{a:?}");
        assert!(!dbg.contains("PRIVATE") && !dbg.contains("key:"), "{dbg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_minted_leaf_verifies_against_the_ca_only() {
        let dir = tmp("ca-leaf");
        let first = ProxyCa::load_or_create(&dir).unwrap();
        // A RELOADED CA's leaves chain to the certificate stored by the first start.
        let ca = ProxyCa::load_or_create(&dir).unwrap();
        let trusting = client_trusting(Some(first.cert_der()));
        assert!(handshake(&ca, "api.example.com", trusting.clone()).await);
        assert!(handshake(&ca, "127.0.0.1", trusting).await, "IP SAN");
        // A client that trusts nothing of ours (a pinned client) refuses the leaf.
        let other = tmp("ca-other");
        let stranger = ProxyCa::load_or_create(&other).unwrap();
        assert!(
            !handshake(
                &ca,
                "api.example.com",
                client_trusting(Some(stranger.cert_der()))
            )
            .await
        );
        assert!(!handshake(&ca, "api.example.com", client_trusting(None)).await);
        // The same host is served from the cache.
        assert!(Arc::ptr_eq(
            &ca.server_config_for("api.example.com").unwrap(),
            &ca.server_config_for("API.example.com.").unwrap()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }
}
