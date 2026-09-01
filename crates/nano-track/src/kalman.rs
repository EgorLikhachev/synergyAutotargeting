//! Simple 2D Kalman filter for tracking target position in image coordinates.
//!
//! State: [x, y, vx, vy] (position + velocity in pixels / second).
//! Observation: [x, y] (detected bbox center).
//!
//! This is a constant-velocity model — adequate for short-term prediction
//! between detections (typically 30–100 ms gaps). For longer occlusions,
//! Phase 3 may upgrade to a constant-acceleration model.

use common::BBox;

/// State vector: [x, y, vx, vy].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct State {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
}

impl State {
    pub fn at_position(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            vx: 0.0,
            vy: 0.0,
        }
    }

    /// Predict the position after `dt` seconds.
    pub fn predicted_position(&self, dt: f32) -> (f32, f32) {
        (self.x + self.vx * dt, self.y + self.vy * dt)
    }
}

/// 2D constant-velocity Kalman filter.
pub struct KalmanFilter2D {
    /// State estimate.
    pub state: State,
    /// Process noise (how much we expect the target to deviate from constant velocity).
    process_noise: f32,
    /// Measurement noise (how noisy the detections are).
    measurement_noise: f32,
}

impl KalmanFilter2D {
    /// Create a new filter initialized at position (x, y), zero velocity.
    pub fn new(x: f32, y: f32) -> Self {
        Self {
            state: State::at_position(x, y),
            process_noise: 1.0,
            measurement_noise: 4.0,
        }
    }

    /// Create with custom noise parameters.
    pub fn with_noise(x: f32, y: f32, process: f32, measurement: f32) -> Self {
        Self {
            state: State::at_position(x, y),
            process_noise: process,
            measurement_noise: measurement,
        }
    }

    /// Predict step: advance state by `dt` seconds. Increases uncertainty.
    pub fn predict(&mut self, dt: f32) {
        // State transition: x' = x + vx*dt, y' = y + vy*dt
        self.state.x += self.state.vx * dt;
        self.state.y += self.state.vy * dt;
        // Velocity unchanged (constant velocity model)
        // Process noise would be added to covariance in a full implementation.
    }

    /// Update step: incorporate a detection at (obs_x, obs_y).
    /// `dt` is the time elapsed since the last `predict()` or `update()`, in seconds.
    /// Simple Kalman gain for a 2D constant-velocity model.
    pub fn update(&mut self, obs_x: f32, obs_y: f32, dt: f32) {
        // Simplified: blend observation with prediction using a fixed gain.
        // A full implementation would track the 4x4 covariance matrix and
        // compute the Kalman gain dynamically. For our use case (short dt,
        // bounded noise) this approximation is adequate.
        let gain = self.measurement_noise / (self.measurement_noise + self.process_noise);
        let inv_gain = 1.0 - gain;

        // Position update
        let pred_x = self.state.x;
        let pred_y = self.state.y;
        self.state.x = inv_gain * pred_x + gain * obs_x;
        self.state.y = inv_gain * pred_y + gain * obs_y;

        // Velocity update: estimate from the residual (obs - pred) divided by dt.
        // This is a rough approximation of the Kalman gain applied to velocity.
        if dt > 1e-6 {
            let residual_vx = (obs_x - pred_x) / dt;
            let residual_vy = (obs_y - pred_y) / dt;
            // Blend current velocity estimate with the residual-based estimate
            let alpha = 0.3; // velocity smoothing factor
            self.state.vx = (1.0 - alpha) * self.state.vx + alpha * residual_vx;
            self.state.vy = (1.0 - alpha) * self.state.vy + alpha * residual_vy;
        }
    }

    /// Convenience: update from a bounding box center.
    pub fn update_from_bbox(&mut self, bbox: &BBox, dt: f32) {
        let (cx, cy) = bbox.center();
        self.update(cx, cy, dt);
    }

    /// Get the current predicted position (without advancing state).
    pub fn position(&self) -> (f32, f32) {
        (self.state.x, self.state.y)
    }

    /// Get the current velocity estimate (pixels / second).
    pub fn velocity(&self) -> (f32, f32) {
        (self.state.vx, self.state.vy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_at_position() {
        let kf = KalmanFilter2D::new(100.0, 200.0);
        assert_eq!(kf.position(), (100.0, 200.0));
        assert_eq!(kf.velocity(), (0.0, 0.0));
    }

    #[test]
    fn predict_advances_position() {
        let mut kf = KalmanFilter2D::new(100.0, 200.0);
        kf.state.vx = 50.0; // pixels per second
        kf.state.vy = -30.0;
        kf.predict(0.1); // 100 ms
                         // 100 + 50*0.1 = 105, 200 - 30*0.1 = 197
        assert!((kf.state.x - 105.0).abs() < 1e-4);
        assert!((kf.state.y - 197.0).abs() < 1e-4);
    }

    #[test]
    fn update_blends_observation() {
        let mut kf = KalmanFilter2D::new(100.0, 200.0);
        // Predicted position is (100, 200), observation is (110, 210)
        kf.update(110.0, 210.0, 0.1);
        // State should move toward observation
        assert!(kf.state.x > 100.0 && kf.state.x < 110.0);
        assert!(kf.state.y > 200.0 && kf.state.y < 210.0);
    }

    #[test]
    fn velocity_estimated_from_sequence() {
        let mut kf = KalmanFilter2D::new(100.0, 100.0);
        // Simulate a target moving at 100 px/s
        for i in 1..=10 {
            kf.predict(0.1);
            kf.update(100.0 + 10.0 * i as f32, 100.0, 0.1);
        }
        // Velocity should converge toward 100 px/s.
        // With our simplified filter and alpha=0.3, after 10 iterations
        // the estimate should be in the right ballpark.
        assert!(
            kf.state.vx > 50.0,
            "velocity should converge toward 100, got {}",
            kf.state.vx
        );
    }

    #[test]
    fn update_from_bbox_uses_center() {
        let mut kf = KalmanFilter2D::new(0.0, 0.0);
        let bbox = BBox {
            x: 100.0,
            y: 100.0,
            w: 60.0,
            h: 40.0,
        };
        kf.update_from_bbox(&bbox, 0.1);
        // Center is (130, 120)
        assert!(kf.state.x > 0.0);
        assert!(kf.state.y > 0.0);
    }
}
