//! Local hardware discovery: joystick and serial device enumeration.

use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct Device {
    pub kind: &'static str,
    pub path: String,
    pub name: String,
}

#[derive(Serialize)]
pub(crate) struct Devices {
    pub joysticks: Vec<Device>,
    pub serial: Vec<Device>,
}

pub(crate) fn list() -> Devices {
    Devices {
        joysticks: list_joysticks(),
        serial: list_serial(),
    }
}

/// Enumerate Linux joystick nodes (`/dev/input/js*`), naming each from sysfs.
fn list_joysticks() -> Vec<Device> {
    let mut devices = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/dev/input") {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let node = file_name.to_string_lossy();
            let is_js = node
                .strip_prefix("js")
                .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()));
            if is_js {
                devices.push(Device {
                    kind: "joystick",
                    name: joystick_name(&node).unwrap_or_else(|| "Joystick".to_string()),
                    path: format!("/dev/input/{node}"),
                });
            }
        }
    }
    devices.sort_by(|a, b| a.path.cmp(&b.path));
    devices
}

/// Best-effort name for a joystick node name (e.g. `js0`) from sysfs.
fn joystick_name(node: &str) -> Option<String> {
    let path = format!("/sys/class/input/{node}/device/name");
    std::fs::read_to_string(path)
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Best-effort name for a full joystick path (e.g. `/dev/input/js0`).
pub(crate) fn joystick_name_for(path: &str) -> Option<String> {
    let node = path.strip_prefix("/dev/input/")?;
    joystick_name(node)
}

/// Enumerate USB/ACM serial ports (`/dev/ttyACM*`, `/dev/ttyUSB*`).
fn list_serial() -> Vec<Device> {
    let mut devices = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/dev") {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let node = file_name.to_string_lossy();
            if node.starts_with("ttyACM") || node.starts_with("ttyUSB") {
                devices.push(Device {
                    kind: "serial",
                    path: format!("/dev/{node}"),
                    name: node.to_string(),
                });
            }
        }
    }
    devices.sort_by(|a, b| a.path.cmp(&b.path));
    devices
}
