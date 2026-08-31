//! D7 RTX lifecycle. Active lighting is never implied by adapter support.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RtxState {
    pub supported: bool,
    pub requested: bool,
    pub configured: bool,
    pub building: bool,
    pub active: bool,
    pub stale: bool,
    pub failed: bool,
}

impl Default for RtxState {
    fn default() -> Self {
        Self {
            supported: false,
            requested: false,
            configured: false,
            building: false,
            active: false,
            stale: false,
            failed: false,
        }
    }
}

static STATE: Mutex<RtxState> = Mutex::new(RtxState {
    supported: false,
    requested: false,
    configured: false,
    building: false,
    active: false,
    stale: false,
    failed: false,
});

pub fn snapshot() -> RtxState {
    *STATE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Request RTX. Never sets active; this crate has no RTX pipeline.
pub fn set_requested(requested: bool) -> RtxState {
    let mut s = STATE.lock().unwrap_or_else(|p| p.into_inner());
    s.requested = requested;
    if !requested {
        s.configured = false;
        s.building = false;
        s.stale = false;
        s.failed = false;
    }
    s.active = false;
    *s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_does_not_activate() {
        let s = set_requested(true);
        assert!(s.requested);
        assert!(!s.active);
        assert!(!s.supported);
        let _ = set_requested(false);
    }
}
