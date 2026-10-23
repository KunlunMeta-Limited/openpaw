#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;

pub fn spawn_mock(responses: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let addr = listener.local_addr().expect("addr").to_string();
    std::thread::spawn(move || {
        for response in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut chunk = [0u8; 8192];
            let header_end = loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break request.len();
                }
                request.extend_from_slice(&chunk[..read]);
                if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    break position + 4;
                }
            };
            let content_length = String::from_utf8_lossy(&request[..header_end])
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            while request.len() < header_end + content_length {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    addr
}

pub fn http_html(body: &str) -> String {
    http_html_status("HTTP/1.1 200 OK", body)
}

pub fn http_html_status(status: &str, body: &str) -> String {
    format!(
        "{status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub fn http_redirect(location: &str) -> String {
    format!("HTTP/1.1 301 Moved Permanently\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}
