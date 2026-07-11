//! Supervises the `electrode-manual-control-bridge` child process. Applying a
//! mapping = (re)launching the bridge with the arguments derived from the
//! current profile, so all the real joystick→Synapse work stays in the proven
//! bridge binary.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;

pub(crate) struct Supervisor {
    bin: PathBuf,
    child: Mutex<Option<Child>>,
}

impl Supervisor {
    pub(crate) fn manual_control() -> Self {
        Self {
            bin: resolve_sibling_bin(
                "ELECTRODE_MANUAL_BRIDGE_BIN",
                "electrode-manual-control-bridge",
            ),
            child: Mutex::new(None),
        }
    }

    pub(crate) fn ppm_bridge() -> Self {
        Self {
            bin: resolve_sibling_bin("ELECTRODE_PPM_BRIDGE_BIN", "electrode-ppm-bridge"),
            child: Mutex::new(None),
        }
    }

    /// Path to the bridge binary this supervisor will launch.
    pub(crate) fn bin_display(&self) -> String {
        self.bin.display().to_string()
    }

    /// Whether a bridge child is currently running (reaps it if it has exited).
    pub(crate) fn running(&self) -> bool {
        let mut guard = self.child.lock().expect("supervisor lock poisoned");
        match guard.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(_)) => {
                    *guard = None;
                    false
                }
                Ok(None) => true,
                Err(_) => true,
            },
            None => false,
        }
    }

    /// Launch (or relaunch) the bridge with the given arguments.
    pub(crate) fn start(&self, args: &[String]) -> std::io::Result<()> {
        self.stop();
        let child = Command::new(&self.bin).args(args).spawn()?;
        *self.child.lock().expect("supervisor lock poisoned") = Some(child);
        Ok(())
    }

    /// Stop the bridge if running.
    pub(crate) fn stop(&self) {
        if let Some(mut child) = self.child.lock().expect("supervisor lock poisoned").take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// If a bridge is currently running, relaunch it with fresh args (used when
    /// the mapping changes live); otherwise do nothing.
    pub(crate) fn restart_if_running(&self, args: &[String]) -> std::io::Result<()> {
        if self.running() {
            self.start(args)?;
        }
        Ok(())
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Some(mut child) = self
            .child
            .get_mut()
            .expect("supervisor lock poisoned")
            .take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Resolve a sibling bridge binary: honor env override, else look next to this
/// executable (they build into the same target dir).
fn resolve_sibling_bin(env_var: &str, binary_name: &str) -> PathBuf {
    if let Ok(path) = std::env::var(env_var) {
        return PathBuf::from(path);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join(binary_name);
        }
    }
    PathBuf::from(binary_name)
}
