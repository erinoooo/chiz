//! TLS control server (spec 3, 4).
//!
//! One task per connection, built around a single `select!` over:
//! - the next frame from the tablet,
//! - the connection's outbox (frames or a close order from the GUI, the
//!   profile watcher, or a newer session replacing this one),
//! - a 250 ms tick that enforces the handshake and liveness deadlines.
//!
//! There is no shared mailbox to poll and no state outside the `Hub`, so a
//! connection either is the active session or is not, and its end is always
//! logged with a reason.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, WriteHalf};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;

use crate::app::{banner, App};
use crate::hub::{Outbound, PairResult};
use crate::util::{now_secs, os_random, LockExt};
use crate::wire::{self, ClientMsg, FrameReader, DEAD_AFTER, HANDSHAKE_DEADLINE, WRITE_TIMEOUT};

pub async fn serve(app: Arc<App>, listener: TcpListener, acceptor: TlsAcceptor) {
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                // EMFILE and friends: back off instead of spinning a core.
                eprintln!("accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            }
        };
        let app = Arc::clone(&app);
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let conn = match app
                .hub
                .lock_ok()
                .admit(peer.ip(), now_secs(), app.cfg.allow_public)
            {
                Ok(c) => c,
                Err(r) => {
                    eprintln!("{peer}: refused ({r:?})");
                    return;
                }
            };
            let mut slot = PendingSlot {
                app: &app,
                held: true,
            };
            let _ = tcp.set_nodelay(true);
            let tls = match tokio::time::timeout(HANDSHAKE_DEADLINE, acceptor.accept(tcp)).await {
                Ok(Ok(tls)) => tls,
                Ok(Err(e)) => {
                    eprintln!("{peer}: TLS handshake failed: {e}");
                    return;
                }
                Err(_) => {
                    eprintln!("{peer}: TLS handshake timed out");
                    return;
                }
            };
            let reason = Connection::new(&app, conn, peer).run(tls, &mut slot).await;
            // Only the connection that still owns the active session ends
            // it; one that was replaced or forgotten finds nothing here.
            let ended = app.hub.lock_ok().end(conn);
            match ended {
                Some(a) => {
                    app.release_input("session end");
                    eprintln!(
                        "{peer}: session {} ended after {:.0} s ({reason})",
                        a.sid,
                        a.since.elapsed().as_secs_f64()
                    );
                }
                None => eprintln!("{peer}: connection closed ({reason})"),
            }
        });
    }
}

/// Holds one of the hub's 4 unauthenticated slots until the connection
/// authenticates (the hub then frees it) or ends.
struct PendingSlot<'a> {
    app: &'a Arc<App>,
    held: bool,
}

impl Drop for PendingSlot<'_> {
    fn drop(&mut self) {
        if self.held {
            self.app.hub.lock_ok().release_unauth();
        }
    }
}

enum Flow {
    Continue,
    Close(String),
}

struct Connection<'a> {
    app: &'a Arc<App>,
    conn: u64,
    peer: SocketAddr,
    nonce: [u8; 16],
    session: Option<u32>,
    /// Kept so the outbox never reports "all senders gone" before auth.
    out_tx: crate::hub::Outbox,
    out_rx: mpsc::UnboundedReceiver<Outbound>,
}

impl<'a> Connection<'a> {
    fn new(app: &'a Arc<App>, conn: u64, peer: SocketAddr) -> Self {
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let mut nonce = [0u8; 16];
        os_random(&mut nonce);
        Self {
            app,
            conn,
            peer,
            nonce,
            session: None,
            out_tx,
            out_rx,
        }
    }

    /// Drive the connection until it ends; returns the reason.
    async fn run<S>(mut self, stream: S, slot: &mut PendingSlot<'_>) -> String
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (rd, mut wr) = tokio::io::split(stream);
        let mut reader = FrameReader::new(rd);
        let opened = Instant::now();
        if let Err(e) = self.send_challenge(&mut wr).await {
            return e;
        }
        let mut last_rx = Instant::now();
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            let flow = tokio::select! {
                out = self.out_rx.recv() => match out {
                    Some(Outbound::Frame(bytes)) => match write_raw(&mut wr, &bytes).await {
                        Ok(()) => Flow::Continue,
                        Err(e) => Flow::Close(e),
                    },
                    Some(Outbound::Close(why)) => Flow::Close(why),
                    None => Flow::Close("outbox closed".into()),
                },
                frame = reader.next() => match frame {
                    Ok(Some(v)) => {
                        last_rx = Instant::now();
                        self.handle(wire::parse(&v), &mut wr, slot).await
                    }
                    Ok(None) => Flow::Close("closed by the tablet".into()),
                    Err(e) => Flow::Close(e.to_string()),
                },
                _ = tick.tick() => {
                    if self.session.is_none() && opened.elapsed() > HANDSHAKE_DEADLINE {
                        Flow::Close("no authentication within 10 s".into())
                    } else if last_rx.elapsed() > DEAD_AFTER {
                        Flow::Close(format!("nothing received for {} s", DEAD_AFTER.as_secs()))
                    } else {
                        Flow::Continue
                    }
                }
            };
            if let Flow::Close(reason) = flow {
                let _ = tokio::time::timeout(WRITE_TIMEOUT, wr.shutdown()).await;
                return reason;
            }
        }
    }

    async fn send_challenge<W: AsyncWrite + Unpin>(&mut self, wr: &mut W) -> Result<(), String> {
        let open = self.app.hub.lock_ok().pairing.is_open(now_secs());
        let msg = wire::challenge(&self.nonce, &self.app.hostname, open);
        write(wr, &msg).await
    }

    async fn handle<S: AsyncWrite>(
        &mut self,
        msg: ClientMsg,
        wr: &mut WriteHalf<S>,
        slot: &mut PendingSlot<'_>,
    ) -> Flow {
        let result = match msg {
            ClientMsg::Ping { id } => write(wr, &wire::pong(id)).await,
            ClientMsg::Bye => return Flow::Close("tablet said bye".into()),
            ClientMsg::Ignored => Ok(()),
            ClientMsg::Hello {
                device_id,
                auth,
                pen_area,
            } if self.session.is_none() => {
                return self.hello(&device_id, &auth, pen_area, wr, slot).await
            }
            ClientMsg::PairRequest {
                device_id,
                device_name,
                proof,
            } if self.session.is_none() => {
                return self.pair(&device_id, &device_name, proof, wr).await
            }
            ClientMsg::Button { id, down } if self.session.is_some() => {
                self.button(&id, down, wr).await
            }
            ClientMsg::Surface { w, h } if self.session.is_some() => {
                let mut hub = self.app.hub.lock_ok();
                if let Some(a) = hub.active_mut().filter(|a| a.conn == self.conn) {
                    a.aspect = (w, h);
                }
                Ok(())
            }
            // Messages that do not fit the current stage are ignored.
            _ => Ok(()),
        };
        match result {
            Ok(()) => Flow::Continue,
            Err(e) => Flow::Close(e),
        }
    }

    async fn hello<S: AsyncWrite>(
        &mut self,
        device_id: &str,
        auth: &str,
        pen_area: Option<(u32, u32)>,
        wr: &mut WriteHalf<S>,
        slot: &mut PendingSlot<'_>,
    ) -> Flow {
        let accepted = self.app.hub.lock_ok().hello(
            self.conn,
            device_id,
            auth,
            &self.nonce,
            self.peer.ip(),
            pen_area,
            now_secs(),
            self.out_tx.clone(),
        );
        let acc = match accepted {
            Ok(a) => a,
            Err(e) => {
                let _ = write(wr, &e.reply()).await;
                return Flow::Close(format!("hello rejected: {e:?}"));
            }
        };
        slot.held = false; // the hub freed the pending slot
        self.session = Some(acc.sid);
        if let Some(old) = acc.replaced {
            eprintln!(
                "{}: replaces the same tablet's session {} ({})",
                self.peer, old.sid, old.peer
            );
            self.app.end_session(
                old,
                "replaced",
                "replaced by a newer connection from the same tablet",
            );
        }
        let welcome = wire::welcome(acc.sid, self.app.cfg.pen_port, &acc.salt);
        if let Err(e) = write(wr, &welcome).await {
            return Flow::Close(e);
        }
        let profile = self.app.profiles.lock_ok().current_wire("connect");
        if let Some(p) = profile {
            if let Err(e) = write(wr, &p).await {
                return Flow::Close(e);
            }
        }
        eprintln!("{}: session {} started for {device_id}", self.peer, acc.sid);
        Flow::Continue
    }

    async fn pair<S: AsyncWrite>(
        &mut self,
        device_id: &str,
        device_name: &str,
        proof: Option<wire::PairProof>,
        wr: &mut WriteHalf<S>,
    ) -> Flow {
        let result = self.app.hub.lock_ok().pair(
            device_id,
            device_name,
            proof.as_ref(),
            &self.nonce,
            &self.app.fp,
            now_secs(),
        );
        let sent = match result {
            PairResult::Paired(secret) => {
                banner(&format!("Tablet paired: {device_name}"));
                // A fresh challenge: the tablet now authenticates with hello.
                os_random(&mut self.nonce);
                match write(wr, &wire::pair_ok(&secret)).await {
                    Ok(()) => self.send_challenge(wr).await,
                    Err(e) => Err(e),
                }
            }
            PairResult::Failed {
                code,
                attempts_left,
            } => write(wr, &wire::pair_fail(code, attempts_left)).await,
            PairResult::Closed => write(wr, &wire::pair_fail("closed", 0)).await,
            PairResult::NotOpen => {
                let _ = write(wr, &wire::error("not_paired", "no pairing window open")).await;
                return Flow::Close("pair request without an open pairing window".into());
            }
        };
        match sent {
            Ok(()) => Flow::Continue,
            Err(e) => Flow::Close(e),
        }
    }

    async fn button<S: AsyncWrite>(
        &mut self,
        id: &str,
        down: bool,
        wr: &mut WriteHalf<S>,
    ) -> Result<(), String> {
        use chiz_core::action::Effect;
        // Unknown ids are ignored (spec 4). Buttons work while pen is paused.
        let button = {
            let ps = self.app.profiles.lock_ok();
            ps.current()
                .and_then(|p| p.buttons.iter().find(|b| b.id == id).cloned())
        };
        let Some(button) = button else {
            return Ok(());
        };
        let effects = self
            .app
            .runner
            .lock_ok()
            .on_button(&button, if down { "down" } else { "up" });
        if effects.iter().any(|e| matches!(e, Effect::WaitMs(_))) {
            // Macros run on their own task: never block this connection.
            let app = Arc::clone(self.app);
            let fx = effects.clone();
            tokio::spawn(async move {
                for e in &fx {
                    match e {
                        Effect::WaitMs(ms) => {
                            tokio::time::sleep(std::time::Duration::from_millis(*ms as u64)).await
                        }
                        other => app.drive(std::slice::from_ref(other)),
                    }
                }
            });
        } else {
            self.app.drive(&effects);
        }
        for e in &effects {
            match e {
                Effect::ToggleState { id, on } => write(wr, &wire::button_state(id, *on)).await?,
                Effect::SwitchProfile { target } => {
                    let switched = self.app.profiles.lock_ok().switch(target);
                    if let Some((_, reason)) = switched {
                        self.app.release_input("profile change");
                        let msg = self.app.profiles.lock_ok().current_wire(reason);
                        if let Some(m) = msg {
                            write(wr, &m).await?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

async fn write<W: AsyncWrite + Unpin>(wr: &mut W, v: &serde_json::Value) -> Result<(), String> {
    write_raw(wr, &wire::encode(v)).await
}

async fn write_raw<W: AsyncWrite + Unpin>(wr: &mut W, bytes: &[u8]) -> Result<(), String> {
    match tokio::time::timeout(WRITE_TIMEOUT, async {
        wr.write_all(bytes).await?;
        wr.flush().await
    })
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("write failed: {e}")),
        Err(_) => Err("write timed out (tablet stopped reading)".into()),
    }
}

// ------------------------------------------------------------------ addresses

/// Addresses a tablet could reach this PC on, best first, for the QR code.
pub fn lan_hosts() -> Vec<String> {
    // The address the OS would use to reach the LAN goes first.
    let default_route = std::net::UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|sock| {
            // No packets sent: connect() only selects the default route.
            sock.connect("192.0.2.1:80").ok()?;
            match sock.local_addr().ok()?.ip() {
                std::net::IpAddr::V4(v4) => Some(v4),
                _ => None,
            }
        });
    let mut all: Vec<std::net::Ipv4Addr> = default_route.into_iter().collect();
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        all.extend(ifs.into_iter().filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(v4) => Some(v4),
            _ => None,
        }));
    }
    rank_lan_hosts(all)
}

/// Keep only addresses a tablet could reach (never loopback, which is the
/// tablet itself, nor link-local/unspecified), de-duplicated. Home-LAN
/// ranges sort before 172.16/12, where WSL, Docker and Hyper-V adapters live.
fn rank_lan_hosts(addrs: Vec<std::net::Ipv4Addr>) -> Vec<String> {
    let mut out: Vec<std::net::Ipv4Addr> = vec![];
    for a in addrs {
        let usable =
            !a.is_loopback() && !a.is_link_local() && !a.is_unspecified() && !a.is_broadcast();
        if usable && !out.contains(&a) {
            out.push(a);
        }
    }
    let rank = |a: &std::net::Ipv4Addr| match a.octets() {
        [192, 168, ..] => 0,
        [10, ..] => 1,
        [172, 16..=31, ..] => 3,
        _ => 2,
    };
    out.sort_by_key(rank); // stable: the default-route address stays first in its class
    out.truncate(4);
    out.iter().map(|a| a.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing;
    use crate::store::PairedTablet;
    use serde_json::{json, Value};
    use std::time::Duration;
    use tokio::net::TcpStream;
    use tokio_rustls::client::TlsStream;

    #[test]
    fn qr_hosts_skip_loopback_and_rank_lan_first() {
        let ip = |s: &str| s.parse::<std::net::Ipv4Addr>().unwrap();
        let got = rank_lan_hosts(vec![
            ip("172.24.46.253"),
            ip("127.0.0.1"),
            ip("169.254.3.4"),
            ip("192.168.1.20"),
            ip("192.168.1.20"),
            ip("10.0.0.5"),
        ]);
        assert_eq!(got, vec!["192.168.1.20", "10.0.0.5", "172.24.46.253"]);
    }

    // ---------------------------------------------------- live server tests

    /// Accepts any certificate: these tests exercise the session layer, the
    /// pinning itself is covered in identity.rs and on the tablet.
    #[derive(Debug)]
    struct AnyCert(Arc<rustls::crypto::CryptoProvider>);

    impl rustls::client::danger::ServerCertVerifier for AnyCert {
        fn verify_server_cert(
            &self,
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &[rustls::pki_types::CertificateDer<'_>],
            _: &rustls::pki_types::ServerName<'_>,
            _: &[u8],
            _: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    async fn start() -> (Arc<App>, SocketAddr) {
        let dir =
            std::env::temp_dir().join(format!("chiz-srv-{}-{}", std::process::id(), rand_u32()));
        let id = crate::identity::load_or_generate_in(&dir, "testpc").unwrap();
        let acceptor = TlsAcceptor::from(crate::identity::server_config(&id).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = testing::app();
        app.hub.lock_ok().paired.insert(
            "tab-A",
            PairedTablet {
                secret: [1; 32],
                name: "A".into(),
            },
        );
        app.hub.lock_ok().paired.insert(
            "tab-B",
            PairedTablet {
                secret: [2; 32],
                name: "B".into(),
            },
        );
        tokio::spawn(serve(Arc::clone(&app), listener, acceptor));
        (app, addr)
    }

    fn rand_u32() -> u32 {
        let mut b = [0u8; 4];
        os_random(&mut b);
        u32::from_le_bytes(b)
    }

    struct Client {
        reader: FrameReader<tokio::io::ReadHalf<TlsStream<TcpStream>>>,
        writer: WriteHalf<TlsStream<TcpStream>>,
    }

    impl Client {
        async fn connect(addr: SocketAddr) -> Self {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let cfg = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .unwrap()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AnyCert(provider)))
                .with_no_client_auth();
            let tcp = TcpStream::connect(addr).await.unwrap();
            let name = rustls::pki_types::ServerName::try_from("chiz").unwrap();
            let tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
                .connect(name, tcp)
                .await
                .unwrap();
            let (r, w) = tokio::io::split(tls);
            Self {
                reader: FrameReader::new(r),
                writer: w,
            }
        }

        async fn send(&mut self, v: Value) {
            self.writer.write_all(&wire::encode(&v)).await.unwrap();
        }

        /// Next frame, or None on close/timeout.
        async fn recv(&mut self) -> Option<Value> {
            tokio::time::timeout(Duration::from_secs(8), self.reader.next())
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
        }

        /// Read until a frame of type `t` (skipping others), None on close.
        async fn expect(&mut self, t: &str) -> Option<Value> {
            loop {
                let v = self.recv().await?;
                if v["t"] == t {
                    return Some(v);
                }
            }
        }

        /// True if the server closes this connection within 2 s (well under
        /// the 5 s liveness deadline, so a timeout cannot pass for a close).
        async fn closed_promptly(&mut self) -> bool {
            let t0 = Instant::now();
            loop {
                match tokio::time::timeout(Duration::from_secs(2), self.reader.next()).await {
                    Err(_) => return false,
                    Ok(Ok(Some(_))) => continue,
                    Ok(_) => return t0.elapsed() < Duration::from_secs(2),
                }
            }
        }

        async fn hello(&mut self, dev: &str, secret: u8) -> Option<Value> {
            let ch = self.expect("challenge").await?;
            let nonce = crate::util::b64_decode(ch["nonce"].as_str()?).ok()?;
            let auth = chiz_core::compute_auth(&[secret; 32], &nonce.try_into().ok()?, dev);
            self.send(json!({"t": "hello", "proto": 1, "device_id": dev, "auth": auth}))
                .await;
            self.recv().await
        }
    }

    #[tokio::test]
    async fn hello_welcome_profile_and_ping() {
        let (app, addr) = start().await;
        let mut c = Client::connect(addr).await;
        let w = c.hello("tab-A", 1).await.unwrap();
        assert_eq!(w["t"], "welcome");
        assert_eq!(c.recv().await.unwrap()["t"], "profile");
        for i in 0..3 {
            c.send(json!({"t": "ping", "id": i})).await;
            assert_eq!(c.expect("pong").await.unwrap()["id"], i);
        }
        assert_eq!(
            app.hub.lock_ok().active().map(|a| a.device_id.clone()),
            Some("tab-A".into())
        );
        // Profile pushes from outside the session reach the tablet.
        app.lock_profile(Some("krita"));
        let p = c.expect("profile").await.unwrap();
        assert_eq!(p["profile"]["id"], "krita");
        assert_eq!(p["reason"], "manual");
    }

    #[tokio::test]
    async fn same_tablet_reconnect_closes_the_old_connection() {
        let (app, addr) = start().await;
        let mut first = Client::connect(addr).await;
        assert_eq!(first.hello("tab-A", 1).await.unwrap()["t"], "welcome");
        let mut second = Client::connect(addr).await;
        assert_eq!(second.hello("tab-A", 1).await.unwrap()["t"], "welcome");
        // The old connection is told why and actively closed.
        assert_eq!(first.expect("error").await.unwrap()["code"], "replaced");
        assert!(first.closed_promptly().await);
        // The new one is the active session and keeps working.
        second.send(json!({"t": "ping", "id": 9})).await;
        assert_eq!(second.expect("pong").await.unwrap()["id"], 9);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(app.hub.lock_ok().active().is_some());
    }

    #[tokio::test]
    async fn other_tablet_is_busy_and_unknown_is_refused() {
        let (_app, addr) = start().await;
        let mut a = Client::connect(addr).await;
        assert_eq!(a.hello("tab-A", 1).await.unwrap()["t"], "welcome");
        let mut b = Client::connect(addr).await;
        let r = b.hello("tab-B", 2).await.unwrap();
        assert_eq!(
            (r["t"].as_str(), r["code"].as_str()),
            (Some("error"), Some("busy"))
        );
        assert!(b.closed_promptly().await); // closed right after the error
        let mut z = Client::connect(addr).await;
        let r = z.hello("tab-Z", 3).await.unwrap();
        assert_eq!(r["code"], "not_paired");
    }

    #[tokio::test]
    async fn qr_pairing_then_hello_on_the_same_connection() {
        let (app, addr) = start().await;
        let (token, _) = app.hub.lock_ok().pairing.open(now_secs());
        let mut c = Client::connect(addr).await;
        c.expect("challenge").await.unwrap();
        c.send(
            json!({"t": "pair_request", "device_id": "new-tab", "device_name": "New",
                      "mode": "qr", "token": crate::util::b64(&token)}),
        )
        .await;
        let ok = c.expect("pair_ok").await.unwrap();
        let secret: [u8; 32] = crate::util::b64_decode(ok["device_secret"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        // A fresh challenge follows; answer it with hello using the secret.
        let ch = c.expect("challenge").await.unwrap();
        let nonce: [u8; 16] = crate::util::b64_decode(ch["nonce"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let auth = chiz_core::compute_auth(&secret, &nonce, "new-tab");
        c.send(json!({"t": "hello", "device_id": "new-tab", "auth": auth}))
            .await;
        assert_eq!(c.expect("welcome").await.unwrap()["t"], "welcome");
        assert_eq!(
            app.hub
                .lock_ok()
                .paired
                .get("new-tab")
                .map(|t| t.name.clone()),
            Some("New".into())
        );
    }

    #[tokio::test]
    async fn forgetting_a_tablet_closes_its_session() {
        let (app, addr) = start().await;
        let mut c = Client::connect(addr).await;
        assert_eq!(c.hello("tab-A", 1).await.unwrap()["t"], "welcome");
        app.forget_tablet("tab-A");
        assert_eq!(c.expect("error").await.unwrap()["code"], "not_paired");
        assert!(c.closed_promptly().await);
        assert!(app.hub.lock_ok().active().is_none());
    }

    #[tokio::test]
    async fn silent_tablet_is_dropped_after_the_liveness_deadline() {
        let (app, addr) = start().await;
        let mut c = Client::connect(addr).await;
        assert_eq!(c.hello("tab-A", 1).await.unwrap()["t"], "welcome");
        let t0 = Instant::now();
        assert!(c.expect("never").await.is_none()); // no pings sent
        let waited = t0.elapsed();
        assert!(waited >= DEAD_AFTER && waited < DEAD_AFTER + Duration::from_secs(2));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(app.hub.lock_ok().active().is_none());
    }
}
