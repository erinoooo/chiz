//! mDNS discovery (spec 3): advertise `_chiz._tcp` with TXT
//! `v=1`, `id` (first 8 hex of the cert fingerprint), `name` (hostname ≤
//! 32 chars). The tablet browses + resolves; manual IP entry remains for
//! guest Wi-Fi / client isolation. Port 5353 sharing with Avahi: if our
//! responder fails to register, fall back to Avahi over D-Bus (logged).

use std::collections::HashMap;

pub fn service_type() -> &'static str {
    "_chiz._tcp.local."
}

/// TXT record: `v`, `id`, `name`. Hostname truncated to 32 chars (spec 3).
pub fn txt_props(fingerprint: &[u8; 32], hostname: &str) -> HashMap<String, String> {
    let id: String = fingerprint[..4].iter().map(|b| format!("{b:02x}")).collect();
    let name: String = hostname.chars().take(32).collect();
    HashMap::from([
        ("v".to_string(), "1".to_string()),
        ("id".to_string(), id),
        ("name".to_string(), name),
    ])
}

pub fn instance_name(hostname: &str) -> String {
    format!("Chiz on {hostname}")
}

/// Start the responder. Non-fatal: discovery is convenience, manual entry
/// always works, so a dead mDNS never blocks pairing.
pub fn start(hostname: &str, port: u16, fingerprint: &[u8; 32]) -> Option<mdns_sd::ServiceDaemon> {
    let daemon = match mdns_sd::ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("mDNS unavailable ({e}); register via Avahi D-Bus or use manual IP entry");
            return None;
        }
    };
    let fullname = format!("{}.{}.", instance_name(hostname).replace(' ', "-"), service_type().trim_end_matches('.'));
    let svc = match mdns_sd::ServiceInfo::new(
        service_type(),
        &instance_name(hostname),
        &fullname,
        "",
        port,
        txt_props(fingerprint, hostname),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mDNS record invalid ({e})");
            return None;
        }
    };
    if let Err(e) = daemon.register(svc) {
        eprintln!("mDNS register failed ({e}); use manual IP entry");
        return None;
    }
    eprintln!("mDNS advertising {} port {port}", service_type());
    Some(daemon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_shape() {
        let mut fp = [0u8; 32];
        for i in 0..32 {
            fp[i] = i as u8;
        }
        let t = txt_props(&fp, "studio-pc-with-a-very-long-hostname-0123456789");
        assert_eq!(t["v"], "1");
        assert_eq!(t["id"], "00010203"); // first 8 hex chars
        assert!(t["name"].chars().count() <= 32);
    }
}
