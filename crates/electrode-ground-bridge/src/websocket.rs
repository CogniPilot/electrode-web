use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

use crate::{
    commands::{handle_command, CommandIntent},
    state::AppState,
};

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut frames = state.frame_tx.subscribe();
    let mut command_sequence = 0_u64;

    // Send the current discovery catalog immediately so the panel populates on
    // connect instead of waiting for the next emitter tick.
    if sender
        .send(Message::Text(state.zenoh.catalog_message().into()))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            broadcast = frames.recv() => {
                match broadcast {
                    Ok(text) => {
                        if sender.send(Message::Text(text.into())).await.is_err() {
                            return;
                        }
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "websocket client lagged behind telemetry broadcast");
                    }
                    Err(RecvError::Closed) => return,
                }
            }
            incoming = receiver.next() => {
                let Some(Ok(message)) = incoming else {
                    return;
                };

                match message {
                    Message::Text(text) => {
                        if handle_control(&state, &text) {
                            // Control messages get an immediate fresh catalog echo.
                            if sender
                                .send(Message::Text(state.zenoh.catalog_message().into()))
                                .await
                                .is_err()
                            {
                                return;
                            }
                            continue;
                        }

                        let now_ms = now_ms();
                        let ack = match serde_json::from_str::<CommandIntent>(&text) {
                            Ok(intent) => handle_command(&state, intent, now_ms),
                            Err(err) => {
                                command_sequence += 1;
                                crate::commands::CommandAck {
                                    kind: "commandAck",
                                    command_id: format!("parse-error-{command_sequence}"),
                                    command: "unknown".to_string(),
                                    status: "rejected",
                                    reason: err.to_string(),
                                    sequence: command_sequence,
                                    received_at_ms: now_ms,
                                }
                            }
                        };

                        if let Ok(text) = serde_json::to_string(&ack) {
                            if sender.send(Message::Text(text.into())).await.is_err() {
                                return;
                            }
                        }
                    }
                    Message::Close(_) => return,
                    _ => {}
                }
            }
        }
    }
}

/// Handle a `control` message (topic subscription changes). Returns true when
/// the text was a control message and should not be treated as a command.
fn handle_control(state: &AppState, text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    if value.get("kind").and_then(Value::as_str) != Some("control") {
        return false;
    }

    match value.get("action").and_then(Value::as_str) {
        Some("setSubscriptions") => {
            let keys = value
                .get("keys")
                .and_then(Value::as_array)
                .map(|keys| {
                    keys.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            state.zenoh.set_selection(keys);
            true
        }
        // `requestCatalog` (and any unknown control action) just triggers the
        // catalog echo the caller performs on a true return.
        _ => true,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before unix epoch")
        .as_millis() as u64
}
