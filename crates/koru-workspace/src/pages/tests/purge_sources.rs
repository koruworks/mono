// SPDX-License-Identifier: MPL-2.0

//! The deletion fixture's existing client port points at this scoped Store API.
//! It serves real closed-source snapshots, not an implicit unknown or an I/O bypass.

use super::*;
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) struct SnapshotServer {
    address: SocketAddr,
    versions: HashMap<DocumentId, loro::VersionVector>,
    stopping: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl SnapshotServer {
    pub(super) fn new(workspace: WorkspaceId, documents: [DocumentId; 2]) -> Self {
        let mut versions = HashMap::new();
        let snapshots: HashMap<_, _> = documents
            .into_iter()
            .map(|id| {
                let mut source = WorkspaceDoc::page_genesis(workspace, id).unwrap();
                source
                    .set_text(genesis_seed_id(id), "closed source in Store")
                    .unwrap();
                versions.insert(id, source.version());
                (
                    format!(
                        "POST /v1/workspaces/{}/snapshot/{} HTTP/1.1",
                        workspace.to_storage(),
                        id.to_storage()
                    ),
                    source.export_snapshot().unwrap(),
                )
            })
            .collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stopping = Arc::new(AtomicBool::new(false));
        let stopped = stopping.clone();
        let worker = std::thread::spawn(move || {
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    assert!(request.len() < 8192, "bounded HTTP headers");
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = std::str::from_utf8(&request).unwrap();
                let body_len = request
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                assert!(body_len < 8192, "bounded fixture request body");
                stream.read_exact(&mut vec![0; body_len]).unwrap();
                let path = request.lines().next().unwrap();
                let snapshot = snapshots
                    .get(path)
                    .expect("only exact fixture source snapshots may be requested");
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", snapshot.len()).unwrap();
                stream.write_all(snapshot).unwrap();
            }
        });
        Self {
            address,
            versions,
            stopping,
            worker: Some(worker),
        }
    }

    pub(super) fn client(&self) -> koru_api_client::Client {
        koru_api_client::Client::new(koru_api_client::Discovery {
            port: self.address.port(),
            token: "fixture".into(),
            expires_at_millis: i64::MAX,
        })
    }

    pub(super) fn version(&self, document: DocumentId) -> loro::VersionVector {
        self.versions[&document].clone()
    }
}

impl Drop for SnapshotServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.expect("snapshot fixture worker failed");
            }
        }
    }
}
