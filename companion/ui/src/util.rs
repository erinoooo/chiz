//! Small shared helpers: randomness, constant-time compare, base64, time,
//! host identity, poison-tolerant locking.

/// Crypto-random bytes from the OS (spec 12.1): getrandom(2) on Linux,
/// BCryptGenRandom on Windows. There is no meaningful fallback for a
/// missing CSPRNG, so this is the one place allowed to abort.
pub fn os_random(buf: &mut [u8]) {
    getrandom::getrandom(buf).expect("OS CSPRNG unavailable");
}

/// Constant-time equality (spec 12.2). No early exit on mismatch.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Standard (padded) base64, as the protocol uses on the wire.
pub fn b64(bytes: &[u8]) -> String {
    const ALPH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = c.iter().fold(0u32, |a, b| (a << 8) | *b as u32) << (8 * (3 - c.len()));
        for i in 0..4 {
            if i <= c.len() {
                out.push(ALPH[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn b64_decode(s: &str) -> Result<Vec<u8>, &'static str> {
    let mut vals = Vec::with_capacity(s.len());
    for c in s.chars() {
        let v = match c {
            '=' => 0,
            'A'..='Z' => c as u8 - b'A',
            'a'..='z' => c as u8 - b'a' + 26,
            '0'..='9' => c as u8 - b'0' + 52,
            '+' => 62,
            '/' => 63,
            _ => return Err("bad b64"),
        };
        vals.push(v);
    }
    if vals.len() % 4 != 0 {
        return Err("bad b64 length");
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in vals.chunks(4) {
        let n = ((c[0] as u32) << 18) | ((c[1] as u32) << 12) | ((c[2] as u32) << 6) | c[3] as u32;
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
    }
    let pad = s.chars().rev().take_while(|&c| c == '=').count();
    out.truncate(out.len().saturating_sub(pad));
    Ok(out)
}

/// Seconds since the Unix epoch (wall clock; only used for coarse windows).
pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// This PC's name for the challenge, the cert CN and mDNS. Every OS sets
/// one of these; the old `/etc/hostname`-only read named every Windows PC
/// "chiz-pc".
pub fn hostname() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_default();
    let name: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(32)
        .collect();
    if name.is_empty() {
        "chiz-pc".into()
    } else {
        name.to_lowercase()
    }
}

/// Poison-tolerant locking for std mutexes. A panic elsewhere must never
/// turn the emergency-release path (or the pen path) into a second panic:
/// the guarded state is plain data and stays usable.
pub trait LockExt<T> {
    fn lock_ok(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockExt<T> for std::sync::Mutex<T> {
    fn lock_ok(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_roundtrip_all_lengths() {
        for n in 0..40u8 {
            let data: Vec<u8> = (0..n).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(b64_decode(&b64(&data)).unwrap(), data, "len {n}");
        }
        assert_eq!(b64(b"hi"), "aGk=");
        assert!(b64_decode("a!==").is_err());
        assert!(b64_decode("abc").is_err());
    }

    #[test]
    fn hostname_is_safe_and_nonempty() {
        let h = hostname();
        assert!(!h.is_empty() && h.len() <= 32);
        assert!(h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
