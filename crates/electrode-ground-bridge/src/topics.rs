use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TopicDefinition {
    pub topic: String,
    pub schema: &'static str,
    pub expected_rate_hz: f32,
    pub stale_timeout_ms: u64,
    pub loggable: bool,
}

pub(crate) fn topic_definitions(vehicle_id: &str) -> Vec<TopicDefinition> {
    let prefix = format!("vehicle/{vehicle_id}");
    vec![
        TopicDefinition {
            topic: format!("{prefix}/state/pose"),
            schema: "Pose",
            expected_rate_hz: 20.0,
            stale_timeout_ms: 250,
            loggable: true,
        },
        TopicDefinition {
            topic: format!("{prefix}/state/velocity"),
            schema: "Velocity",
            expected_rate_hz: 20.0,
            stale_timeout_ms: 250,
            loggable: true,
        },
        TopicDefinition {
            topic: format!("{prefix}/state/attitude"),
            schema: "Attitude",
            expected_rate_hz: 30.0,
            stale_timeout_ms: 200,
            loggable: true,
        },
        TopicDefinition {
            topic: format!("{prefix}/state/battery"),
            schema: "Battery",
            expected_rate_hz: 2.0,
            stale_timeout_ms: 2000,
            loggable: true,
        },
        TopicDefinition {
            topic: format!("{prefix}/state/link"),
            schema: "LinkStatus",
            expected_rate_hz: 2.0,
            stale_timeout_ms: 2000,
            loggable: true,
        },
        TopicDefinition {
            topic: format!("{prefix}/state/mode"),
            schema: "ModeState",
            expected_rate_hz: 2.0,
            stale_timeout_ms: 2000,
            loggable: true,
        },
        TopicDefinition {
            topic: format!("{prefix}/state/localization"),
            schema: "LocalizationState",
            expected_rate_hz: 5.0,
            stale_timeout_ms: 1000,
            loggable: true,
        },
    ]
}
