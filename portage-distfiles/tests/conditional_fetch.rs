//! End-to-end conditional-GET coverage for `fetch_index`: a minimal
//! hand-rolled HTTP/1.1 server (no mock-server crate — just enough of the
//! protocol to drive `reqwest` through a real TCP round trip), verifying
//! `If-Modified-Since` is actually sent and a 304 response is actually
//! surfaced as [`portage_distfiles::IndexFetch::NotModified`].

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use portage_distfiles::{IndexFetch, fetch_index};

// Read one HTTP/1.1 request's headers off `stream` and return them
// lowercased-key-mapped, plus the request line's path.
fn read_request(stream: &TcpStream) -> (String, std::collections::HashMap<String, String>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();

    let mut headers = std::collections::HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    (path, headers)
}

// `Connection: close` on every response is load-bearing, not decoration:
// the server below serves exactly one request per accepted connection, but
// HTTP/1.1 defaults to keep-alive, so without this header `reqwest`/hyper
// is entitled to reuse the `Packages.gz` connection for the `Packages`
// request. When it does, the server has already moved on to `accept()`ing
// a new connection and never reads the reused one; the original
// `TcpStream` is then dropped at the end of its loop iteration, closing
// the socket out from under the client's in-flight second request —
// observed as a flaky `hyper::Error(IncompleteMessage)`.
fn write_response(mut stream: &TcpStream, status_line: &str, extra_headers: &str, body: &str) {
    let resp = format!(
        "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).unwrap();
    let _ = stream.flush();
}

// Serves exactly two requests on one listener: `Packages.gz` (always 404,
// forcing the real fallback path) then plain `Packages`, whose response
// depends on whether the request carried `If-Modified-Since`.
fn spawn_server(if_modified_since_present_means_304: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            let (path, headers) = read_request(&stream);
            if path.ends_with("Packages.gz") {
                write_response(&stream, "HTTP/1.1 404 Not Found", "", "");
                continue;
            }
            if if_modified_since_present_means_304 && headers.contains_key("if-modified-since") {
                write_response(&stream, "HTTP/1.1 304 Not Modified", "", "");
            } else {
                let body = "CPV: app-test/foo-1.0\nPATH: foo-1.0-1.gpkg.tar\n\n";
                write_response(
                    &stream,
                    "HTTP/1.1 200 OK",
                    "Last-Modified: Wed, 21 Oct 2026 07:28:00 GMT\r\n",
                    body,
                );
            }
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn plain_fetch_without_if_modified_since_returns_fresh_content() {
    let base = spawn_server(true);
    match fetch_index(&base, None).await.unwrap() {
        IndexFetch::Fresh {
            text,
            last_modified,
        } => {
            assert!(text.contains("app-test/foo-1.0"));
            assert_eq!(
                last_modified.as_deref(),
                Some("Wed, 21 Oct 2026 07:28:00 GMT")
            );
        }
        IndexFetch::NotModified => panic!("expected fresh content with no If-Modified-Since sent"),
    }
}

#[tokio::test]
async fn conditional_fetch_with_if_modified_since_gets_304() {
    let base = spawn_server(true);
    let result = fetch_index(&base, Some("Wed, 21 Oct 2026 07:28:00 GMT"))
        .await
        .unwrap();
    assert!(matches!(result, IndexFetch::NotModified));
}
