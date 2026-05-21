use std::sync::LazyLock;

use reqwest::Client;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::info;

use crate::communication::frame_chunk;
use crate::error::TunnelError;
use crate::params::ConnectionParams;

static HTTP_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .build()
        .expect("failed to build HTTP client")
});

/// Approximate target frame size for streamed binary chunks. Sized below
/// typical WebSocket buffers — large enough to keep per-frame overhead
/// negligible, small enough to keep memory bounded on slow consumers.
const CHUNK_TARGET: usize = 64 * 1024;

/// Execute a ClickHouse query and stream its rows as a JSON array of
/// row objects, framed for the tunnel protocol.
///
/// We ask ClickHouse for `FORMAT JSONEachRow` (one row per line) so the
/// HTTP response body is naturally streamable, then transform it on the
/// fly into `[{...},{...},...]` shape that the rest of the system
/// expects (matching the old buffered behaviour). Bytes flow:
/// reqwest body → line-batch → `frame_chunk` → mpsc → WS sink.
/// Backpressure propagates end-to-end via the mpsc.
pub async fn execute_streaming(
    conn: &ConnectionParams,
    query: &str,
    id: i64,
    out: &mpsc::Sender<Message>,
) -> Result<(), TunnelError> {
    let ConnectionParams::Clickhouse {
        host,
        port,
        username,
        password,
        database,
        secure,
    } = conn
    else {
        return Err(TunnelError::Connection(
            "clickhouse executor received non-clickhouse connection params".into(),
        ));
    };

    let scheme = if *secure { "https" } else { "http" };
    let url = format!("{scheme}://{host}:{port}/");

    info!(url = %url, "executing clickhouse query");

    let mut request = HTTP_CLIENT.post(&url);
    if let Some(db) = database {
        request = request.query(&[("database", db.as_str())]);
    }
    if let Some(user) = username {
        request = request.basic_auth(user, password.as_deref());
    }

    let body = format!("{query} FORMAT JSONEachRow");
    let response = request
        .body(body)
        .send()
        .await
        .map_err(|e| TunnelError::Connection(format!("clickhouse HTTP error: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(TunnelError::Connection(format!(
            "clickhouse returned {status}: {text}"
        )));
    }

    // Stream the body, splitting on newlines and re-emitting as a JSON
    // array. We hold incomplete trailing bytes between chunks; complete
    // lines are batched into ~`CHUNK_TARGET` outgoing frames.
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;

    let mut leftover: Vec<u8> = Vec::new();
    let mut out_buf: Vec<u8> = Vec::with_capacity(CHUNK_TARGET + 1024);
    let mut emitted_open = false;
    let mut emitted_any_line = false;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            TunnelError::Connection(format!("clickhouse stream read error: {e}"))
        })?;
        leftover.extend_from_slice(&chunk);

        // Emit every complete line in `leftover`; retain the partial tail.
        while let Some(nl) = leftover.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = leftover.drain(..nl).collect();
            leftover.remove(0); // consume the newline
            if line.is_empty() {
                continue;
            }
            if !emitted_open {
                out_buf.push(b'[');
                emitted_open = true;
            } else if emitted_any_line {
                out_buf.push(b',');
            }
            out_buf.extend_from_slice(&line);
            emitted_any_line = true;
            if out_buf.len() >= CHUNK_TARGET {
                flush(&mut out_buf, id, out).await?;
            }
        }
    }

    // ClickHouse normally ends with a trailing newline (already consumed
    // above) but be defensive about a payload that doesn't.
    if !leftover.is_empty() {
        let line = std::mem::take(&mut leftover);
        if !emitted_open {
            out_buf.push(b'[');
            emitted_open = true;
        } else if emitted_any_line {
            out_buf.push(b',');
        }
        out_buf.extend_from_slice(&line);
    }

    if !emitted_open {
        // Zero rows. Emit an empty array.
        out_buf.push(b'[');
    }
    out_buf.push(b']');
    flush(&mut out_buf, id, out).await?;
    Ok(())
}

async fn flush(
    buf: &mut Vec<u8>,
    id: i64,
    out: &mpsc::Sender<Message>,
) -> Result<(), TunnelError> {
    if buf.is_empty() {
        return Ok(());
    }
    let payload = std::mem::take(buf);
    out.send(Message::Binary(frame_chunk(id, &payload)))
        .await
        .map_err(|_| TunnelError::Connection("writer channel closed".into()))?;
    Ok(())
}
