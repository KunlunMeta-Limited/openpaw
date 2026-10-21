use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use openpaw_engine_host::{http, HostConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn rejects_non_loopback_binding() {
    for ip in [
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    ] {
        assert!(HostConfig::new(SocketAddr::new(ip, 0)).is_err());
    }
    assert!(HostConfig::new(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0)).is_ok());
}

#[tokio::test]
async fn defaults_to_a_dynamic_loopback_listener() {
    let listener = http::bind(HostConfig::default()).await.unwrap();
    let address = listener.local_addr().unwrap();
    assert!(address.ip().is_loopback());
    assert_ne!(address.port(), 0);

    let service = tokio::spawn(http::serve(listener));
    let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
    client
        .write_all(b"GET /global/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("\"healthy\":true"));
    service.abort();
}
