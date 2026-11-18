use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use cockpit_intercept::{handle_query, DnsConfig, DnsServer, TYPE_A, TYPE_AAAA};

fn query(name: &str, qtype: u16) -> Vec<u8> {
    let mut v = vec![
        0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    for label in name.split('.') {
        v.push(label.len() as u8);
        v.extend_from_slice(label.as_bytes());
    }
    v.push(0);
    v.extend_from_slice(&qtype.to_be_bytes());
    v.extend_from_slice(&1u16.to_be_bytes());
    v
}

fn ancount(packet: &[u8]) -> u16 {
    u16::from_be_bytes([packet[6], packet[7]])
}

#[test]
fn a_query_returns_configured_address() {
    let cfg = DnsConfig::new("127.0.0.1:0".parse().unwrap(), Ipv4Addr::new(10, 1, 2, 3));
    let response = handle_query(&query("example.com", TYPE_A), &cfg).unwrap();

    assert_eq!(ancount(&response), 1);
    assert_eq!(&response[response.len() - 4..], &[10, 1, 2, 3]);
    // id сохраняется
    assert_eq!(&response[0..2], &[0x12, 0x34]);
}

#[test]
fn aaaa_query_returns_empty_answer() {
    let cfg = DnsConfig::new("127.0.0.1:0".parse().unwrap(), Ipv4Addr::new(10, 1, 2, 3));
    let response = handle_query(&query("example.com", TYPE_AAAA), &cfg).unwrap();
    assert_eq!(ancount(&response), 0);
}

#[test]
fn malformed_packet_is_ignored() {
    let cfg = DnsConfig::new("127.0.0.1:0".parse().unwrap(), Ipv4Addr::new(10, 1, 2, 3));
    assert!(handle_query(&[0, 1, 2], &cfg).is_none());
    assert!(handle_query(&[], &cfg).is_none());
}

#[test]
fn server_answers_over_udp() {
    let cfg = DnsConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        Ipv4Addr::new(192, 168, 137, 1),
    );
    let server = DnsServer::start(cfg).unwrap();
    let addr: SocketAddr = server.addr();

    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client.send_to(&query("test.local", TYPE_A), addr).unwrap();

    let mut buf = [0u8; 512];
    let (len, _) = client.recv_from(&mut buf).unwrap();
    let response = &buf[..len];

    assert_eq!(ancount(response), 1);
    assert_eq!(&response[len - 4..], &[192, 168, 137, 1]);

    server.stop();
}
