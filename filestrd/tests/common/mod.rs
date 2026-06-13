//! Shared e2e harness for the `cargo test` port of `scripts/autotests/`.
//!
//! Spawns the real `filestrd` binary as a child process (relay disabled, like
//! the bash suite), drives it over its typed control protocol (`libfilestrctl`)
//! and HTTP gateway, and asserts with plain Rust. Replaces the bash harness's
//! `curl | jq` assertions, `sleep`-based synchronisation, copy-pasted setup, and
//! `die`-on-failure with typed calls, condition-polling, and `assert!`.
//!
//! Build the gateway in: `cargo test -p filestrd --features grayjay`.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use libfilestr::ctl::{DaemonStatus, FileEntry, RequestBody, ResponseBody, SearchHit};
use libfilestrctl::Client;
use tempfile::TempDir;

/// Default budget for `wait_until` polls. Generous: a reachable peer answers in
/// well under a second; this only bounds giving up.
const POLL_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_STEP: Duration = Duration::from_millis(100);

/// Options for [`Node::start`].
#[derive(Default)]
pub struct NodeOpts {
    /// Create a `share/` dir and a `[[share]] name="files"` root.
    pub share: bool,
    /// Bind the loopback HTTP gateway on this port (requires the `grayjay` feature).
    pub http_port: Option<u16>,
    /// Extra TOML appended to the generated config (e.g. a `[search]` block).
    pub extra_config: String,
}

/// A running `filestrd` instance in its own temp dir. Killed on drop.
pub struct Node {
    pub name: String,
    _tmp: TempDir,
    dir: PathBuf,
    socket: PathBuf,
    http_port: Option<u16>,
    child: Child,
}

impl Node {
    /// Spawn a node and wait until its control socket answers and the startup
    /// scan has settled.
    pub async fn start(name: &str, opts: NodeOpts) -> Node {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let socket = dir.join("ctl.sock");

        let mut config = format!(
            "socket = \"{}\"\ndata_dir = \"{}\"\nrelay = \"disabled\"\n",
            socket.display(),
            dir.join("data").display(),
        );
        if opts.share {
            let share = dir.join("share");
            std::fs::create_dir_all(&share).unwrap();
            config.push_str(&format!(
                "[[share]]\nname = \"files\"\npath = \"{}\"\n",
                share.display()
            ));
        }
        if let Some(port) = opts.http_port {
            config.push_str(&format!("[http]\nlisten = \"127.0.0.1:{port}\"\n"));
        }
        config.push_str(&opts.extra_config);
        config.push('\n');
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, config).unwrap();

        let log = std::fs::File::create(dir.join("daemon.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_filestrd"))
            .arg("--config")
            .arg(&config_path)
            .arg("-vv")
            .stderr(log.try_clone().unwrap())
            .stdout(log)
            .spawn()
            .expect("spawn filestrd");

        let node = Node {
            name: name.to_string(),
            _tmp: tmp,
            dir,
            socket,
            http_port: opts.http_port,
            child,
        };
        node.wait_ready().await;
        node
    }

    async fn wait_ready(&self) {
        // control socket comes up, then the startup scan settles
        wait_until("daemon ready", || async {
            self.try_status().await.is_some_and(|s| s.indexing.is_none())
        })
        .await;
    }

    async fn try_status(&self) -> Option<DaemonStatus> {
        let mut client = Client::connect(&self.socket).await.ok()?;
        match client.roundtrip(RequestBody::Status).await.ok()? {
            ResponseBody::Status { status } => Some(status),
            _ => None,
        }
    }

    /// A fresh control client (the protocol is one request/response per connection
    /// in the simple cases; reconnect per call keeps the harness simple).
    pub async fn client(&self) -> Client {
        Client::connect(&self.socket).await.expect("connect ctl")
    }

    /// Send one request and return its response (a daemon refusal — which the
    /// client surfaces as `Err` — panics).
    pub async fn call(&self, body: RequestBody) -> ResponseBody {
        match self.client().await.roundtrip(body).await {
            Ok(resp) => resp,
            Err(e) => panic!("[{}] daemon error: {e}", self.name),
        }
    }

    /// Send a request expecting it to be refused, returning the error message.
    pub async fn call_expect_err(&self, body: RequestBody) -> String {
        match self.client().await.roundtrip(body).await {
            Err(e) => e.to_string(),
            Ok(other) => panic!("[{}] expected an error, got {other:?}", self.name),
        }
    }

    pub async fn status(&self) -> DaemonStatus {
        match self.call(RequestBody::Status).await {
            ResponseBody::Status { status } => status,
            other => panic!("unexpected status response: {other:?}"),
        }
    }

    pub async fn node_id(&self) -> String {
        self.status().await.endpoint_id
    }

    pub fn share_dir(&self) -> PathBuf {
        self.dir.join("share")
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Re-scan shares; returns the indexed file count.
    pub async fn rescan(&self) -> usize {
        match self.call(RequestBody::Rescan).await {
            ResponseBody::Rescanned { files } => files,
            other => panic!("unexpected rescan response: {other:?}"),
        }
    }

    pub async fn invite_create(&self, label: Option<&str>) -> String {
        let body = RequestBody::InviteCreate {
            view: None,
            label: label.map(String::from),
            allow_reshare: None,
            relay_only: None,
        };
        match self.call(body).await {
            ResponseBody::InviteCreated { ticket, .. } => ticket,
            other => panic!("unexpected invite response: {other:?}"),
        }
    }

    pub async fn peer_add(&self, ticket: &str) {
        self.call(RequestBody::PeerAdd { ticket: ticket.to_string(), label: None }).await;
    }

    pub async fn peer_revoke(&self, peer: &str) {
        self.call(RequestBody::PeerRevoke { peer: peer.to_string() }).await;
    }

    /// Browse a peer's file listing (by node id).
    pub async fn browse(&self, peer: &str) -> Vec<FileEntry> {
        match self.call(RequestBody::Browse { peer: peer.to_string() }).await {
            ResponseBody::Entries { entries } => entries,
            other => panic!("unexpected browse response: {other:?}"),
        }
    }

    /// Federated search; collects streamed hits until SearchDone.
    pub async fn search(&self, query: &str) -> Vec<SearchHit> {
        let mut client = self.client().await;
        let id = client.send(RequestBody::Search { query: query.to_string(), ttl: None })
            .await
            .expect("send search");
        let mut hits = Vec::new();
        loop {
            match client.recv(id).await.expect("recv search") {
                ResponseBody::SearchHit { hit } => hits.push(hit),
                ResponseBody::SearchDone { .. } => break,
                ResponseBody::Error { message } => panic!("search error: {message}"),
                other => panic!("unexpected search response: {other:?}"),
            }
        }
        hits
    }

    /// Fetch `hash` to a temp file and return its bytes.
    pub async fn get(&self, hash: &str) -> Vec<u8> {
        let out = self.dir.join(format!("get-{hash}.bin"));
        let mut client = self.client().await;
        let id = client
            .send(RequestBody::Get {
                hash: hash.to_string(),
                out: out.clone(),
                peer: None,
                range: None,
                background: false,
            })
            .await
            .expect("send get");
        loop {
            match client.recv(id).await.expect("recv get") {
                ResponseBody::GetProgress { .. } => {}
                ResponseBody::GetDone { .. } => break,
                ResponseBody::Error { message } => panic!("get error: {message}"),
                other => panic!("unexpected get response: {other:?}"),
            }
        }
        std::fs::read(out).expect("read fetched file")
    }

    /// The HTTP gateway client (requires `http_port` to have been set).
    pub fn http(&self) -> Gateway {
        let port = self.http_port.expect("node has no http gateway");
        Gateway { base: format!("http://127.0.0.1:{port}"), client: reqwest::Client::new() }
    }

    /// Poll until this node's share index reports `want` files.
    pub async fn wait_share_files(&self, want: usize) {
        wait_until(&format!("[{}] {want} shared files", self.name), || async {
            self.status().await.files == want
        })
        .await;
    }

    /// Stop answering (kept process, kept address) — simulates the radio dropping.
    pub fn pause(&self) {
        signal(self.pid(), libc::SIGSTOP);
    }

    /// Resume answering — simulates the network coming back.
    pub fn resume(&self) {
        signal(self.pid(), libc::SIGCONT);
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        // make sure a paused child can die, then reap it
        signal(self.pid(), libc::SIGCONT);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn signal(pid: u32, sig: libc::c_int) {
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

/// The loopback HTTP gateway, the surface a media player (Grayjay) uses.
pub struct Gateway {
    base: String,
    client: reqwest::Client,
}

impl Gateway {
    pub async fn wait_ready(&self) {
        wait_until("http gateway", || async {
            self.client.get(format!("{}/files", self.base)).send().await.is_ok_and(|r| r.status().is_success())
        })
        .await;
    }

    /// GET a path, returning the raw response.
    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.client.get(format!("{}{}", self.base, path)).send().await.expect("http get")
    }

    /// GET `/file/<hash>` with an optional inclusive byte range.
    pub async fn get_file(&self, hash: &str, range: Option<&str>) -> reqwest::Response {
        let mut req = self.client.get(format!("{}/file/{}", self.base, hash));
        if let Some(r) = range {
            req = req.header("Range", format!("bytes={r}"));
        }
        req.send().await.expect("http get file")
    }

    /// `/files` parsed as JSON.
    pub async fn files(&self) -> serde_json::Value {
        let body = self.get("/files").await.text().await.expect("body");
        serde_json::from_str(&body).expect("parse /files")
    }

    /// The grayjay plugin config (for its version).
    pub async fn plugin_version(&self) -> u64 {
        let v: serde_json::Value = serde_json::from_str(
            &self.get("/grayjay/FilestrConfig.json").await.text().await.unwrap(),
        )
        .unwrap();
        v["version"].as_u64().unwrap_or(0)
    }
}

/// Poll `cond` until it returns true or [`POLL_TIMEOUT`] elapses (then panic).
/// Replaces the bash suite's `sleep N` with an explicit, fast condition wait.
pub async fn wait_until<F, Fut>(what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + POLL_TIMEOUT;
    loop {
        if cond().await {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for: {what}");
        }
        tokio::time::sleep(POLL_STEP).await;
    }
}

/// Write `content` to `<share>/<rel>`, creating parent dirs.
pub fn write_share_file(node: &Node, rel: &str, content: &[u8]) -> PathBuf {
    let path = node.share_dir().join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    path
}
