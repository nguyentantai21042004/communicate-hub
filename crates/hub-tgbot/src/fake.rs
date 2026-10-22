//! Local Bot API stand-in. Tests are the only callers.
//!
//! The process binds `127.0.0.1` and never dials `api.telegram.org`.
#![allow(clippy::unwrap_used)]

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;

/// One request the fake accepted.
#[derive(Debug, Clone)]
pub struct Hit {
    /// Path, including `/bot<token>/`.
    pub path: String,
    /// `Content-Type` header, or empty when the client sent none.
    pub content_type: String,
    /// Raw body. Empty for `getMe`.
    pub body: Vec<u8>,
    /// When the request line was fully read.
    pub at: tokio::time::Instant,
}

impl Hit {
    /// Last path segment, which is the Bot API method name.
    pub fn method(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or("")
    }

    /// Body as text. Multipart bodies in these tests are ASCII plus a short file.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Scripted HTTP response.
#[derive(Debug, Clone)]
pub struct Reply {
    status: u16,
    body: String,
    delay: Duration,
    hang: bool,
}

impl Reply {
    /// JSON body with the given HTTP status and no delay.
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            delay: Duration::ZERO,
            hang: false,
        }
    }

    /// Same as [`Reply::json`], after `delay`.
    pub fn delayed(status: u16, body: impl Into<String>, delay: Duration) -> Self {
        Self {
            status,
            body: body.into(),
            delay,
            hang: false,
        }
    }

    /// Read the request, then leave the socket open until the client drops it.
    pub fn hang() -> Self {
        Self {
            status: 200,
            body: String::new(),
            delay: Duration::ZERO,
            hang: true,
        }
    }
}

/// One-shot HTTP/1.1 server on `127.0.0.1`.
pub struct Fake {
    /// Origin with no path, for [`crate::BotConfig::api_base_url`].
    pub base: String,
    hits: Arc<Mutex<Vec<Hit>>>,
    /// Increments after a response is written, or when a hang starts.
    completed: watch::Sender<usize>,
    max_inflight: Arc<AtomicUsize>,
    shutdown: watch::Sender<bool>,
    accept_task: JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.accept_task.abort();
    }
}

impl Fake {
    /// Binds a port and answers each request with `handler`.
    pub async fn bind(handler: impl Fn(&Hit) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let (completed, _) = watch::channel(0usize);
        let inflight = Arc::new(AtomicUsize::new(0));
        let max_inflight = Arc::new(AtomicUsize::new(0));
        let (shutdown, shutdown_rx) = watch::channel(false);
        let handler = Arc::new(handler);
        let accept_task = tokio::spawn(accept_loop(
            listener,
            handler,
            Arc::clone(&hits),
            completed.clone(),
            Arc::clone(&inflight),
            Arc::clone(&max_inflight),
            shutdown_rx,
        ));
        Self {
            base: format!("http://{addr}"),
            hits,
            completed,
            max_inflight,
            shutdown,
            accept_task,
        }
    }

    /// Requests in arrival order.
    pub async fn hits(&self) -> Vec<Hit> {
        self.hits.lock().await.clone()
    }

    /// How many requests have been fully read.
    pub async fn hit_count(&self) -> usize {
        self.hits.lock().await.len()
    }

    /// Waits until `n` responses have been written, or hang replies have started.
    pub async fn wait_completed(&self, n: usize) {
        let mut rx = self.completed.subscribe();
        loop {
            if *rx.borrow() >= n {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Highest number of requests inside the handler at once.
    pub fn max_inflight(&self) -> usize {
        self.max_inflight.load(Ordering::SeqCst)
    }

    /// Responses written so far, or hang replies that have started.
    pub fn completed_count(&self) -> usize {
        *self.completed.subscribe().borrow()
    }
}

async fn accept_loop(
    listener: TcpListener,
    handler: Arc<dyn Fn(&Hit) -> Reply + Send + Sync>,
    hits: Arc<Mutex<Vec<Hit>>>,
    completed: watch::Sender<usize>,
    inflight: Arc<AtomicUsize>,
    max_inflight: Arc<AtomicUsize>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let Ok((socket, _)) = accepted else {
                    break;
                };
                let handler = Arc::clone(&handler);
                let hits = Arc::clone(&hits);
                let completed = completed.clone();
                let inflight = Arc::clone(&inflight);
                let max_inflight = Arc::clone(&max_inflight);
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    let _ = handle_conn(
                        socket,
                        handler,
                        &hits,
                        &completed,
                        &inflight,
                        &max_inflight,
                        shutdown,
                    )
                    .await;
                });
            }
        }
    }
}

async fn handle_conn(
    mut socket: TcpStream,
    handler: Arc<dyn Fn(&Hit) -> Reply + Send + Sync>,
    hits: &Mutex<Vec<Hit>>,
    completed: &watch::Sender<usize>,
    inflight: &AtomicUsize,
    max_inflight: &AtomicUsize,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let hit = read_request(&mut socket).await?;
    let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
    max_inflight.fetch_max(now, Ordering::SeqCst);
    let _guard = InflightGuard(inflight);
    hits.lock().await.push(hit.clone());
    let reply = handler(&hit);
    if reply.hang {
        completed.send_modify(|count| *count += 1);
        hang_until_drop(&mut socket, &mut shutdown).await;
        return Ok(());
    }
    if !reply.delay.is_zero() {
        tokio::time::sleep(reply.delay).await;
    }
    write_response(&mut socket, reply.status, &reply.body).await?;
    completed.send_modify(|count| *count += 1);
    Ok(())
}

struct InflightGuard<'a>(&'a AtomicUsize);

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn hang_until_drop(socket: &mut TcpStream, shutdown: &mut watch::Receiver<bool>) {
    let mut byte = [0u8; 1];
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            read = socket.read(&mut byte) => {
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
    }
}

async fn read_request(socket: &mut TcpStream) -> io::Result<Hit> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "closed before headers",
            ));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 65_536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "headers too large",
            ));
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut pending = buf[header_end + 4..].to_vec();
    let (path, content_type, content_length, chunked) = parse_head(&head)?;
    let body = if chunked {
        read_chunked(socket, &mut pending).await?
    } else if let Some(len) = content_length {
        read_exact_body(socket, &mut pending, len).await?
    } else {
        pending
    };
    Ok(Hit {
        path,
        content_type,
        body,
        at: tokio::time::Instant::now(),
    })
}

fn parse_head(head: &str) -> io::Result<(String, String, Option<usize>, bool)> {
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or("");
    let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
    let mut content_type = String::new();
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == "content-type" {
            content_type = value.to_owned();
        } else if name == "content-length" {
            let len = value
                .parse::<usize>()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad content-length"))?;
            content_length = Some(len);
        } else if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        }
    }
    Ok((path, content_type, content_length, chunked))
}

async fn read_exact_body(
    socket: &mut TcpStream,
    pending: &mut Vec<u8>,
    len: usize,
) -> io::Result<Vec<u8>> {
    const MAX_BODY: usize = 2 * 1024 * 1024;
    if len > MAX_BODY {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large"));
    }
    while pending.len() < len {
        let mut tmp = [0u8; 8192];
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "closed before body",
            ));
        }
        pending.extend_from_slice(&tmp[..n]);
    }
    let rest = pending.split_off(len);
    Ok(std::mem::replace(pending, rest))
}

async fn read_chunked(socket: &mut TcpStream, pending: &mut Vec<u8>) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line = read_line(socket, pending).await?;
        let size_hex = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
        if size == 0 {
            loop {
                if read_line(socket, pending).await?.is_empty() {
                    break;
                }
            }
            break;
        }
        if size > 2 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chunk too large",
            ));
        }
        let data = read_exact_body(socket, pending, size).await?;
        out.extend_from_slice(&data);
        let crlf = read_exact_body(socket, pending, 2).await?;
        if crlf != b"\r\n" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chunk missing crlf",
            ));
        }
    }
    Ok(out)
}

async fn read_line(socket: &mut TcpStream, pending: &mut Vec<u8>) -> io::Result<String> {
    loop {
        if let Some(pos) = find_subsequence(pending, b"\r\n") {
            let line = pending.drain(..pos).collect::<Vec<_>>();
            pending.drain(..2);
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
        let mut tmp = [0u8; 1024];
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "closed before line",
            ));
        }
        pending.extend_from_slice(&tmp[..n]);
    }
}

fn find_subsequence(buf: &[u8], needle: &[u8]) -> Option<usize> {
    buf.windows(needle.len())
        .position(|window| window == needle)
}

async fn write_response(socket: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        408 => "Request Timeout",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let bytes = body.as_bytes();
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(bytes).await?;
    socket.shutdown().await?;
    Ok(())
}
