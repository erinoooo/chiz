//! QR pairing URI codec (spec 3):
//! `chiz://pair?v=1&h=<hosts>&p=<port>&fp=<base64url>&t=<base64url>`.
//! `h` is a comma-separated IPv4 list (tablet tries each, 2 s in order);
//! `fp` is the 32-byte certificate fingerprint, `t` the 16-byte pair token.
//! Mirrors protocol/chiz_proto.py build/parse_qr_uri.

use base64::Engine as _;

pub struct QrPairing {
    pub hosts: Vec<String>,
    pub port: u16,
    pub fp: [u8; 32],
    pub token: [u8; 16],
}

fn b64url_nopad(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64url(s: &str) -> Result<Vec<u8>, &'static str> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| "bad base64url")
}

pub fn build_qr_uri(hosts: &[&str], port: u16, fp: &[u8; 32], token: &[u8; 16]) -> String {
    format!(
        "chiz://pair?v=1&h={}&p={}&fp={}&t={}",
        hosts.join(","),
        port,
        b64url_nopad(fp),
        b64url_nopad(token)
    )
}

pub fn parse_qr_uri(uri: &str) -> Result<QrPairing, &'static str> {
    let q = uri.strip_prefix("chiz://pair?").ok_or("bad QR scheme")?;
    let mut v = None;
    let mut h = None;
    let mut p = None;
    let mut fp = None;
    let mut t = None;
    for kv in q.split('&') {
        let (k, val) = kv.split_once('=').ok_or("bad QR query")?;
        match k {
            "v" => v = Some(val),
            "h" => h = Some(val),
            "p" => p = Some(val),
            "fp" => fp = Some(val),
            "t" => t = Some(val),
            _ => {} // ignore unknown QR params
        }
    }
    if v != Some("1") {
        return Err("bad QR version");
    }
    let hosts: Vec<String> = h.ok_or("missing h")?.split(',').map(str::to_string).collect();
    if hosts.is_empty() || hosts.iter().any(|x| x.is_empty()) {
        return Err("bad hosts");
    }
    let port: u16 = p.ok_or("missing p")?.parse().map_err(|_| "bad port")?;
    let fp_raw = unb64url(fp.ok_or("missing fp")?)?;
    if fp_raw.len() != 32 {
        return Err("bad fp length");
    }
    let tok_raw = unb64url(t.ok_or("missing t")?)?;
    if tok_raw.len() != 16 {
        return Err("bad token length");
    }
    let mut fp_arr = [0u8; 32];
    fp_arr.copy_from_slice(&fp_raw);
    let mut tok_arr = [0u8; 16];
    tok_arr.copy_from_slice(&tok_raw);
    Ok(QrPairing {
        hosts,
        port,
        fp: fp_arr,
        token: tok_arr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_roundtrip_and_shape() {
        let mut fp = [0u8; 32];
        for i in 0..32 {
            fp[i] = 0x20 + i as u8;
        }
        let tok = [7u8; 16];
        let uri = build_qr_uri(&["192.168.1.10", "192.168.1.11"], 47800, &fp, &tok);
        assert!(uri.starts_with("chiz://pair?v=1&h=192.168.1.10,192.168.1.11&p=47800&fp="));
        let back = parse_qr_uri(&uri).unwrap();
        // Base64url values (no padding): fp/t segments carry no +/=.
        for kv in uri.split('?').nth(1).unwrap().split('&') {
            let (k, v) = kv.split_once('=').unwrap();
            if k == "fp" || k == "t" {
                assert!(!v.contains(['+', '/', '=']), "{k}={v}");
            }
        }
        assert_eq!(back.hosts, vec!["192.168.1.10", "192.168.1.11"]);
        assert_eq!(back.port, 47800);
        assert_eq!(back.fp, fp);
        assert_eq!(back.token, tok);
    }

    #[test]
    fn qr_rejects_garbage() {
        assert!(parse_qr_uri("https://x/?v=1").is_err());
        assert!(parse_qr_uri("chiz://pair?v=2&h=a&p=1&fp=AA&t=AA").is_err());
        assert!(parse_qr_uri("chiz://pair?v=1&h=&p=1&fp=AA&t=AA").is_err());
    }

    #[test]
    fn fp_is_sha256_of_der() {
        let fp = crate::sha256_fp(b"fake-der");
        assert_eq!(fp.len(), 32);
        assert_eq!(fp, crate::sha256_fp(b"fake-der"));
        assert_ne!(fp, crate::sha256_fp(b"other"));
    }
}
