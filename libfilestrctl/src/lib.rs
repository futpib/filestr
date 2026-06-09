//! Client library for the filestrd control socket.
//!
//! One connection per command, sequential request/response. Streaming
//! requests (search, subscribe) emit multiple responses under the same id;
//! call [`Client::recv`] repeatedly until the terminal variant.

use std::path::Path;

use libfilestr::ctl::{Request, RequestBody, Response, ResponseBody};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Parse(serde_json::Error),
    /// The daemon answered with an error response.
    Server(String),
    ConnectionClosed,
    UnexpectedResponse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Parse(e) => write!(f, "protocol parse error: {e}"),
            Error::Server(message) => write!(f, "{message}"),
            Error::ConnectionClosed => write!(f, "daemon closed the connection"),
            Error::UnexpectedResponse(got) => write!(f, "unexpected response: {got}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Parse(e)
    }
}

pub struct Client {
    lines: Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl Client {
    pub async fn connect(socket: &Path) -> Result<Self, Error> {
        let stream = UnixStream::connect(socket).await?;
        let (read, writer) = stream.into_split();
        Ok(Self { lines: BufReader::new(read).lines(), writer, next_id: 1 })
    }

    pub async fn send(&mut self, body: RequestBody) -> Result<u64, Error> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_string(&Request { id, body })?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        Ok(id)
    }

    /// Next response for `id`. Error responses become [`Error::Server`].
    pub async fn recv(&mut self, id: u64) -> Result<ResponseBody, Error> {
        loop {
            let line = self.lines.next_line().await?.ok_or(Error::ConnectionClosed)?;
            if line.trim().is_empty() {
                continue;
            }
            let response: Response = serde_json::from_str(&line)?;
            if response.id != id {
                continue;
            }
            if let ResponseBody::Error { message } = response.body {
                return Err(Error::Server(message));
            }
            return Ok(response.body);
        }
    }

    /// Send a non-streaming request and read its single response.
    pub async fn roundtrip(&mut self, body: RequestBody) -> Result<ResponseBody, Error> {
        let id = self.send(body).await?;
        self.recv(id).await
    }
}
