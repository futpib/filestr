//! Shared e2e harness for the `cargo test` port of `scripts/autotests/`.
//!
//! Spawns the real `filestrd` binary as a child process (relay disabled, like
//! the bash suite), drives it over its typed control protocol (`libfilestrctl`)
//! and HTTP gateway, and asserts with plain Rust. Replaces the bash harness's
//! `curl | jq` assertions, `sleep`-based synchronisation, copy-pasted setup, and
//! `die`-on-failure with typed calls, condition-polling, and `assert!`.
//!
//! Build the gateway + chat in: `cargo test -p filestrd --features grayjay`
//! (the default `chat` feature stays on, so hub tests work too).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use libfilestr::ctl::{
    ChatMessage, DaemonStatus, FileEntry, HubInfo, PeerReputation, RequestBody, ResponseBody,
    SearchHit, ShareInfo, TransferInfo,
};
use libfilestrctl::Client;
use tempfile::TempDir;

const POLL_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_STEP: Duration = Duration::from_millis(100);

/// Options for [`Node::start`].
#[derive(Default)]
pub struct NodeOpts {
    /// Create a `share/` dir and a `[[share]] name="files"` root.
    pub share: bool,
    /// Bind the loopback HTTP gateway on this port (requires the `grayjay` feature).
    pub http_port: Option<u16>,
    /// Extra TOML appended to the generated config (e.g. `[search]`, `[chat]`).
    pub extra_config: String,
}

/// A running `filestrd` instance in its own temp dir. Killed on drop.
pub struct Node {
    pub name: String,
    _tmp: TempDir,
    dir: PathBuf,
    socket: PathBuf,
    config_path: PathBuf,
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

        let child = spawn_daemon(&config_path, &dir, false);
        let node = Node {
            name: name.to_string(),
            _tmp: tmp,
            dir,
            socket,
            config_path,
            http_port: opts.http_port,
            child,
        };
        node.wait_ready().await;
        node
    }

    /// Kill and relaunch reusing the same data/state dirs and config (cold
    /// restart). Used by persistence/queued-join tests.
    pub async fn restart(&mut self) {
        signal(self.child.id(), libc::SIGCONT);
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.child = spawn_daemon(&self.config_path, &self.dir, true);
        self.wait_ready().await;
    }

    async fn wait_ready(&self) {
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

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The daemon's combined stdout/stderr log.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("daemon.log")).unwrap_or_default()
    }

    /// Append TOML to the config file (for SIGHUP-reload / enable-on-restart tests).
    pub fn append_config(&self, toml: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&self.config_path).unwrap();
        writeln!(f, "\n{toml}").unwrap();
    }

    /// Replace the config file wholesale (used to flip a setting on restart).
    pub fn rewrite_config(&self, contents: &str) {
        std::fs::write(&self.config_path, contents).unwrap();
    }

    pub async fn rescan(&self) -> usize {
        match self.call(RequestBody::Rescan).await {
            ResponseBody::Rescanned { files } => files,
            other => panic!("unexpected rescan response: {other:?}"),
        }
    }

    pub async fn scan_pause(&self) {
        self.call(RequestBody::ScanPause).await;
    }
    pub async fn scan_resume(&self) {
        self.call(RequestBody::ScanResume).await;
    }
    pub async fn scan_cancel(&self) {
        self.call(RequestBody::ScanCancel).await;
    }

    pub async fn invite_create(&self, label: Option<&str>) -> String {
        self.invite_create_opts(label, None, None).await
    }

    pub async fn invite_create_opts(
        &self,
        label: Option<&str>,
        allow_reshare: Option<bool>,
        relay_only: Option<bool>,
    ) -> String {
        let body = RequestBody::InviteCreate {
            view: None,
            label: label.map(String::from),
            allow_reshare,
            relay_only,
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

    pub async fn browse(&self, peer: &str) -> Vec<FileEntry> {
        match self.call(RequestBody::Browse { peer: peer.to_string() }).await {
            ResponseBody::Entries { entries } => entries,
            other => panic!("unexpected browse response: {other:?}"),
        }
    }

    /// `browse` with no peer = this node's own shared files.
    pub async fn browse_self(&self) -> Vec<FileEntry> {
        match self.call(RequestBody::BrowseSelf).await {
            ResponseBody::Entries { entries } => entries,
            other => panic!("unexpected browse-self response: {other:?}"),
        }
    }

    pub async fn shares(&self) -> Vec<ShareInfo> {
        match self.call(RequestBody::ShareList).await {
            ResponseBody::Shares { shares, .. } => shares,
            other => panic!("unexpected share-list response: {other:?}"),
        }
    }

    pub async fn share_add(&self, path: &Path, name: Option<&str>) -> ResponseBody {
        self.call(RequestBody::ShareAdd { path: path.to_path_buf(), name: name.map(String::from) })
            .await
    }

    pub async fn share_add_expect_err(&self, path: &Path, name: Option<&str>) -> String {
        self.call_expect_err(RequestBody::ShareAdd {
            path: path.to_path_buf(),
            name: name.map(String::from),
        })
        .await
    }

    pub async fn share_remove(&self, name: &str) {
        self.call(RequestBody::ShareRemove { name: name.to_string() }).await;
    }

    pub async fn search(&self, query: &str) -> Vec<SearchHit> {
        let mut client = self.client().await;
        let id = client
            .send(RequestBody::Search { query: query.to_string(), ttl: None })
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

    pub async fn get(&self, hash: &str) -> Vec<u8> {
        self.get_opts(hash, None, None).await.expect("get should succeed")
    }

    /// Fetch with an optional inclusive byte `range` and/or preferred `peer`.
    /// Returns the bytes, or the daemon error message.
    pub async fn get_opts(
        &self,
        hash: &str,
        range: Option<&str>,
        peer: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let out = self.dir.join(format!("get-{hash}-{}.bin", range.unwrap_or("full")));
        let mut client = self.client().await;
        let id = client
            .send(RequestBody::Get {
                hash: hash.to_string(),
                out: out.clone(),
                peer: peer.map(String::from),
                range: range.map(String::from),
                background: false,
            })
            .await
            .expect("send get");
        loop {
            match client.recv(id).await {
                Ok(ResponseBody::GetProgress { .. }) => {}
                Ok(ResponseBody::GetDone { .. }) => break,
                Ok(other) => panic!("unexpected get response: {other:?}"),
                Err(libfilestrctl::Error::Server(msg)) => return Err(msg),
                Err(e) => panic!("get transport error: {e}"),
            }
        }
        Ok(std::fs::read(out).expect("read fetched file"))
    }

    /// Start a background transfer; returns its id.
    pub async fn get_background(&self, hash: &str, peer: Option<&str>, out: &Path) -> u64 {
        match self
            .call(RequestBody::Get {
                hash: hash.to_string(),
                out: out.to_path_buf(),
                peer: peer.map(String::from),
                range: None,
                background: true,
            })
            .await
        {
            ResponseBody::TransferStarted { id } => id,
            other => panic!("unexpected background get response: {other:?}"),
        }
    }

    pub async fn transfers(&self) -> Vec<TransferInfo> {
        match self.call(RequestBody::Transfers).await {
            ResponseBody::Transfers { transfers } => transfers,
            other => panic!("unexpected transfers response: {other:?}"),
        }
    }

    pub async fn reputation(&self) -> Vec<PeerReputation> {
        match self.call(RequestBody::Reputation).await {
            ResponseBody::Reputation { peers } => peers,
            other => panic!("unexpected reputation response: {other:?}"),
        }
    }

    // --- hubs / chat -------------------------------------------------------

    pub async fn hub_create(&self, name: &str) -> String {
        match self.call(RequestBody::HubCreate { name: name.to_string() }).await {
            ResponseBody::HubCreated { hub } => hub.group_ref,
            other => panic!("unexpected hub-create response: {other:?}"),
        }
    }

    pub async fn hub_invite(&self, hub: &str) -> String {
        match self.call(RequestBody::HubInvite { hub: hub.to_string() }).await {
            ResponseBody::HubInvite { ticket } => ticket,
            other => panic!("unexpected hub-invite response: {other:?}"),
        }
    }

    /// Join a hub ticket; returns whether the join was queued (chat disabled).
    pub async fn hub_join(&self, ticket: &str) -> bool {
        match self.call(RequestBody::HubJoin { ticket: ticket.to_string() }).await {
            ResponseBody::HubJoined { queued, .. } => queued,
            other => panic!("unexpected hub-join response: {other:?}"),
        }
    }

    pub async fn hub_join_expect_err(&self, ticket: &str) -> String {
        self.call_expect_err(RequestBody::HubJoin { ticket: ticket.to_string() }).await
    }

    pub async fn hub_request(&self, address: Option<&str>, label: Option<&str>) -> String {
        match self
            .call(RequestBody::HubRequest {
                address: address.map(String::from),
                hub: None,
                label: label.map(String::from),
            })
            .await
        {
            ResponseBody::HubRequestTicket { ticket, .. } => ticket,
            other => panic!("unexpected hub-request response: {other:?}"),
        }
    }

    pub async fn hub_admit(&self, ticket: &str) -> ResponseBody {
        self.call(RequestBody::HubAdmit { ticket: ticket.to_string(), hub: None }).await
    }

    pub async fn hub_admit_expect_err(&self, ticket: &str) -> String {
        self.call_expect_err(RequestBody::HubAdmit { ticket: ticket.to_string(), hub: None }).await
    }

    pub async fn hub_address(&self, hub: &str) -> String {
        match self.call(RequestBody::HubAddress { hub: hub.to_string() }).await {
            ResponseBody::HubAddress { address } => address,
            other => panic!("unexpected hub-address response: {other:?}"),
        }
    }

    pub async fn hub_members(&self, hub: &str) -> Vec<String> {
        match self.call(RequestBody::HubMembers { hub: hub.to_string() }).await {
            ResponseBody::HubMembers { members } => members,
            other => panic!("unexpected hub-members response: {other:?}"),
        }
    }

    pub async fn hub_send(&self, hub: &str, text: &str) {
        self.call(RequestBody::HubSend { hub: hub.to_string(), text: text.to_string() }).await;
    }

    pub async fn hub_log(&self, hub: &str) -> Vec<ChatMessage> {
        match self.call(RequestBody::HubLog { hub: hub.to_string() }).await {
            ResponseBody::HubMessages { messages } => messages,
            other => panic!("unexpected hub-log response: {other:?}"),
        }
    }

    pub async fn hub_ls(&self) -> Vec<HubInfo> {
        match self.call(RequestBody::HubList).await {
            ResponseBody::Hubs { hubs } => hubs,
            other => panic!("unexpected hub-ls response: {other:?}"),
        }
    }

    pub async fn hub_ls_expect_err(&self) -> String {
        self.call_expect_err(RequestBody::HubList).await
    }

    /// The HTTP gateway client (requires `http_port` to have been set).
    pub fn http(&self) -> Gateway {
        let port = self.http_port.expect("node has no http gateway");
        Gateway { base: format!("http://127.0.0.1:{port}"), client: reqwest::Client::new() }
    }

    pub async fn wait_share_files(&self, want: usize) {
        wait_until(&format!("[{}] {want} shared files", self.name), || async {
            self.status().await.files == want
        })
        .await;
    }

    /// Stop answering (process + address kept) — simulates the radio dropping.
    pub fn pause(&self) {
        signal(self.pid(), libc::SIGSTOP);
    }
    /// Resume answering — simulates the network coming back.
    pub fn resume(&self) {
        signal(self.pid(), libc::SIGCONT);
    }
    /// Reload config (SIGHUP) — e.g. a per-peer reputation override.
    pub fn sighup(&self) {
        signal(self.pid(), libc::SIGHUP);
    }
    /// Kill the process but keep the struct/dirs (so already-fetched state stays
    /// readable) — for "the provider died mid-stream" tests.
    pub fn kill(&self) {
        signal(self.pid(), libc::SIGKILL);
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        signal(self.pid(), libc::SIGCONT);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_daemon(config_path: &Path, dir: &Path, append: bool) -> Child {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(append)
        .write(true)
        .truncate(!append)
        .open(dir.join("daemon.log"))
        .unwrap();
    Command::new(env!("CARGO_BIN_EXE_filestrd"))
        .arg("--config")
        .arg(config_path)
        .arg("-vv")
        .stderr(log.try_clone().unwrap())
        .stdout(log)
        .spawn()
        .expect("spawn filestrd")
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
            self.client
                .get(format!("{}/files", self.base))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
        })
        .await;
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.client.get(format!("{}{}", self.base, path)).send().await.expect("http get")
    }

    pub async fn head(&self, path: &str) -> reqwest::Response {
        self.client.head(format!("{}{}", self.base, path)).send().await.expect("http head")
    }

    /// GET `/file/<hash>` with an optional inclusive byte range.
    pub async fn get_file(&self, hash: &str, range: Option<&str>) -> reqwest::Response {
        let mut req = self.client.get(format!("{}/file/{}", self.base, hash));
        if let Some(r) = range {
            req = req.header("Range", format!("bytes={r}"));
        }
        req.send().await.expect("http get file")
    }

    /// GET `/file/<hash>` with arbitrary extra headers (If-None-Match, If-Range, …).
    pub async fn get_file_headers(&self, hash: &str, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut req = self.client.get(format!("{}/file/{}", self.base, hash));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req.send().await.expect("http get file w/ headers")
    }

    /// Any gateway path parsed as JSON.
    pub async fn json(&self, path: &str) -> serde_json::Value {
        let body = self.get(path).await.text().await.unwrap();
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("parse {path}: {e} (body: {body})"))
    }

    /// `/files` parsed as JSON.
    pub async fn files(&self) -> serde_json::Value {
        self.json("/files").await
    }

    /// `/search?q=` parsed as JSON.
    pub async fn search(&self, q: &str) -> serde_json::Value {
        let body = self
            .client
            .get(format!("{}/search", self.base))
            .query(&[("q", q)])
            .send()
            .await
            .expect("http search")
            .text()
            .await
            .unwrap();
        serde_json::from_str(&body).expect("parse /search")
    }

    pub async fn grayjay_config(&self) -> serde_json::Value {
        serde_json::from_str(&self.get("/grayjay/FilestrConfig.json").await.text().await.unwrap())
            .unwrap()
    }

    pub async fn plugin_version(&self) -> u64 {
        self.grayjay_config().await["version"].as_u64().unwrap_or(0)
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

/// Write `content` to `<share>/<rel>`, creating parent dirs. Returns the path.
pub fn write_share_file(node: &Node, rel: &str, content: &[u8]) -> PathBuf {
    let path = node.share_dir().join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    path
}

/// Deterministic pseudo-random bytes (no rng dep; good enough for distinct blobs).
pub fn pseudo_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Disk bytes actually allocated under `dir` (recursively) — counts allocated
/// blocks, not apparent size, so a sparse partial blob (iroh allocates a
/// full-size sparse file but writes only the fetched range) is measured by what
/// it really occupies, matching `du`. Used to assert a relay did NOT cache
/// forwarded content and a ranged GET didn't over-fetch.
pub fn dir_size(dir: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(m) = e.metadata() {
                total += m.blocks() * 512; // 512-byte blocks, like du
            }
        }
    }
    total
}

/// Whether `ffmpeg` is on PATH (media tests skip without it, like the bash suite).
pub fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run an ffmpeg command (args after the implicit `-v error -y`), panicking on failure.
pub fn ffmpeg(args: &[&str]) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed: {args:?}");
}
