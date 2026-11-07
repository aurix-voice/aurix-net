//! Tiny IP/CIDR matcher (no extra dependency) for trusted proxies and SSRF checks.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNet {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl IpNet {
    /// `"10.0.0.0/8"`, `"192.168.1.5"` (a /32), `"fd00::/8"`.
    pub fn parse(s: &str) -> Option<Self> {
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a.trim(), Some(p.trim().parse::<u8>().ok()?)),
            None => (s.trim(), None),
        };
        let addr: IpAddr = a.parse().ok()?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = p.unwrap_or(max);
        (prefix <= max).then_some(Self { addr, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let m = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.prefix)
                };
                (u32::from(n) & m) == (u32::from(a) & m)
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let m = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.prefix)
                };
                (u128::from(n) & m) == (u128::from(a) & m)
            }
            (IpAddr::V4(_), IpAddr::V6(a)) => a
                .to_ipv4_mapped()
                .is_some_and(|v4| self.contains(IpAddr::V4(v4))),
            _ => false,
        }
    }
}

/// True for addresses a server may legitimately be asked to call: not loopback, private,
/// link-local (incl. cloud metadata 169.254.169.254), CGNAT, multicast, documentation or
/// unspecified ranges.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            is_public_v6(v6)
        }
    }
}

fn is_public_v4(a: Ipv4Addr) -> bool {
    let o = a.octets();
    !(a.is_private()
        || a.is_loopback()
        || a.is_link_local()
        || a.is_unspecified()
        || a.is_broadcast()
        || a.is_documentation()
        || a.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || o[0] >= 240)
}

fn is_public_v6(a: Ipv6Addr) -> bool {
    let s = a.segments();
    !(a.is_loopback()
        || a.is_unspecified()
        || a.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
        || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || (s[0] == 0x0064 && s[1] == 0xff9b)) // NAT64 well-known prefix
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cidr_and_public() {
        let n = IpNet::parse("10.0.0.0/8").unwrap();
        assert!(n.contains("10.3.4.5".parse().unwrap()));
        assert!(!n.contains("11.0.0.1".parse().unwrap()));
        assert!(IpNet::parse("1.2.3.4")
            .unwrap()
            .contains("1.2.3.4".parse().unwrap()));
        assert!(IpNet::parse("1.2.3.4/33").is_none());
        for bad in [
            "127.0.0.1",
            "10.1.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.1.1",
            "192.168.0.9",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public_ip(bad.parse().unwrap()), "{bad}");
        }
        for ok in ["8.8.8.8", "2606:4700::1111", "93.184.216.34"] {
            assert!(is_public_ip(ok.parse().unwrap()), "{ok}");
        }
    }
}
