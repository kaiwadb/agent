//! Tunnel WebSocket client.
//!
//! - Pings the server every [`HEARTBEAT_PERIOD`] so middleboxes (ingress,
//!   load balancers) keep the TCP idle timer reset.
//! - Each command from the server is processed in its own task and streams
//!   its response back as `begin` + binary chunks (each tagged with the
//!   8-byte cmd_id header) + `end`. Multiple in-flight commands can
//!   interleave their chunks safely.
//! - A single writer task drains the shared mpsc into the WebSocket sink
//!   so concurrent handlers don't race on writes.
//! - Reconnect uses exponential backoff capped at [`MAX_RECONNECT_DELAY`].
//!   A 4001 close (server-side lease loss, e.g. another tunnel took over)
//!   bypasses the normal backoff and waits [`EVICTION_BACKOFF`] before
//!   trying again.

use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::Request, protocol::CloseFrame},
};
use tracing::{error, info, warn};

use crate::communication::{ServerCommand, ServerMessage, TunnelControl};
use crate::discovery;
use crate::error::TunnelError;
use crate::query::stream_serializable;

const HEARTBEAT_PERIOD: Duration = Duration::from_secs(15);
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);
const EVICTION_BACKOFF: Duration = Duration::from_secs(30);

/// Server-defined close code: lease lost (another tunnel took over, or our
/// presence row was swept after a network blip).
const CLOSE_CODE_EVICTED: u16 = 4001;

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

pub async fn run(uri: String, token: String, no_scan: bool) -> Result<(), TunnelError> {
    let mut request = uri
        .into_client_request()
        .map_err(|e| TunnelError::Connection(format!("invalid WebSocket URI: {e}")))?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse()?);

    let mut delay = INITIAL_RECONNECT_DELAY;

    loop {
        match connect_and_handle(request.clone(), no_scan).await {
            Ok(SessionEnd::Normal) => {
                info!("connection ended normally");
                return Ok(());
            }
            Ok(SessionEnd::Evicted) => {
                warn!(
                    delay_secs = EVICTION_BACKOFF.as_secs(),
                    "evicted by another tunnel connection; backing off"
                );
                tokio::time::sleep(EVICTION_BACKOFF).await;
                delay = INITIAL_RECONNECT_DELAY;
            }
            Err(e) => {
                error!(error = %e, delay_secs = delay.as_secs(), "connection error, reconnecting");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(MAX_RECONNECT_DELAY);
            }
        }
    }
}

enum SessionEnd {
    Normal,
    Evicted,
}

async fn connect_and_handle(
    request: Request<()>,
    no_scan: bool,
) -> Result<SessionEnd, TunnelError> {
    let (ws_stream, _) = connect_async(request).await?;
    info!("connected");

    let (write, mut read) = ws_stream.split();
    // mpsc capacity controls per-tunnel buffering. Small enough that a
    // slow socket applies backpressure to the producing tasks (and
    // transitively to the upstream DB read) rather than blowing memory.
    let (out_tx, out_rx) = mpsc::channel::<Message>(32);

    let writer_task = tokio::spawn(writer_loop(write, out_rx));

    let mut heartbeat = interval(HEARTBEAT_PERIOD);
    heartbeat.tick().await; // consume immediate first tick

    let outcome = loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let server_msg: ServerMessage = match serde_json::from_str(&text) {
                            Ok(m) => m,
                            Err(e) => {
                                warn!(error = %e, "ignoring unparseable server message");
                                continue;
                            }
                        };
                        spawn_handler(server_msg, out_tx.clone(), no_scan);
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if out_tx.send(Message::Pong(payload)).await.is_err() {
                            break Err(TunnelError::Connection("writer channel closed".into()));
                        }
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Binary(_)))
                        | Some(Ok(Message::Frame(_))) => continue,
                    Some(Ok(Message::Close(frame))) => {
                        info!(?frame, "connection closed by server");
                        break Ok(close_session_end(frame));
                    }
                    Some(Err(e)) => break Err(e.into()),
                    None => break Err(TunnelError::Connection("connection closed unexpectedly".into())),
                }
            }
            _ = heartbeat.tick() => {
                if out_tx.send(Message::Ping("heartbeat".into())).await.is_err() {
                    break Err(TunnelError::Connection("writer channel closed".into()));
                }
            }
        }
    };

    // Drop the sender so the writer drains pending messages and exits.
    drop(out_tx);
    if let Err(e) = writer_task.await {
        warn!(error = %e, "writer task panicked");
    }
    outcome
}

fn close_session_end(frame: Option<CloseFrame>) -> SessionEnd {
    match frame {
        Some(f) if u16::from(f.code) == CLOSE_CODE_EVICTED => SessionEnd::Evicted,
        _ => SessionEnd::Normal,
    }
}

async fn writer_loop(mut write: WsSink, mut rx: mpsc::Receiver<Message>) {
    while let Some(msg) = rx.recv().await {
        if let Err(e) = write.send(msg).await {
            warn!(error = %e, "writer send failed; closing");
            break;
        }
    }
    let _ = write.close().await;
}

fn spawn_handler(msg: ServerMessage, out_tx: mpsc::Sender<Message>, no_scan: bool) {
    tokio::spawn(async move {
        let ServerMessage::Command { id, request } = msg;
        // begin
        send_control(&out_tx, &TunnelControl::Begin { id }).await;

        let result = match request {
            ServerCommand::Query(query) => query.execute_streaming(id, &out_tx).await,
            ServerCommand::Scan(req) => {
                if no_scan {
                    info!(cmd_id = id, "rejecting scan: disabled by operator");
                    Err(crate::error::TunnelError::Connection(
                        "scan disabled by operator".into(),
                    ))
                } else {
                    let report = discovery::scan(req).await;
                    stream_serializable(&report, id, &out_tx).await
                }
            }
        };

        let end = match result {
            Ok(()) => TunnelControl::End { id, error: None },
            Err(e) => TunnelControl::End {
                id,
                error: Some(e.to_string()),
            },
        };
        send_control(&out_tx, &end).await;
    });
}

async fn send_control(out_tx: &mpsc::Sender<Message>, ctrl: &TunnelControl) {
    let Ok(frame) = serde_json::to_string(ctrl) else {
        error!("failed to serialize control frame");
        return;
    };
    if out_tx.send(Message::Text(frame.as_str().into())).await.is_err() {
        warn!("dropping control frame: writer channel closed");
    }
}
