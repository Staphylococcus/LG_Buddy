//! Transport substitution for the installed GUI smoke's debug binaries.
//! Release selection, verification, installation, and handoff remain unchanged.

use std::{net::SocketAddr, time::Duration};

pub(crate) fn request(request: ureq::Request) -> ureq::Request {
    let Ok(address) = std::env::var("LG_BUDDY_TEST_GITHUB_ADDRESS") else {
        return request;
    };
    request_to(request, address.parse().expect("fixture socket address"))
}

fn request_to(request: ureq::Request, address: SocketAddr) -> ureq::Request {
    assert!(address.ip().is_loopback(), "fixture must be on loopback");
    let original = url::Url::parse(request.url()).expect("original request URL");
    let target = format!(
        "http://{address}/{}{}{}",
        original.host_str().expect("original request host"),
        original.path(),
        original
            .query()
            .map(|query| format!("?{query}"))
            .unwrap_or_default(),
    );
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(Duration::from_secs(10))
        .build();
    let mut replay = agent.request(request.method(), &target);
    for header in request.header_names() {
        if let Some(value) = request.header(&header) {
            replay = replay.set(&header, value);
        }
    }
    replay
}

#[cfg(test)]
mod tests {
    use super::request_to;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn request_rewrites_host_and_preserves_path_query_and_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let address = listener.local_addr().expect("read listener address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fixture request");
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).expect("read fixture request");
                assert!(count > 0, "fixture request ended before headers");
                bytes.extend_from_slice(&chunk[..count]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .expect("write fixture response");
            String::from_utf8(bytes).expect("fixture request is UTF-8")
        });

        let agent = ureq::AgentBuilder::new().try_proxy_from_env(false).build();
        let request = agent
            .get("https://api.github.com/repos/Staphylococcus/LG_Buddy/releases?per_page=1")
            .set("Accept", "application/vnd.github+json")
            .set("X-Fixture-Test", "preserve-me");
        let response = request_to(request, address)
            .call()
            .expect("fixture request succeeds");
        assert_eq!(response.status(), 200);

        let captured = server.join().expect("fixture server joins");
        assert!(captured.starts_with(
            "GET /api.github.com/repos/Staphylococcus/LG_Buddy/releases?per_page=1 HTTP/1.1"
        ));
        let captured = captured.to_ascii_lowercase();
        assert!(captured.contains("accept: application/vnd.github+json\r\n"));
        assert!(captured.contains("x-fixture-test: preserve-me\r\n"));
    }
}
