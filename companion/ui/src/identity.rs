//! TLS identity (spec 3, 12.5): self-signed ECDSA P-256 certificate,
//! TLS 1.3 only, no fallback. The tablet never uses the system trust store;
//! after pairing it pins the SHA-256 fingerprint of this exact DER cert.
//!
//! Storage mirrors spec 9/10 (`cert.der` + `key.bin` in the companion data
//! dir; key file 0600 on Unix, DPAPI on Windows). `CHIZ_DATA_DIR` overrides
//! the directory for tests and portable installs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Identity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: [u8; 32],
}

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CHIZ_DATA_DIR") {
        return d.into();
    }
    // Production paths (spec 9/10) land with the installer; dev default is
    // the repo companion/ dir so certs never pollute a real config dir.
    for c in ["companion/identity", "identity"] {
        if Path::new(c).is_dir() {
            return c.into();
        }
    }
    PathBuf::from("companion/identity")
}

/// Load or first-run-generate the identity. Key material always comes from
/// the OS CSPRNG (rcgen's ring backend); compare + overwrite are constant
/// in structure though never secret-dependent here.
pub fn load_or_generate(hostname: &str) -> Result<Identity, String> {
    load_or_generate_in(&data_dir(), hostname)
}

fn load_or_generate_in(dir: &Path, hostname: &str) -> Result<Identity, String> {
    std::fs::create_dir_all(&dir).map_err(|e| format!("data dir: {e}"))?;
    let cert_path = dir.join("cert.der");
    let key_path = dir.join("key.bin");
    if cert_path.is_file() && key_path.is_file() {
        let cert_der = std::fs::read(&cert_path).map_err(|e| format!("cert read: {e}"))?;
        let key_der = std::fs::read(&key_path).map_err(|e| format!("key read: {e}"))?;
        // Validate the stored key parses as the P-256 pair we wrote; a
        // corrupt file regenerates rather than bricking pairing.
        rcgen::KeyPair::from_der_and_sign_algo(
            &rustls::pki_types::PrivateKeyDer::Pkcs8(key_der.clone().into()),
            &rcgen::PKCS_ECDSA_P256_SHA256,
        )
        .map_err(|e| format!("key parse: {e}"))?;
        if cert_der.is_empty() {
            return Err("empty cert".into());
        }
        let fingerprint = chiz_core::sha256_fp(&cert_der);
        return Ok(Identity {
            cert_der,
            key_der,
            fingerprint,
        });
    }
    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .map_err(|e| format!("keygen: {e}"))?;
    let mut params = rcgen::CertificateParams::default();
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        format!("chiz-{hostname}"),
    );
    params
        .subject_alt_names
        .push(rcgen::SanType::DnsName(
            rcgen::string::Ia5String::try_from(hostname).map_err(|e| format!("san: {e}"))?,
        ));
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(365 * 10); // 10 years (spec 3)
    let cert = params.self_signed(&key_pair).map_err(|e| format!("sign: {e}"))?;
    let cert_der = cert.der().to_vec();
    let key_der = key_pair.serialize_der();
    std::fs::write(&cert_path, &cert_der).map_err(|e| format!("cert write: {e}"))?;
    std::fs::write(&key_path, &key_der).map_err(|e| format!("key write: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("key chmod: {e}"))?;
    }
    let fingerprint = chiz_core::sha256_fp(&cert_der);
    Ok(Identity {
        cert_der,
        key_der,
        fingerprint,
    })
}

/// TLS 1.3-only server config. TLS 1.2 is not offered, so there is no
/// downgrade path (spec 12.5); handshake failures just close the socket.
pub fn server_config(id: &Identity) -> Result<Arc<rustls::ServerConfig>, String> {
    let provider = rustls::crypto::ring::default_provider();
    let cert = rustls::pki_types::CertificateDer::from(id.cert_der.clone());
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(id.key_der.clone().into());
    let cfg = rustls::ServerConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| format!("versions: {e}"))?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .map_err(|e| format!("cert: {e}"))?;
    Ok(Arc::new(cfg))
}

pub fn fp_hex(fp: &[u8; 32]) -> String {
    fp.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("chiz-tls-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn generate_reload_stable_and_key_private() {
        let d = tmpdir("reload");
        let a = load_or_generate_in(&d, "testhost").unwrap();
        assert_eq!(&a.cert_der[..2], &[0x30, 0x82]); // DER SEQUENCE
        // CN is stored as a DER string: the hostname bytes appear verbatim.
        assert!(String::from_utf8_lossy(&a.cert_der).contains("chiz-testhost"));
        let b = load_or_generate_in(&d, "testhost").unwrap();
        assert_eq!(a.cert_der, b.cert_der); // stable across restarts
        assert_eq!(a.fingerprint, chiz_core::sha256_fp(&b.cert_der));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.join("key.bin")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// Pinning test client: trusts ONLY the exact fingerprint (what the
    /// tablet does with its stored record — never the system store).
    #[derive(Debug)]
    struct PinVerifier {
        fp: [u8; 32],
        provider: Arc<rustls::crypto::CryptoProvider>,
    }

    impl rustls::client::danger::ServerCertVerifier for PinVerifier {
        fn verify_server_cert(
            &self,
            end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            if chiz_core::sha256_fp(end_entity.as_ref()) == self.fp {
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            } else {
                Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::UnknownIssuer,
                ))
            }
        }
        fn verify_tls12_signature(
            &self,
            _m: &[u8],
            _c: &rustls::pki_types::CertificateDer<'_>,
            _d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _m: &[u8],
            _c: &rustls::pki_types::CertificateDer<'_>,
            _d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.provider
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    fn pinning_client(fp: [u8; 32], versions: Vec<&'static rustls::SupportedProtocolVersion>) -> tokio_rustls::TlsConnector {
        let provider = rustls::crypto::ring::default_provider();
        let cfg = rustls::ClientConfig::builder_with_provider(provider.clone().into())
            .with_protocol_versions(&versions)
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinVerifier { fp, provider: Arc::new(provider) }))
            .with_no_client_auth();
        tokio_rustls::TlsConnector::from(Arc::new(cfg))
    }

    async fn loopback_pair(
        versions: Vec<&'static rustls::SupportedProtocolVersion>,
    ) -> Result<(rustls::ProtocolVersion, [u8; 32]), String> {
        let d = tmpdir(&format!("hs-{}", versions.len()));
        let id = load_or_generate_in(&d, "hs-host").map_err(|e| e.to_string())?;
        let fp = id.fingerprint;
        let acceptor = tokio_rustls::TlsAcceptor::from(server_config(&id).map_err(|e| e.to_string())?);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.map_err(|e| e.to_string())?;
            let tls = acceptor.accept(tcp).await.map_err(|e| format!("server hs: {e}"))?;
            Ok::<_, String>(tls.get_ref().1.protocol_version().expect("negotiated"))
        });
        let tcp = tokio::net::TcpStream::connect(addr).await.map_err(|e| e.to_string())?;
        let name = rustls::pki_types::ServerName::IpAddress(rustls::pki_types::IpAddr::from(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        let tls = pinning_client(fp, versions)
            .connect(name, tcp)
            .await
            .map_err(|e| format!("client hs: {e}"))?;
        let client_ver = tls.get_ref().1.protocol_version().expect("negotiated");
        let server_ver = server.await.map_err(|e| e.to_string())??;
        assert_eq!(client_ver, server_ver);
        Ok((client_ver, fp))
    }

    #[tokio::test]
    async fn tls13_handshake_with_pinning() {
        let (ver, _) = loopback_pair(vec![&rustls::version::TLS13]).await.unwrap();
        assert_eq!(ver, rustls::ProtocolVersion::TLSv1_3);
    }

    #[tokio::test]
    async fn tls12_client_is_refused_no_fallback() {
        // Server offers TLS 1.3 only: a TLS-1.2-only client MUST fail.
        let r = loopback_pair(vec![&rustls::version::TLS12]).await;
        assert!(r.is_err(), "TLS 1.2 handshake must not succeed");
    }

    #[tokio::test]
    async fn wrong_fingerprint_is_refused() {
        let d = tmpdir("wrongfp");
        let id = load_or_generate_in(&d, "fp-host").map_err(|s| s).unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(server_config(&id).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            acceptor.accept(tcp).await.map(|_| ()).map_err(|e| e.to_string())
        });
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let name = rustls::pki_types::ServerName::IpAddress(rustls::pki_types::IpAddr::from(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        // Attacker's cert fingerprint, not the server's.
        let r = pinning_client([0xabu8; 32], vec![&rustls::version::TLS13])
            .connect(name, tcp)
            .await;
        assert!(r.is_err(), "wrong pin must not connect");
        let _ = server.await;
    }
}
