//! Симулятор платформы наведения (фаза D): цель фиксирована в мире, камера
//! на платформе с инерцией первого порядка; RC-мкс задают угловую скорость.
//! Служит стеном контура без железа (ROADMAP: симулятор с инерцией).

/// Платформа: camera_angle (px на полукадре, знаковое) отстаёт от уставки
/// с постоянной времени tau; скорость пропорциональна (ch − 1500)/500 × max_rate.
pub struct PlatformSim {
    /// Позиция оси X камеры (px, 0 — центр).
    pub pos_x: f32,
    /// Позиция оси Y камеры (px, 0 — центр).
    pub pos_y: f32,
    /// Максимальная скорость слежения, px/с при полном отклонении стика.
    pub max_rate_px_s: f32,
    /// Постоянная времени инерции, с (жёсткий монтаж — заметная).
    pub tau: f32,
    rate_x: f32,
    rate_y: f32,
}

impl PlatformSim {
    pub fn new(max_rate_px_s: f32, tau: f32) -> Self {
        Self {
            pos_x: 0.0,
            pos_y: 0.0,
            max_rate_px_s,
            tau,
            rate_x: 0.0,
            rate_y: 0.0,
        }
    }

    /// Шаг симуляции: ch — RC-каналы (используются ch0/ch1).
    pub fn step(&mut self, ch: &[u16; 16], dt: f32) {
        let cmd_x = (ch[0] as f32 - 1500.0) / 500.0; // −1..1
        let cmd_y = (ch[1] as f32 - 1500.0) / 500.0;
        // Первая инерция: скорость стремится к уставке с тау.
        let target_x = cmd_x * self.max_rate_px_s;
        let target_y = cmd_y * self.max_rate_px_s;
        let a = dt / self.tau.max(1e-3);
        self.rate_x += (target_x - self.rate_x) * a.min(1.0);
        self.rate_y += (target_y - self.rate_y) * a.min(1.0);
        self.pos_x += self.rate_x * dt;
        self.pos_y += self.rate_y * dt;
    }

    /// Пиксельные координаты неподвижной цели в кадре камеры (frame w×h):
    /// цель в мире на (tx, ty) px от центра.
    pub fn target_in_frame(&self, frame: (u32, u32), target_offset: (f32, f32)) -> (f32, f32) {
        (
            frame.0 as f32 / 2.0 + target_offset.0 - self.pos_x,
            frame.1 as f32 / 2.0 + target_offset.1 - self.pos_y,
        )
    }
}

/// Стык передачи команд: UART на борту, симулятор/лог в тестах.
pub trait AimLink: Send {
    fn send_rc(&mut self, ch: &[u16; 16]) -> anyhow::Result<()>;
}

/// Пустой линк (телеметрия в лог).
pub struct NoopLink {
    pub sent: std::sync::atomic::AtomicU64,
}

impl NoopLink {
    pub fn new() -> Self {
        Self {
            sent: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl AimLink for NoopLink {
    fn send_rc(&mut self, _ch: &[u16; 16]) -> anyhow::Result<()> {
        self.sent
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::law::{AimConfig, AimLaw};

    /// Замкнутый контур на симуляторе: неподвижная цель со смещением —
    /// платформа должна привести её в центр (критерий ROADMAP ±30 px).
    #[test]
    fn closed_loop_converges_to_center() {
        let mut sim = PlatformSim::new(400.0, 0.3); // 400 px/с, инерция 0.3 с
        let mut law = AimLaw::new(AimConfig {
            x: crate::law::AxisParams {
                kp: 1.5,
                slew_us: 30.0,
                ..Default::default()
            },
            y: crate::law::AxisParams {
                kp: 1.5,
                slew_us: 30.0,
                ..Default::default()
            },
            ..Default::default()
        });
        let frame = (640u32, 480u32);
        let target = (120.0f32, -80.0f32); // цель в мире
        let dt = 1.0 / 30.0;
        let mut converged_at = None;
        for i in 0..300 {
            // 10 секунд
            let tp = sim.target_in_frame(frame, target);
            let err = (tp.0 - frame.0 as f32 / 2.0, tp.1 - frame.1 as f32 / 2.0);
            if err.0.abs() < 30.0 && err.1.abs() < 30.0 && converged_at.is_none() {
                converged_at = Some(i);
            }
            let ch = law.update(tp, frame, dt);
            sim.step(&ch, dt);
        }
        let tp = sim.target_in_frame(frame, target);
        let err = (tp.0 - 320.0, tp.1 - 240.0);
        assert!(
            err.0.abs() < 30.0 && err.1.abs() < 30.0,
            "не сошлось: err={err:?}"
        );
        assert!(converged_at.is_some(), "ни разу не попало в окно ±30 px");
        // Не ушло в автоколебания большой амплитуды.
        assert!(err.0.abs() < 15.0 && err.1.abs() < 15.0, "финал дрожит: {err:?}");
    }

    /// Движущаяся цель (Лиссажу): медиана ошибки после захвата ≤ 30 px.
    #[test]
    fn closed_loop_tracks_moving_target() {
        let mut sim = PlatformSim::new(600.0, 0.25);
        let mut law = AimLaw::new(AimConfig {
            x: crate::law::AxisParams {
                kp: 2.0,
                kd: 0.15,
                slew_us: 40.0,
                ..Default::default()
            },
            y: crate::law::AxisParams {
                kp: 2.0,
                kd: 0.15,
                slew_us: 40.0,
                ..Default::default()
            },
            ..Default::default()
        });
        let frame = (640u32, 480u32);
        let dt = 1.0 / 30.0;
        let mut errs = Vec::new();
        for i in 0..600 {
            // 20 с
            let t = i as f32 * dt;
            let target = (150.0 * (0.5 * t).sin(), 100.0 * (0.8 * t).cos());
            let tp = sim.target_in_frame(frame, target);
            errs.push((
                tp.0 - 320.0,
                tp.1 - 240.0,
            ));
            let ch = law.update(tp, frame, dt);
            sim.step(&ch, dt);
        }
        // после 5 секунд
        let settled: Vec<f32> = errs[150..].iter().map(|e| e.0.abs().max(e.1.abs())).collect();
        let mut sorted = settled.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = sorted[sorted.len() / 2];
        assert!(median < 30.0, "медиана ошибки {median} px > 30");
    }

    #[test]
    fn noop_link_counts() {
        let mut l = NoopLink::new();
        l.send_rc(&[1500u16; 16]).unwrap();
        assert_eq!(l.sent.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
