use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::AppState;

const ALLOWED_COMMANDS: &[&str] = &[
    "arm",
    "disarm",
    "setMode",
    "land",
    "return",
    "clearMission",
    "uploadMission",
    "setParameter",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandIntent {
    pub kind: String,
    pub command_id: String,
    pub command: String,
    pub vehicle_id: String,
    pub topic: String,
    #[serde(default)]
    pub args: Value,
    pub expires_at_ms: u64,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandAck {
    pub kind: &'static str,
    pub command_id: String,
    pub command: String,
    pub status: &'static str,
    pub reason: String,
    pub sequence: u64,
    pub received_at_ms: u64,
}

pub(crate) fn handle_command(state: &AppState, intent: CommandIntent, now_ms: u64) -> CommandAck {
    let rejected = |reason: String| CommandAck {
        kind: "commandAck",
        command_id: intent.command_id.clone(),
        command: intent.command.clone(),
        status: "rejected",
        reason,
        sequence: intent.sequence,
        received_at_ms: now_ms,
    };

    if intent.kind != "command" {
        return rejected("message kind is not command".to_string());
    }

    if intent.vehicle_id != state.vehicle_id {
        return rejected(format!("vehicle {} is not selected", intent.vehicle_id));
    }

    if !ALLOWED_COMMANDS.contains(&intent.command.as_str()) {
        return rejected(format!("command {} is not allowlisted", intent.command));
    }

    if !intent
        .topic
        .starts_with(&format!("vehicle/{}/", state.vehicle_id))
    {
        return rejected("command topic does not match selected vehicle".to_string());
    }

    if intent.expires_at_ms < now_ms {
        return rejected("command expired before bridge receive".to_string());
    }

    if !state.accept_command_sequence(intent.sequence) {
        return rejected("command sequence is not monotonic".to_string());
    }

    state.apply_command_effect(&intent.command, &intent.args);

    CommandAck {
        kind: "commandAck",
        command_id: intent.command_id,
        command: intent.command,
        status: "acked",
        reason: format!("accepted {}", intent.args),
        sequence: intent.sequence,
        received_at_ms: now_ms,
    }
}
