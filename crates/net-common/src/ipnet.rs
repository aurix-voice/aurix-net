//! IP ranges for trusted proxies, API key allow-lists and SSRF checks.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub use ::ipnet::IpNet;

/// `"10.0.0.0/8"`, `"fd00::/8"`, or a single address such as `"192.168.1.5"`.
pub fn parse_net(text: &str) -> Option<IpNet> {
    let text = text.trim();
    text.parse()
        .ok()
        .or_else(|| text.parse::<IpAddr>().ok().map(IpNet::from))
}

/// Whether `ip` is in `net`. An IPv4-mapped IPv6 address counts as the IPv4 address it carries.
pub fn net_contains(net: &IpNet, ip: IpAddr) -> bool {
    net.contains(&ip.to_canonical())
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

fn is_public_v4(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    !(addr.is_private()
        || addr.is_loopback()
        || addr.is_link_local()
        || addr.is_unspecified()
        || addr.is_broadcast()
        || addr.is_documentation()
        || addr.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || octets[0] >= 240)
}

fn is_public_v6(addr: Ipv6Addr) -> bool {
    let segments = addr.segments();
    !(addr.is_loopback()
        || addr.is_unspecified()
        || addr.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
        || (segments[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
        || (segments[0] == 0x2001 && segments[1] == 0x0db8) // documentation
        || (segments[0] == 0x0064 && segments[1] == 0xff9b)) // NAT64 well-known prefix
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cidr_and_public() {
        let net = parse_net("10.0.0.0/8").unwrap();
        assert!(net_contains(&net, "10.3.4.5".parse().unwrap()));
        assert!(net_contains(&net, "::ffff:10.3.4.5".parse().unwrap()));
        assert!(!net_contains(&net, "11.0.0.1".parse().unwrap()));
        let one = parse_net("1.2.3.4").unwrap();
        assert!(net_contains(&one, "1.2.3.4".parse().unwrap()));
        assert!(!net_contains(&one, "1.2.3.5".parse().unwrap()));
        assert!(parse_net("1.2.3.4/33").is_none());
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
