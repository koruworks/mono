// SPDX-License-Identifier: MPL-2.0

//! **A local fixture server, because no test in this crate reaches the
//! network.**
//!
//! The precedent is `koru_ai_transport::test_server`: a `TcpListener` on
//! `127.0.0.1:0`, a scripted queue of replies, and every request captured so a
//! test can assert on the headers that were *sent* rather than only on the file
//! that arrived. That distinction is the whole of the resume suite — a download
//! that completes proves the bytes are right and says nothing about whether a
//! `Range` was ever asked for.
//!
//! It speaks plain HTTP on loopback, which [`super::source::http`] allows in a
//! debug build for exactly this reason and refuses everywhere else.
//!
//! ## What a reply can be told to do
//!
//! Each [`Reply`] answers one request and is popped off the script, so a test
//! writes the *sequence* of server behaviours it wants and nothing has to guess
//! which request is which. A reply can honour a range or ignore it, carry an
//! `ETag` or not, force a status, and — the one that matters most — stop
//! writing partway through a body whose `Content-Length` it has already
//! declared, which is what a dropped connection looks like from a client.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use url::Url;

/// One scripted answer.
#[derive(Clone, Debug)]
pub struct Reply {
    body: Vec<u8>,
    etag: Option<String>,
    honour_range: bool,
    /// Stop after this many bytes of the payload and close, having already
    /// declared the full `Content-Length`.
    truncate_after: Option<usize>,
    /// Answer this status instead of deciding one — `416`, `503`.
    force_status: Option<u16>,
    /// Answer `206` with this `Content-Range` verbatim, whatever was asked
    /// for. The only way to test that a lying range is refused rather than
    /// appended.
    forced_content_range: Option<String>,
}

impl Reply {
    /// Serve the whole body, honouring a `Range` if one arrives.
    pub fn ranged(body: &[u8]) -> Reply {
        Reply {
            body: body.to_vec(),
            etag: None,
            honour_range: true,
            truncate_after: None,
            force_status: None,
            forced_content_range: None,
        }
    }

    /// Serve the whole body and ignore any `Range` — a `200` to a ranged
    /// request, which is what a client must never append to.
    pub fn ignores_range(body: &[u8]) -> Reply {
        Reply {
            honour_range: false,
            ..Reply::ranged(body)
        }
    }

    pub fn status(code: u16) -> Reply {
        Reply {
            force_status: Some(code),
            ..Reply::ranged(&[])
        }
    }

    pub fn with_etag(mut self, etag: &str) -> Reply {
        self.etag = Some(etag.to_string());
        self
    }

    /// Declare the full length and then close after `bytes` of it.
    pub fn truncated_after(mut self, bytes: usize) -> Reply {
        self.truncate_after = Some(bytes);
        self
    }

    /// Answer `206` with a `Content-Range` of your choosing.
    pub fn with_content_range(mut self, value: &str) -> Reply {
        self.forced_content_range = Some(value.to_string());
        self
    }
}

/// What a request carried. Headers are lowercased, as
/// `koru_ai_transport::test_server` does.
#[derive(Clone, Debug)]
pub struct Seen {
    pub path: String,
    pub headers: HashMap<String, String>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

pub struct FixtureServer {
    base: Url,
    seen: Arc<Mutex<Vec<Seen>>>,
    join: Option<JoinHandle<()>>,
    stop: Arc<Mutex<bool>>,
}

impl FixtureServer {
    pub fn start(replies: Vec<Reply>) -> FixtureServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(Mutex::new(false));
        let thread_seen = seen.clone();
        let thread_stop = stop.clone();
        let join = thread::spawn(move || {
            let mut replies = VecDeque::from(replies);
            let deadline = Instant::now() + Duration::from_secs(20);
            while !replies.is_empty() && Instant::now() < deadline {
                if *thread_stop.lock().unwrap_or_else(|p| p.into_inner()) {
                    return;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let Some(request) = read_request(&mut stream) else {
                            continue;
                        };
                        let reply = replies.pop_front().unwrap();
                        write_response(&mut stream, &request, &reply);
                        thread_seen
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push(request);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fixture server accept failed: {error}"),
                }
            }
        });
        FixtureServer {
            base: Url::parse(&format!("http://{address}/")).unwrap(),
            seen,
            join: Some(join),
            stop,
        }
    }

    /// The URL a catalog entry would carry. The last segment is what
    /// [`crate::DirectorySource`] matches on, so tests that swap one source for
    /// the other can use the same URL.
    pub fn url(&self, name: &str) -> Url {
        self.base.join(name).unwrap()
    }

    /// Every request answered so far, in order.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        *self.stop.lock().unwrap_or_else(|p| p.into_inner()) = true;
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let text = std::str::from_utf8(&bytes[..header_end]).ok()?;
    let mut lines = text.split("\r\n");
    let path = lines.next()?.split_whitespace().nth(1)?.to_owned();
    let mut headers = HashMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    Some(Seen { path, headers })
}

/// `bytes=40-` → `40`. The only form this crate's downloader sends.
fn requested_start(request: &Seen) -> Option<u64> {
    let value = request.header("range")?;
    let rest = value.trim().strip_prefix("bytes=")?;
    rest.split('-').next()?.parse::<u64>().ok()
}

fn write_response(stream: &mut TcpStream, request: &Seen, reply: &Reply) {
    if let Some(code) = reply.force_status {
        let _ = write!(
            stream,
            "HTTP/1.1 {code} Response\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.flush();
        return;
    }
    let total = reply.body.len();
    let start = requested_start(request)
        .filter(|_| reply.honour_range)
        .map(|value| (value as usize).min(total))
        .unwrap_or(0);
    let partial = start > 0 || reply.forced_content_range.is_some();
    let payload = &reply.body[start.min(total)..];

    let mut head = String::new();
    if partial {
        head.push_str("HTTP/1.1 206 Partial Content\r\n");
        let range = reply
            .forced_content_range
            .clone()
            .unwrap_or_else(|| format!("bytes {start}-{}/{total}", total.saturating_sub(1)));
        head.push_str(&format!("Content-Range: {range}\r\n"));
    } else {
        head.push_str("HTTP/1.1 200 OK\r\n");
    }
    head.push_str("Accept-Ranges: bytes\r\n");
    if let Some(etag) = &reply.etag {
        head.push_str(&format!("ETag: {etag}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    head.push_str("Content-Type: application/octet-stream\r\n");
    head.push_str("Connection: close\r\n\r\n");
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let end = reply
        .truncate_after
        .unwrap_or(payload.len())
        .min(payload.len());
    let _ = stream.write_all(&payload[..end]);
    let _ = stream.flush();
}
