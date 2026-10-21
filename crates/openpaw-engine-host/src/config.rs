use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[derive(Debug, Clone, Copy)]
pub struct HostConfig {
    bind: SocketAddr,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        }
    }
}

impl HostConfig {
    pub fn new(bind: SocketAddr) -> Result<Self, String> {
        if !bind.ip().is_loopback() {
            return Err(format!("engine host requires a loopback address: {bind}"));
        }
        Ok(Self { bind })
    }

    pub fn bind_address(self) -> SocketAddr {
        self.bind
    }
}
