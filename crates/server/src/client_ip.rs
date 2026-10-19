//! The address a request comes from, as the limiter of failed sign-ins keys it.
//!
//! The TCP peer is the answer unless the peer is a trusted reverse proxy; only then is the
//! configured header read. In `X-Forwarded-For` the client is the right-most entry outside the
//! trusted set (never blindly the right-most, which behind a CDN is the CDN's edge); any other
//! header is read as one address. A malformed header from a trusted peer falls back to the peer and
//! logs a warning.

use std::net::{IpAddr, SocketAddr};

use axum::http::{HeaderMap, HeaderName};

/// A block of addresses, `10.0.0.0/8` or `fd00::/8`; a bare address is a block of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Parses `address/prefix` or a bare address.
    ///
    /// # Errors
    /// A sentence saying what is wrong.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (text, None),
        };
        let network = address
            .trim()
            .parse::<IpAddr>()
            .map_err(|_| "it is not an IP address".to_owned())?
            .to_canonical();
        let width = width_of(network);
        let prefix = match prefix {
            None => width,
            Some(text) => text
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|prefix| *prefix <= width)
                .ok_or_else(|| format!("the prefix length is a number from 0 to {width}"))?,
        };
        Ok(Self { network, prefix })
    }

    /// Whether the address lies in the block. An IPv4 address written as IPv6 (`::ffff:10.0.0.1`)
    /// is the IPv4 address.
    pub fn contains(&self, address: IpAddr) -> bool {
        let address = address.to_canonical();
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => same_prefix(
                network.to_bits().into(),
                address.to_bits().into(),
                32,
                self.prefix,
            ),
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                same_prefix(network.to_bits(), address.to_bits(), 128, self.prefix)
            }
            _ => false,
        }
    }
}

fn width_of(address: IpAddr) -> u8 {
    if address.is_ipv4() { 32 } else { 128 }
}

fn same_prefix(a: u128, b: u128, width: u8, prefix: u8) -> bool {
    let host_bits = u32::from(width - prefix);
    let mask = u128::MAX.checked_shl(host_bits).unwrap_or(0);
    let mask = if width == 32 {
        mask & u128::from(u32::MAX)
    } else {
        mask
    };
    a & mask == b & mask
}

/// The reverse proxies whose word on the client's address is taken, and the header they put it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyTrust {
    trusted: Vec<Cidr>,
    header: HeaderName,
}

impl ProxyTrust {
    /// Trust these blocks' word, given in `header`.
    pub fn new(trusted: Vec<Cidr>, header: HeaderName) -> Self {
        Self { trusted, header }
    }

    /// The trusted blocks.
    pub fn trusted(&self) -> &[Cidr] {
        &self.trusted
    }

    /// The header read.
    pub fn header(&self) -> &HeaderName {
        &self.header
    }

    fn is_trusted(&self, address: IpAddr) -> bool {
        self.trusted.iter().any(|cidr| cidr.contains(address))
    }
}

/// Reads the client's address out of the header's values, or `None` when they are malformed.
type HeaderReader = fn(&ProxyTrust, &[&str]) -> Option<IpAddr>;

/// Decides the client address of each request.
#[derive(Debug, Clone)]
pub struct ClientIpPolicy {
    proxy: Option<(ProxyTrust, HeaderReader)>,
}

impl ClientIpPolicy {
    /// The policy for a deployment with these trusted proxies, or none.
    pub fn new(proxy: Option<ProxyTrust>) -> Self {
        let proxy = proxy.map(|trust| {
            let reader: HeaderReader = if trust.header == "x-forwarded-for" {
                read_forwarded_for
            } else {
                read_single
            };
            (trust, reader)
        });
        Self { proxy }
    }

    /// The client address of a request from `peer` carrying `headers`.
    pub fn client(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let peer = peer.ip().to_canonical();
        let Some((trust, reader)) = &self.proxy else {
            return peer;
        };
        if !trust.is_trusted(peer) {
            return peer;
        }
        let values: Option<Vec<&str>> = headers
            .get_all(&trust.header)
            .iter()
            .map(|value| value.to_str().ok())
            .collect();
        match values {
            Some(values) if values.is_empty() => peer,
            Some(values) => reader(trust, &values).unwrap_or_else(|| {
                tracing::warn!(header = %trust.header, "a trusted proxy sent a malformed client address");
                peer
            }),
            None => {
                tracing::warn!(header = %trust.header, "a trusted proxy sent a malformed client address");
                peer
            }
        }
    }
}

/// One address, with or without a port.
fn parse_address(text: &str) -> Option<IpAddr> {
    let text = text.trim();
    text.parse::<IpAddr>()
        .or_else(|_| text.parse::<SocketAddr>().map(|socket| socket.ip()))
        .ok()
        .map(|address| address.to_canonical())
}

/// The right-most entry of the comma-separated list, across every header line in order, that is not
/// a trusted proxy; the left-most when every entry is one.
fn read_forwarded_for(trust: &ProxyTrust, values: &[&str]) -> Option<IpAddr> {
    let entries: Vec<&str> = values.iter().flat_map(|value| value.split(',')).collect();
    let mut leftmost = None;
    for entry in entries.iter().rev() {
        let address = parse_address(entry)?;
        if !trust.is_trusted(address) {
            return Some(address);
        }
        leftmost = Some(address);
    }
    leftmost
}

/// The header's only value, read as one address.
fn read_single(_: &ProxyTrust, values: &[&str]) -> Option<IpAddr> {
    match values {
        [value] => parse_address(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn peer(text: &str) -> SocketAddr {
        SocketAddr::new(ip(text), 40_000)
    }

    fn headers(name: &str, values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in values {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn policy(trusted: &[&str], header: &str) -> ClientIpPolicy {
        ClientIpPolicy::new(Some(ProxyTrust::new(
            trusted
                .iter()
                .map(|text| Cidr::parse(text).unwrap())
                .collect(),
            HeaderName::from_bytes(header.as_bytes()).unwrap(),
        )))
    }

    #[test]
    fn cidr_table() {
        let cases: &[(&str, &str, bool)] = &[
            ("10.0.0.0/8", "10.255.1.2", true),
            ("10.0.0.0/8", "11.0.0.1", false),
            ("172.18.0.0/16", "172.18.4.9", true),
            ("172.18.0.0/16", "172.19.0.1", false),
            ("192.168.1.7", "192.168.1.7", true),
            ("192.168.1.7", "192.168.1.8", false),
            ("0.0.0.0/0", "8.8.8.8", true),
            ("10.0.0.0/8", "::ffff:10.1.2.3", true),
            ("fd00::/8", "fd12::1", true),
            ("fd00::/8", "fe80::1", false),
            ("::/0", "10.0.0.1", false),
        ];
        for (cidr, address, inside) in cases {
            assert_eq!(
                Cidr::parse(cidr).unwrap().contains(ip(address)),
                *inside,
                "{cidr} {address}"
            );
        }
        for bad in [
            "10.0.0.0/33",
            "fd00::/129",
            "10.0.0/8",
            "example.com",
            "10.0.0.0/x",
        ] {
            assert!(Cidr::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn without_trusted_proxies_the_peer_is_the_client() {
        let policy = ClientIpPolicy::new(None);
        let forged = headers("x-forwarded-for", &["1.2.3.4"]);
        assert_eq!(
            policy.client(peer("203.0.113.5"), &forged),
            ip("203.0.113.5")
        );
    }

    #[test]
    fn a_header_from_an_untrusted_peer_is_ignored() {
        let policy = policy(&["172.18.0.0/16"], "x-forwarded-for");
        let forged = headers("x-forwarded-for", &["1.2.3.4"]);
        assert_eq!(
            policy.client(peer("203.0.113.5"), &forged),
            ip("203.0.113.5")
        );
    }

    #[test]
    fn forwarded_for_picks_the_first_untrusted_entry_from_the_right() {
        let policy = policy(&["172.18.0.0/16", "198.51.100.0/24"], "x-forwarded-for");
        let cases: &[(&[&str], &str)] = &[
            (&["203.0.113.9"], "203.0.113.9"),
            (&["1.1.1.1, 203.0.113.9, 198.51.100.7"], "203.0.113.9"),
            (
                &["1.1.1.1, 203.0.113.9", "198.51.100.7, 172.18.0.3"],
                "203.0.113.9",
            ),
            (&["198.51.100.7, 172.18.0.3"], "198.51.100.7"),
            (&["[2001:db8::1]:443"], "2001:db8::1"),
        ];
        for (values, client) in cases {
            let map = headers("x-forwarded-for", values);
            assert_eq!(
                policy.client(peer("172.18.0.2"), &map),
                ip(client),
                "{values:?}"
            );
        }
    }

    #[test]
    fn a_malformed_header_from_a_trusted_peer_falls_back_to_the_peer() {
        let policy = policy(&["172.18.0.0/16"], "x-forwarded-for");
        for values in [&["not-an-address"][..], &["1.1.1.1, garbage, 172.18.0.9"]] {
            let map = headers("x-forwarded-for", values);
            assert_eq!(policy.client(peer("172.18.0.2"), &map), ip("172.18.0.2"));
        }
        let none = HeaderMap::new();
        assert_eq!(policy.client(peer("172.18.0.2"), &none), ip("172.18.0.2"));
    }

    #[test]
    fn another_header_is_read_as_one_address() {
        let policy = policy(&["172.18.0.0/16"], "cf-connecting-ip");
        let one = headers("cf-connecting-ip", &["203.0.113.9"]);
        assert_eq!(policy.client(peer("172.18.0.2"), &one), ip("203.0.113.9"));
        let list = headers("cf-connecting-ip", &["203.0.113.9, 1.1.1.1"]);
        assert_eq!(policy.client(peer("172.18.0.2"), &list), ip("172.18.0.2"));
        let ignored = headers("x-forwarded-for", &["1.2.3.4"]);
        assert_eq!(
            policy.client(peer("172.18.0.2"), &ignored),
            ip("172.18.0.2")
        );
    }
}
