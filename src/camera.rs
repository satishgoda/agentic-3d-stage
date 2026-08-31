//! Two cameras, one window: authored (proof) vs review/fly (human only).

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, MutexGuard};

/// Fixed look-at used by authored evidence, queries, and default window.
pub const EYE: Vec3 = Vec3::new(0.35, 1.75, 7.8);
pub const TARGET: Vec3 = Vec3::new(0.0, 0.55, 0.0);
pub const UP: Vec3 = Vec3::Y;
pub const FOV_Y_DEG: f32 = 45.0;
pub const NEAR: f32 = 0.1;
pub const FAR: f32 = 100.0;
/// Matches the default window LogicalSize 960×640.
pub const DEFAULT_ASPECT: f32 = 960.0 / 640.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    Authored,
    Review,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LookAt {
    pub eye: [f32; 3],
    pub target: [f32; 3],
    pub up: [f32; 3],
    pub fov_y_deg: f32,
    pub near: f32,
    pub far: f32,
}

impl LookAt {
    pub fn authored_default() -> Self {
        Self {
            eye: EYE.to_array(),
            target: TARGET.to_array(),
            up: UP.to_array(),
            fov_y_deg: FOV_Y_DEG,
            near: NEAR,
            far: FAR,
        }
    }

    pub fn eye_v(&self) -> Vec3 {
        Vec3::from_array(self.eye)
    }
    pub fn target_v(&self) -> Vec3 {
        Vec3::from_array(self.target)
    }
    pub fn up_v(&self) -> Vec3 {
        Vec3::from_array(self.up)
    }

    pub fn view_matrix(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye_v(), self.target_v(), self.up_v())
    }

    pub fn proj_matrix(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(
            self.fov_y_deg.to_radians(),
            aspect.max(0.01),
            self.near,
            self.far,
        )
    }

    pub fn view_proj_matrix(&self, aspect: f32) -> Mat4 {
        self.proj_matrix(aspect) * self.view_matrix()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ViewportCameras {
    pub authored: LookAt,
    pub review: LookAt,
    pub mode: ViewMode,
}

impl ViewportCameras {
    pub fn new() -> Self {
        let a = LookAt::authored_default();
        Self {
            authored: a.clone(),
            review: a,
            mode: ViewMode::Authored,
        }
    }

    pub fn active(&self) -> &LookAt {
        match self.mode {
            ViewMode::Authored => &self.authored,
            ViewMode::Review => &self.review,
        }
    }

    pub fn effective_name(&self) -> &'static str {
        match self.mode {
            ViewMode::Authored => "authored",
            ViewMode::Review => "review",
        }
    }
}

impl Default for ViewportCameras {
    fn default() -> Self {
        Self::new()
    }
}

static RIG: Mutex<ViewportCameras> = Mutex::new(ViewportCameras {
    authored: LookAt {
        eye: [0.35, 1.75, 7.8],
        target: [0.0, 0.55, 0.0],
        up: [0.0, 1.0, 0.0],
        fov_y_deg: 45.0,
        near: 0.1,
        far: 100.0,
    },
    review: LookAt {
        eye: [0.35, 1.75, 7.8],
        target: [0.0, 0.55, 0.0],
        up: [0.0, 1.0, 0.0],
        fov_y_deg: 45.0,
        near: 0.1,
        far: 100.0,
    },
    mode: ViewMode::Authored,
});

pub fn rig() -> MutexGuard<'static, ViewportCameras> {
    RIG.lock().unwrap_or_else(|p| p.into_inner())
}

/// Evidence, queries, beauty, objectId — always authored. Never review.
pub fn view_matrix() -> Mat4 {
    LookAt::authored_default().view_matrix()
}

pub fn proj_matrix(aspect: f32) -> Mat4 {
    LookAt::authored_default().proj_matrix(aspect)
}

pub fn view_proj_matrix(aspect: f32) -> Mat4 {
    LookAt::authored_default().view_proj_matrix(aspect)
}

/// Window draw: follows viewMode (review is human fly, not proof).
pub fn window_view_proj_matrix(aspect: f32) -> Mat4 {
    rig().active().view_proj_matrix(aspect)
}

/// Pull authored look-at back so a sphere at `center` with `radius` fits the FOV.
pub fn frame_authored(center: Vec3, radius: f32) {
    let mut r = rig();
    let back = (r.authored.eye_v() - r.authored.target_v())
        .try_normalize()
        .unwrap_or(Vec3::Z);
    let half = (r.authored.fov_y_deg.to_radians() * 0.5).tan().max(0.05);
    let dist = (radius.max(0.5) / half) * 1.35;
    r.authored.target = center.to_array();
    r.authored.eye = (center + back * dist).to_array();
}

pub fn camera_basis() -> (Vec3, Vec3, Vec3) {
    let look = LookAt::authored_default();
    let forward = (look.target_v() - look.eye_v()).normalize();
    let right = forward.cross(look.up_v()).normalize();
    let up = right.cross(forward).normalize();
    (right, up, forward)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_stays_authored_when_review_is_active() {
        {
            let mut r = rig();
            r.mode = ViewMode::Review;
            r.review.eye = [0.0, 3.0, 12.0];
        }
        let authored = view_proj_matrix(1.5);
        let window = window_view_proj_matrix(1.5);
        assert_ne!(authored, window);
        {
            let mut r = rig();
            *r = ViewportCameras::new();
        }
        assert_eq!(
            view_proj_matrix(1.5),
            LookAt::authored_default().view_proj_matrix(1.5)
        );
    }
}
