mod communication;
mod query;

use clap::Parser;
use communication::{ServerCommand, ServerCommandMsg};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::time::interval;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::Request},
};

use crate::communication::{AgentResult, AgentResultMsg, PayloadFromAgent};

const HEARTBEAT_PERIOD: Duration = Duration::from_secs(15);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(name = "kaiwadb-agent")]
#[command(about = "KaiwaDB Agent WebSocket client")]
#[command(version)]
struct Args {
    /// WebSocket URL to connect to
    #[arg(short, long, default_value = "wss://api.kaiwadb.com/agent/connector")]
    uri: String,

    /// Authentication token
    #[arg(short, long, env = "KAIWADB_AGENT_TOKEN")]
    token: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("🚀 Starting KaiwaDB Agent");
    println!("📡 Connecting to: {}", args.uri);

    let mut request = args.uri.into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {}", args.token).parse()?);

    loop {
        match connect_and_handle(request.clone()).await {
            Ok(_) => {
                println!("Connection ended normally");
                break;
            }
            Err(e) => {
                println!("❌ Connection error: {}", e);

                println!("⏳ Reconnecting in {}s...", RECONNECT_DELAY.as_secs());
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
    Ok(())
}

async fn connect_and_handle(request: Request<()>) -> Result<(), Box<dyn std::error::Error>> {
    let (ws_stream, _) = connect_async(request).await?;

    println!("✅ Connected!");

    let (mut write, mut read) = ws_stream.split();
    let mut heartbeat_interval = interval(HEARTBEAT_PERIOD);

    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(e) = handle_server_message(text.to_string(), &mut write).await {
                            println!("Error handling message: {}", e);
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        write.send(Message::Pong(payload)).await?;
                    }
                    Some(Ok(Message::Pong(_))) => {
                        continue
                    }
                    Some(Ok(Message::Close(frame))) => {
                        println!("Connection closed by server: {:?}", frame);
                        break;
                    }
                    Some(Ok(Message::Binary(_))) => {
                        continue
                    }
                    Some(Ok(Message::Frame(_))) => {
                        continue
                    }
                    Some(Err(e)) => {
                        return Err(format!("WebSocket error: {}", e).into());
                    }
                    None => {
                        return Err("Connection closed unexpectedly".into());
                    }
                }
            }
            _ = heartbeat_interval.tick() => {
                if let Err(e) = write.send(Message::Ping("heartbeat".into())).await {
                    return Err(format!("Failed to send heartbeat: {}", e).into());
                }
            }
        }
    }
    Ok(())
}

async fn handle_server_message(
    text: String,
    write: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg: ServerCommandMsg = serde_json::from_str(&text)
        .map_err(|e| format!("Failed to parse server message: {}", e))?;

    match msg.payload {
        ServerCommand::Query(query) => {
            let data = query.execute().await?;
            println!("Query completed successfully");
            let response = PayloadFromAgent::Result(AgentResultMsg {
                channel: msg.channel,
                payload: AgentResult::QueryResult(data),
            });
            let response_txt = serde_json::to_string(&response)?;
            write
                .send(Message::Text(response_txt.as_str().into()))
                .await?;
            println!("Sent response");
        }
    }

    Ok(())
}
