//! This machine's IPv4 address on the local network, for URLs that other devices open.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// TEST-NET-1 (RFC 5737). Connecting a UDP socket sends nothing; the operating system only
/// picks the local address its routing table would use, here the default route's.
const ROUTE_PROBE: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 9);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanAddress {
    pub address: Ipv4Addr,
    pub found_through: String,
}

impl fmt::Display for LanAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.address, self.found_through)
    }
}

/// The local address of the default route, else the first IPv4 interface address that is
/// neither loopback nor link-local.
pub fn lan_ipv4_address() -> Option<LanAddress> {
    default_route_address()
        .map(|address| LanAddress {
            address,
            found_through: "the default route's local address".to_owned(),
        })
        .or_else(first_interface_address)
}

fn default_route_address() -> Option<Ipv4Addr> {
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|socket| socket.connect(ROUTE_PROBE).map(|()| socket))
        .and_then(|socket| socket.local_addr());
    match probe {
        Ok(local) => match local.ip() {
            IpAddr::V4(address) if is_reachable_from_the_network(address) => Some(address),
            other => {
                re_log::debug!(
                    "lan address: the default route's local address {other} is not usable"
                );
                None
            }
        },
        Err(error) => {
            re_log::debug!("lan address: no default route ({error})");
            None
        }
    }
}

fn first_interface_address() -> Option<LanAddress> {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces,
        Err(error) => {
            re_log::warn!("lan address: listing the network interfaces failed: {error}");
            return None;
        }
    };
    interfaces
        .into_iter()
        .find_map(|interface| match interface.ip() {
            IpAddr::V4(address) if is_reachable_from_the_network(address) => Some(LanAddress {
                address,
                found_through: format!("the first usable interface, {}", interface.name),
            }),
            _ => None,
        })
}

fn is_reachable_from_the_network(address: Ipv4Addr) -> bool {
    !(address.is_loopback()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_broadcast()
        || address.is_multicast())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_and_link_local_addresses_are_not_reachable_from_the_network() {
        assert!(!is_reachable_from_the_network(Ipv4Addr::LOCALHOST));
        assert!(!is_reachable_from_the_network(Ipv4Addr::new(
            169, 254, 10, 20
        )));
        assert!(!is_reachable_from_the_network(Ipv4Addr::UNSPECIFIED));
        assert!(is_reachable_from_the_network(Ipv4Addr::new(
            192, 168, 1, 23
        )));
        assert!(is_reachable_from_the_network(Ipv4Addr::new(10, 0, 0, 5)));
    }

    #[test]
    fn a_found_address_is_usable_from_the_network() {
        if let Some(found) = lan_ipv4_address() {
            assert!(is_reachable_from_the_network(found.address), "{found}");
        }
    }
}
