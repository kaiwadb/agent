mod clickhouse;
mod mongo;
mod mssql;
mod mysql;
mod postgres;

use mongodb::bson::Document;
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_value};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info};

use crate::communication::frame_chunk;
use crate::engine::Engine;
use crate::error::TunnelError;
use crate::params::ConnectionParams;

/// Target frame size for buffered engines that don't stream natively. The
/// query result Value is serialized to JSON once then sliced into binary
/// chunks of this size.
const CHUNK_TARGET: usize = 64 * 1024;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Query {
    pub connection: ConnectionParams,
    /// Engine version metadata. Kept on the wire for future
    /// version-conditional logic; not used for connection (backend
    /// validates engine/connection variants match before forwarding).
    pub engine: Engine,
    pub data: Value,
}

#[derive(Deserialize)]
struct MongoData {
    collection: String,
    pipeline: Vec<Document>,
}

impl Query {
    /// Execute the query and stream its response into `out` as framed
    /// binary chunks tagged with `id`. Caller is responsible for sending
    /// the surrounding `begin`/`end` control frames.
    pub async fn execute_streaming(
        self,
        id: i64,
        out: &mpsc::Sender<Message>,
    ) -> Result<(), TunnelError> {
        let start = Instant::now();
        let Query {
            connection,
            engine: _,
            data,
        } = self;

        let outcome = match &connection {
            ConnectionParams::Clickhouse { .. } => {
                let query: String = from_value(data)?;
                info!("executing clickhouse query");
                debug!(query = %query);
                clickhouse::execute_streaming(&connection, &query, id, out).await
            }
            ConnectionParams::Postgres { .. } => {
                let query: String = from_value(data)?;
                info!("executing postgresql query");
                debug!(query = %query);
                let value = postgres::execute(&connection, &query).await?;
                stream_value(&value, id, out).await
            }
            ConnectionParams::Mysql { .. } => {
                let query: String = from_value(data)?;
                info!("executing mysql query");
                debug!(query = %query);
                let value = mysql::execute(&connection, &query).await?;
                stream_value(&value, id, out).await
            }
            ConnectionParams::Mariadb { .. } => {
                // MariaDB shares the MySQL wire protocol — the executor
                // dispatches on the ConnectionParams variant internally and
                // uses the same sqlx `mysql` driver.
                let query: String = from_value(data)?;
                info!("executing mariadb query");
                debug!(query = %query);
                let value = mysql::execute(&connection, &query).await?;
                stream_value(&value, id, out).await
            }
            ConnectionParams::Mssql { .. } => {
                let query: String = from_value(data)?;
                info!("executing mssql query");
                debug!(query = %query);
                let value = mssql::execute(&connection, &query).await?;
                stream_value(&value, id, out).await
            }
            ConnectionParams::Mongo { .. } => {
                let MongoData { collection, pipeline } = from_value(data)?;
                info!(collection = %collection, "executing mongodb query");
                debug!(pipeline = ?pipeline);
                let value = mongo::execute(&connection, collection, pipeline).await?;
                stream_value(&value, id, out).await
            }
        };

        let duration = start.elapsed();
        match &outcome {
            Ok(()) => info!(duration_ms = duration.as_millis(), "query completed"),
            Err(e) => error!(duration_ms = duration.as_millis(), error = %e, "query failed"),
        }
        outcome
    }
}

/// Serialize `value` to JSON and stream as binary chunks. Used by engines
/// that don't (yet) stream natively; their full result must fit in memory
/// on the tunnel side but the WebSocket frame size limit no longer
/// constrains the payload.
async fn stream_value(
    value: &Value,
    id: i64,
    out: &mpsc::Sender<Message>,
) -> Result<(), TunnelError> {
    let bytes = serde_json::to_vec(value)?;
    for chunk in bytes.chunks(CHUNK_TARGET) {
        out.send(Message::Binary(frame_chunk(id, chunk)))
            .await
            .map_err(|_| TunnelError::Connection("writer channel closed".into()))?;
    }
    Ok(())
}

/// Serialize an arbitrary `Serialize` value and stream it. Used for scan
/// reports which never get large.
pub async fn stream_serializable<T: Serialize>(
    value: &T,
    id: i64,
    out: &mpsc::Sender<Message>,
) -> Result<(), TunnelError> {
    let bytes = serde_json::to_vec(value)?;
    for chunk in bytes.chunks(CHUNK_TARGET) {
        out.send(Message::Binary(frame_chunk(id, chunk)))
            .await
            .map_err(|_| TunnelError::Connection("writer channel closed".into()))?;
    }
    Ok(())
}
