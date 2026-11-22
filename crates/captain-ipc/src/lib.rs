//! The daemon's local socket protocol: one JSON object per line, request then response.
//!
//! A client sends `{"id": 1, "request": {...}}` and gets `{"id": 1, "ok": true, "result": ...}`
//! or `{"id": 1, "ok": false, "error": "..."}`. After a `subscribe` request the daemon keeps the
//! connection open and writes one `{"event": ...}` line per update. The socket file is
//! owner-only and the daemon also checks the peer uid; this crate only frames messages.

use std::path::{Path, PathBuf};

use captain_core::{ChatId, ChatRef, ContactUpsert, DialogFolderSpec, MessageId, OutgoingMessage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// One request to the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// Connection state, pause flag, account and counts.
    Status,
    /// Stop creating tickets. Messages are still stored.
    Pause,
    /// Start creating tickets again.
    Resume,
    /// Stream every update as `{"event": ...}` lines, as `captain listen` prints them.
    Subscribe,
    /// Send a message as the account.
    Send {
        /// The message.
        message: OutgoingMessage,
    },
    /// Save a message's media to a file on this machine.
    Download {
        /// Chat.
        chat: ChatId,
        /// Message.
        message: MessageId,
        /// Destination path.
        out: PathBuf,
    },
    /// Recent chats with their ids, for building `scope.toml`.
    Chats {
        /// How many.
        limit: usize,
    },
    /// Search a chat's history on Telegram with a regex. Reads only; nothing is stored.
    History {
        /// Chat to read.
        chat: ChatId,
        /// Regular expression, matched case-insensitively against text and captions.
        pattern: String,
        /// Start below this message, to page further back; `None` starts at the newest.
        before: Option<MessageId>,
        /// How many messages to read at most.
        scan_max: usize,
    },
    /// List tickets.
    Tickets {
        /// Only this state, by name.
        state: Option<String>,
        /// Only this trigger, by name.
        trigger: Option<String>,
        /// Only tickets opened in the last this many seconds.
        within_secs: Option<i64>,
        /// Only tickets without the owner's label.
        unlabelled: bool,
        /// At most this many.
        limit: Option<usize>,
    },
    /// One ticket with its history.
    Ticket {
        /// Ticket id.
        id: i64,
    },
    /// Set or clear the owner's label on a ticket.
    Label {
        /// Ticket id.
        id: i64,
        /// `want`, `noise`, `skip`, or `None` to clear.
        label: Option<String>,
    },
    /// Create a manual ticket for one message.
    NewTicket {
        /// Chat of the message.
        chat: ChatId,
        /// The message.
        message: MessageId,
        /// Instruction for the runner.
        note: Option<String>,
    },
    /// The phase 1 report over the last `within_secs`.
    Report {
        /// Window length.
        within_secs: i64,
    },
    /// Reread `scope.toml` and `policy.toml`.
    ScopeReload,
    /// List saved Telegram contacts.
    Contacts,
    /// Add or rename a contact without notifying the peer.
    ContactUpsert {
        /// Contact specification.
        spec: ContactUpsert,
    },
    /// Delete our own copy of a private DM history (`revoke: false`).
    DeleteUserHistory {
        /// Target user peer.
        chat: ChatRef,
        /// Clear messages while keeping the dialog slot.
        just_clear: bool,
    },
    /// List current chat folders (tabs).
    Folders,
    /// Create or replace a custom chat folder.
    FolderUpsert {
        /// Folder specification.
        spec: DialogFolderSpec,
    },
    /// Delete a custom chat folder by id.
    FolderDelete {
        /// Folder id (`>= 2`).
        id: i32,
    },
    /// Reorder chat folders (`0` represents All Chats).
    FolderOrder {
        /// Folder ids in display order.
        order: Vec<i32>,
    },
    /// Create a private supergroup owned by this account.
    GroupCreate {
        /// Group title.
        title: String,
        /// Optional description.
        about: String,
    },
    /// Run the AI runner immediately on one ticket id.
    TicketRun {
        /// Ticket id.
        id: i64,
    },
    /// List recent AI runner executions.
    Runs {
        /// Maximum rows to return.
        limit: usize,
    },
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    id: u64,
    #[serde(flatten)]
    body: T,
}

#[derive(Serialize, Deserialize)]
struct RequestBody {
    request: Request,
}

/// One response line from the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Id of the request it answers.
    pub id: u64,
    /// Whether the request succeeded.
    pub ok: bool,
    /// Result on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Message on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    /// A success.
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    /// A failure with a message for the user.
    pub fn err(id: u64, error: impl std::fmt::Display) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error.to_string()),
        }
    }
}

/// Why an IPC call failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No daemon is listening on the socket.
    #[error("the daemon is not running (no socket at {0})")]
    NotRunning(PathBuf),
    /// The socket broke.
    #[error("daemon socket: {0}")]
    Io(#[from] std::io::Error),
    /// A line did not decode.
    #[error("daemon sent an invalid message: {0}")]
    Protocol(String),
    /// The daemon rejected the request.
    #[error("{0}")]
    Remote(String),
}

/// Result alias for IPC.
pub type Result<T> = std::result::Result<T, Error>;

/// Decodes one request line, returning its id and request.
///
/// # Errors
///
/// [`Error::Protocol`] when the line is not a request envelope.
pub fn decode_request(line: &str) -> Result<(u64, Request)> {
    let env: Envelope<RequestBody> =
        serde_json::from_str(line).map_err(|e| Error::Protocol(e.to_string()))?;
    Ok((env.id, env.body.request))
}

/// Encodes any value as one line, newline included.
///
/// # Errors
///
/// [`Error::Protocol`] if the value cannot be serialized.
pub fn encode_line(value: &impl Serialize) -> Result<String> {
    let mut line = serde_json::to_string(value).map_err(|e| Error::Protocol(e.to_string()))?;
    line.push('\n');
    Ok(line)
}

/// A connection to the daemon.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl Client {
    /// Connects to the daemon socket at `path`.
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] when nothing listens there, [`Error::Io`] for other failures.
    pub async fn connect(path: &Path) -> Result<Self> {
        let stream = match UnixStream::connect(path).await {
            Ok(s) => s,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Err(Error::NotRunning(path.to_owned()));
            }
            Err(e) => return Err(e.into()),
        };
        let (read, write) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(read),
            writer: write,
            next_id: 1,
        })
    }

    /// Sends one request and waits for its response.
    ///
    /// # Errors
    ///
    /// [`Error::Remote`] when the daemon rejects it, [`Error::Io`] or [`Error::Protocol`] when
    /// the connection fails.
    pub async fn call(&mut self, request: Request) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let line = encode_line(&Envelope {
            id,
            body: RequestBody { request },
        })?;
        self.writer.write_all(line.as_bytes()).await?;
        let Some(value) = self.next_line().await? else {
            return Err(Error::Protocol("daemon closed the connection".into()));
        };
        let response: Response =
            serde_json::from_value(value).map_err(|e| Error::Protocol(e.to_string()))?;
        match (response.ok, response.result, response.error) {
            (true, result, _) => Ok(result.unwrap_or(Value::Null)),
            (false, _, error) => Err(Error::Remote(error.unwrap_or_default())),
        }
    }

    /// Reads the next line, or `None` when the daemon closed the connection.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] or [`Error::Protocol`] when the line cannot be read or decoded.
    pub async fn next_line(&mut self) -> Result<Option<Value>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        serde_json::from_str(&line)
            .map(Some)
            .map_err(|e| Error::Protocol(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_through_the_wire_format() {
        let line = encode_line(&Envelope {
            id: 7,
            body: RequestBody {
                request: Request::Label {
                    id: 3,
                    label: Some("want".into()),
                },
            },
        })
        .unwrap();
        assert!(line.ends_with('\n'));
        assert!(line.contains(r#""method":"label""#));
        let (id, request) = decode_request(line.trim_end()).unwrap();
        assert_eq!(id, 7);
        assert_eq!(
            request,
            Request::Label {
                id: 3,
                label: Some("want".into())
            }
        );
        let (_, status) = decode_request(r#"{"id":1,"request":{"method":"status"}}"#).unwrap();
        assert_eq!(status, Request::Status);
        assert!(decode_request("{}").is_err());

        for req in [
            Request::Contacts,
            Request::Folders,
            Request::FolderDelete { id: 2 },
            Request::FolderOrder {
                order: vec![0, 2, 3],
            },
            Request::DeleteUserHistory {
                chat: ChatRef::Id(ChatId(42)),
                just_clear: false,
            },
        ] {
            let line = encode_line(&Envelope {
                id: 9,
                body: RequestBody {
                    request: req.clone(),
                },
            })
            .unwrap();
            let (id, decoded) = decode_request(line.trim_end()).unwrap();
            assert_eq!(id, 9);
            assert_eq!(decoded, req);
        }
    }

    #[tokio::test]
    async fn client_talks_to_a_server_over_a_socket() {
        let path = std::env::temp_dir().join(format!("captain-ipc-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let (id, request) = decode_request(&line).unwrap();
                let response = match request {
                    Request::Status => Response::ok(id, serde_json::json!({"paused": false})),
                    _ => Response::err(id, "not here"),
                };
                write
                    .write_all(encode_line(&response).unwrap().as_bytes())
                    .await
                    .unwrap();
            }
        });
        let mut client = Client::connect(&path).await.unwrap();
        assert_eq!(client.call(Request::Status).await.unwrap()["paused"], false);
        let err = client.call(Request::Pause).await.unwrap_err();
        assert_eq!(err.to_string(), "not here");
        drop(client);
        server.await.unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            Client::connect(&path).await,
            Err(Error::NotRunning(_))
        ));
    }
}
