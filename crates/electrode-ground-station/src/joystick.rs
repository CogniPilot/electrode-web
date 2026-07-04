//! Live raw joystick inspector: streams normalized axis values and button
//! states over a WebSocket so the UI can show exactly which channel moves with
//! which stick/button (independent of any mapping).

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Query;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::devices;

// Linux joystick (`js`) event protocol.
const JS_EVENT_BUTTON: u8 = 0x01;
const JS_EVENT_AXIS: u8 = 0x02;
const JS_EVENT_INIT: u8 = 0x80;

#[derive(Deserialize)]
pub(crate) struct JoystickQuery {
    pub device: String,
}

#[derive(Clone, Serialize)]
struct Snapshot {
    device: String,
    name: String,
    /// Normalized axis values in [-1, 1], indexed by axis number.
    axes: Vec<f32>,
    /// Button states (0/1), indexed by button number.
    buttons: Vec<u8>,
}

pub(crate) async fn joystick_ws(
    ws: WebSocketUpgrade,
    Query(query): Query<JoystickQuery>,
) -> Response {
    ws.on_upgrade(move |socket| stream(socket, query.device))
}

/// Only allow reading `/dev/input/js*` nodes, never arbitrary paths.
fn is_allowed(device: &str) -> bool {
    device
        .strip_prefix("/dev/input/js")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

async fn stream(mut socket: WebSocket, device: String) {
    if !is_allowed(&device) {
        let _ = socket.send(Message::Close(None)).await;
        return;
    }

    let name = devices::joystick_name_for(&device).unwrap_or_else(|| "Joystick".to_string());
    let (tx, mut rx) = mpsc::channel::<Snapshot>(16);
    let stop = Arc::new(AtomicBool::new(false));

    {
        let device = device.clone();
        let stop = stop.clone();
        std::thread::spawn(move || read_loop(device, name, tx, stop));
    }

    loop {
        tokio::select! {
            snapshot = rx.recv() => match snapshot {
                Some(snapshot) => {
                    let text = serde_json::to_string(&snapshot).unwrap_or_default();
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(_)) => break,
                _ => {}
            },
        }
    }

    stop.store(true, Ordering::Relaxed);
}

/// Blocking reader: parses `js_event`s and pushes a full snapshot per event.
/// The kernel emits INIT events on open, so the axis/button arrays fill in
/// immediately when a client connects.
fn read_loop(device: String, name: String, tx: mpsc::Sender<Snapshot>, stop: Arc<AtomicBool>) {
    let mut file = match std::fs::File::open(&device) {
        Ok(file) => file,
        Err(_) => return,
    };
    let mut snapshot = Snapshot {
        device,
        name,
        axes: Vec::new(),
        buttons: Vec::new(),
    };
    let mut buf = [0_u8; 8];

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if file.read_exact(&mut buf).is_err() {
            break;
        }
        let value = i16::from_ne_bytes([buf[4], buf[5]]);
        let event_type = buf[6] & !JS_EVENT_INIT;
        let number = usize::from(buf[7]);
        match event_type {
            JS_EVENT_AXIS => {
                ensure_len(&mut snapshot.axes, number + 1, 0.0);
                snapshot.axes[number] = normalize_axis(value);
            }
            JS_EVENT_BUTTON => {
                ensure_len(&mut snapshot.buttons, number + 1, 0);
                snapshot.buttons[number] = u8::from(value != 0);
            }
            _ => continue,
        }
        if tx.blocking_send(snapshot.clone()).is_err() {
            break;
        }
    }
}

fn ensure_len<T: Clone>(vec: &mut Vec<T>, len: usize, fill: T) {
    if vec.len() < len {
        vec.resize(len, fill);
    }
}

fn normalize_axis(value: i16) -> f32 {
    (if value < 0 {
        f32::from(value) / 32768.0
    } else {
        f32::from(value) / 32767.0
    })
    .clamp(-1.0, 1.0)
}
