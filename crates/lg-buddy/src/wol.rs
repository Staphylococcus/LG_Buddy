use std::error::Error;
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

use crate::config::MacAddress;

pub const MAGIC_PACKET_LEN: usize = 102;
pub const DEFAULT_WOL_PORT: u16 = 9;

#[derive(Debug)]
pub enum WakeOnLanError {
    Io {
        action: &'static str,
        source: io::Error,
    },
}

impl fmt::Display for WakeOnLanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { action, source } => write!(f, "failed to {action}: {source}"),
        }
    }
}

impl Error for WakeOnLanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
        }
    }
}

pub trait WakeOnLanSender {
    fn send_magic_packet(&self, mac: &MacAddress, target_ip: Option<Ipv4Addr>, subnet_mask: Option<Ipv4Addr>) -> Result<(), WakeOnLanError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpWakeOnLanSender {
    bind_addr: SocketAddrV4,
    target_addr: SocketAddrV4,
    subnet_mask: Option<Ipv4Addr>,
}

impl Default for UdpWakeOnLanSender {
    fn default() -> Self {
        Self::broadcast(DEFAULT_WOL_PORT)
    }
}

impl UdpWakeOnLanSender {
    pub fn new(bind_addr: SocketAddrV4, target_addr: SocketAddrV4) -> Self {
        Self {
            bind_addr,
            target_addr,
            subnet_mask: None,
        }
    }

    pub fn with_subnet_mask(bind_addr: SocketAddrV4, target_addr: SocketAddrV4, subnet_mask: Option<Ipv4Addr>) -> Self {
        Self {
            bind_addr,
            target_addr,
            subnet_mask,
        }
    }

    pub fn broadcast(port: u16) -> Self {
        Self::new(
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
            SocketAddrV4::new(Ipv4Addr::BROADCAST, port),
        )
    }

    pub fn bind_addr(&self) -> SocketAddrV4 {
        self.bind_addr
    }

    pub fn target_addr(&self) -> SocketAddrV4 {
        self.target_addr
    }

    // Helper function to calculate directed broadcast address
    fn calculate_broadcast_addr(ip: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
        let ip_u32 = u32::from(ip);
        let mask_u32 = u32::from(mask);
        let broadcast_u32 = ip_u32 | !mask_u32;
        Ipv4Addr::from(broadcast_u32)
    }
}

impl WakeOnLanSender for UdpWakeOnLanSender {
    fn send_magic_packet(&self, mac: &MacAddress, target_ip: Option<Ipv4Addr>, subnet_mask: Option<Ipv4Addr>) -> Result<(), WakeOnLanError> {
        let target_addr = if let Some(ip) = target_ip {
            let mask = subnet_mask.or(self.subnet_mask).unwrap_or(Ipv4Addr::new(255, 255, 255, 0)); // Default to /24
            let broadcast_addr = Self::calculate_broadcast_addr(ip, mask);
            SocketAddrV4::new(broadcast_addr, self.target_addr.port())
        } else {
            self.target_addr
        };

        let socket = UdpSocket::bind(self.bind_addr).map_err(|source| WakeOnLanError::Io {
            action: "bind UDP socket",
            source,
        })?;
        socket
            .set_broadcast(true)
            .map_err(|source| WakeOnLanError::Io {
                action: "enable UDP broadcast",
                source,
            })?;

        let packet = build_magic_packet(mac);
        socket
            .send_to(&packet, target_addr)
            .map_err(|source| WakeOnLanError::Io {
                action: "send Wake-on-LAN packet",
                source,
            })?;

        Ok(())
    }
}

pub fn build_magic_packet(mac: &MacAddress) -> [u8; MAGIC_PACKET_LEN] {
    let mut packet = [0_u8; MAGIC_PACKET_LEN];
    packet[..6].fill(0xFF);

    let mac_octets = mac.octets();
    for chunk in packet[6..].chunks_exact_mut(mac_octets.len()) {
        chunk.copy_from_slice(&mac_octets);
    }

    packet
}

#[cfg(test)]
mod tests {
    use super::{
        build_magic_packet, UdpWakeOnLanSender, WakeOnLanSender, DEFAULT_WOL_PORT, MAGIC_PACKET_LEN,
    };
    use crate::config::MacAddress;
    use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn default_sender_uses_broadcast_port_9() {
        let sender = UdpWakeOnLanSender::default();

        assert_eq!(
            sender.bind_addr(),
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)
        );
        assert_eq!(
            sender.target_addr(),
            SocketAddrV4::new(Ipv4Addr::BROADCAST, DEFAULT_WOL_PORT)
        );
    }

    #[test]
    fn magic_packet_has_expected_layout() {
        let mac = parse_mac("aa:bb:cc:dd:ee:ff");
        let packet = build_magic_packet(&mac);

        assert_eq!(packet.len(), MAGIC_PACKET_LEN);
        assert_eq!(&packet[..6], &[0xFF; 6]);

        for chunk in packet[6..].chunks_exact(6) {
            assert_eq!(chunk, &[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        }
    }

    #[test]
    fn udp_sender_delivers_magic_packet() {
        let listener =
            UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set listener timeout");
        let target = match listener.local_addr().expect("listener addr") {
            std::net::SocketAddr::V4(addr) => addr,
            std::net::SocketAddr::V6(_) => panic!("expected IPv4 listener address"),
        };

        let receiver = thread::spawn(move || {
            let mut buf = [0_u8; MAGIC_PACKET_LEN];
            let (size, _) = listener.recv_from(&mut buf).expect("receive magic packet");
            (size, buf)
        });

        let sender = UdpWakeOnLanSender::new(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0), target);
        let mac = parse_mac("01:23:45:67:89:ab");
        sender
            .send_magic_packet(&mac, None, None)
            .expect("send magic packet over udp");

        let (size, received) = receiver.join().expect("join receiver thread");
        assert_eq!(size, MAGIC_PACKET_LEN);
        assert_eq!(received, build_magic_packet(&mac));
    }

    fn parse_mac(value: &str) -> MacAddress {
        value.parse().expect("parse mac address")
    }
}
