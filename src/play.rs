//! E: playhead clock only. Not gameplay. behaviorRuntime stays false.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlayState {
    pub playing: bool,
    pub paused: bool,
    pub time: f32,
    pub duration: f32,
}

impl Default for PlayState {
    fn default() -> Self {
        Self {
            playing: false,
            paused: false,
            time: 0.0,
            duration: 10.0,
        }
    }
}

static STATE: Mutex<PlayState> = Mutex::new(PlayState {
    playing: false,
    paused: false,
    time: 0.0,
    duration: 10.0,
});

pub fn snapshot() -> PlayState {
    *STATE.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn apply(action: &str, time: Option<f32>) -> Result<PlayState, String> {
    let mut s = STATE.lock().unwrap_or_else(|p| p.into_inner());
    match action {
        "enter" | "play" => {
            s.playing = true;
            s.paused = false;
        }
        "stop" => {
            s.playing = false;
            s.paused = false;
            s.time = 0.0;
        }
        "pause" => {
            if s.playing {
                s.paused = true;
                s.playing = false;
            }
        }
        "seek" => {
            let t = time.ok_or("seek_needs_time")?;
            s.time = t.clamp(0.0, s.duration);
        }
        "step" => {
            s.time = (s.time + 1.0 / 24.0).min(s.duration);
            s.playing = false;
            s.paused = true;
        }
        other => return Err(format!("unknown_play_action:{other}")),
    }
    Ok(*s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_stop_resets_time() {
        let _ = apply("stop", None);
        assert!(apply("enter", None).unwrap().playing);
        assert_eq!(apply("stop", None).unwrap().time, 0.0);
    }
}
