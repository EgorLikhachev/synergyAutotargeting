//! Закон наведения: ошибка в пикселях → RC-каналы (мкс).
//! Порт сути bkb `utils/shim.py` (фаза D, ADR-012): нормировка ошибки,
//! мёртвая зона, P/PID-усиление, ограничение slew, свап/реверс осей
//! (камера может стоять повёрнутой на 90°, как в bkb).

/// Параметры одного канала (ось X или Y).
#[derive(Debug, Clone, Copy)]
pub struct AxisParams {
    /// Пропорциональный коэффициент: ошибка 1.0 (вся полуось кадра) → kp × pwm_range.
    pub kp: f32,
    /// Интегральный коэффициент (сек·ошибка → мкс).
    pub ki: f32,
    /// Дифференциальный коэффициент (ошибка/сек → мкс).
    pub kd: f32,
    /// Мёртвая зона, px (внутри — центр 1500).
    pub deadband_px: f32,
    /// Максимальный шаг выхода за тик, мкс (slew-лимит bkb one_step).
    pub slew_us: f32,
    /// Инвертировать знак (реверс серво).
    pub reverse: bool,
}

impl Default for AxisParams {
    fn default() -> Self {
        // Стартовые значения из bkb: gain 2.0, мёртвая зона ~1% кадра,
        // шаг 3..8 мкс на тик 30 Гц.
        Self {
            kp: 2.0,
            ki: 0.0,
            kd: 0.0,
            deadband_px: 6.0,
            slew_us: 8.0,
            reverse: false,
        }
    }
}

/// Регулятор одной оси: PID с анти-виндапом и slew-лимитом.
pub struct Axis {
    pub p: AxisParams,
    int: f32,
    prev_err: Option<f32>,
    last_out: f32,
    pwm_range: f32,
}

impl Axis {
    pub fn new(p: AxisParams) -> Self {
        Self {
            p,
            int: 0.0,
            prev_err: None,
            last_out: 1500.0,
            pwm_range: 500.0, // 1000..2000 мкс
        }
    }

    /// Ошибка в пикселях (цель − центр кадра) → мкс.
    pub fn update(&mut self, err_px: f32, half_frame: f32, dt: f32) -> u16 {
        let half = half_frame.max(1.0);
        let mut err = if err_px.abs() < self.p.deadband_px {
            0.0
        } else {
            err_px / half // нормировка: ±1.0 на краях кадра
        };
        if self.p.reverse {
            err = -err;
        }

        // PID с ограничением интеграла (анти-виндап: ±20% диапазона).
        let d = match self.prev_err.replace(err) {
            Some(prev) => (err - prev) / dt.max(1e-3),
            None => 0.0,
        };
        self.int = (self.int + err * dt).clamp(-0.2, 0.2);
        let out = 1500.0 + (err * self.p.kp + self.int * self.p.ki + d * self.p.kd)
            * self.pwm_range;

        // Slew-лимит и общий клэмп.
        let target = out.clamp(1500.0 - self.pwm_range, 1500.0 + self.pwm_range);
        let step = (target - self.last_out).clamp(-self.p.slew_us, self.p.slew_us);
        self.last_out += step;
        self.last_out.round().clamp(1000.0, 2000.0) as u16
    }

    /// Сброс накопленного состояния (потеря цели).
    pub fn reset(&mut self) {
        self.int = 0.0;
        self.prev_err = None;
        self.last_out = 1500.0;
    }
}

/// Предиктор упреждения (ROADMAP-D). Скорость цели оценивается в МИРЕ:
/// кадр движется вместе с платформой, поэтому мировая скорость цели =
/// скорость в кадре + скорость платформы (фидфорвард из нашей же RC-команды
/// × масштаб стика). Точка прицеливания выносится вперёд на lead_s секунд.
pub struct LeadPredictor {
    pos: Option<(f32, f32)>,
    /// Оценка мировой скорости цели, px/с.
    vel: (f32, f32),
    /// Сглаживание оценки скорости (0..1): меньше — спокойнее.
    pub alpha: f32,
    /// Горизонт упреждения, с.
    pub lead_s: f32,
}

impl LeadPredictor {
    pub fn new(alpha: f32, lead_s: f32) -> Self {
        Self { pos: None, vel: (0.0, 0.0), alpha: alpha.clamp(0.01, 1.0), lead_s }
    }

    /// Наблюдение центра цели в кадре + оценка скорости платформы (px/с).
    /// Возвращает точку прицеливания с упреждением.
    pub fn observe(&mut self, p: (f32, f32), dt: f32, platform_vel: (f32, f32)) -> (f32, f32) {
        if let Some(prev) = self.pos.replace(p) {
            let dt = dt.max(1e-3);
            // мировая скорость = относительная (в кадре) + платформа
            let inst = (
                (p.0 - prev.0) / dt + platform_vel.0,
                (p.1 - prev.1) / dt + platform_vel.1,
            );
            self.vel.0 += self.alpha * (inst.0 - self.vel.0);
            self.vel.1 += self.alpha * (inst.1 - self.vel.1);
        }
        (p.0 + self.vel.0 * self.lead_s, p.1 + self.vel.1 * self.lead_s)
    }

    pub fn reset(&mut self) {
        self.pos = None;
        self.vel = (0.0, 0.0);
    }
}

/// Конфиг маппинга осей на RC-каналы (порты из bkb: roll→ch0, pitch→ch1,
/// yaw→ch2, throttle→ch3, aux1=ch4 — ARM).
#[derive(Debug, Clone, Copy)]
pub struct AimConfig {
    /// Ось «вправо-влево по кадру» (пиксельный X).
    pub x: AxisParams,
    /// Ось «вверх-вниз по кадру» (пиксельный Y).
    pub y: AxisParams,
    /// Свап осей (камера повёрнута на 90°, как в bkb).
    pub swap_axes: bool,
    /// Постоянные каналы: throttle (ch3) и aux1 (ch4, ARM-уровень).
    pub throttle_us: u16,
    pub aux1_us: u16,
    /// Упреждение: горизонт, с (0 — выкл) и сглаживание скорости.
    pub lead_s: f32,
    pub lead_alpha: f32,
    /// Скорость платформы при полном стике, px/с (для фидфорварда упреждения).
    pub stick_rate_px_s: f32,
}

impl Default for AimConfig {
    fn default() -> Self {
        Self {
            x: AxisParams::default(),
            y: AxisParams::default(),
            swap_axes: false,
            throttle_us: 1310,
            aux1_us: 1950,
            lead_s: 0.0,
            lead_alpha: 0.25,
            stick_rate_px_s: 600.0,
        }
    }
}

use crate::msp;

/// Закон наведения целиком: кадр → 16 RC-каналов.
pub struct AimLaw {
    cfg: AimConfig,
    ax: Axis,
    ay: Axis,
    lead: LeadPredictor,
    /// Прошлая RC-команда — оценка скорости платформы для упреждения.
    last_ch: [u16; 16],
}

impl AimLaw {
    pub fn new(cfg: AimConfig) -> Self {
        let lead = LeadPredictor::new(cfg.lead_alpha, cfg.lead_s);
        let (ax, ay) = (Axis::new(cfg.x), Axis::new(cfg.y));
        Self { cfg, ax, ay, lead, last_ch: msp::center_channels() }
    }

    /// `target_px` — (x, y) центра цели в пикселях; `frame` — (w, h).
    /// Возврат: 16 каналов RC (мкс).
    pub fn update(&mut self, target_px: (f32, f32), frame: (u32, u32), dt: f32) -> [u16; 16] {
        let (cx, cy) = (frame.0 as f32 / 2.0, frame.1 as f32 / 2.0);
        // Скорость платформы из прошлой команды (rate-режим: стик ≈ скорость).
        let scale = self.cfg.stick_rate_px_s / 500.0;
        let platform_vel = (
            (self.last_ch[0] as f32 - 1500.0) * scale,
            (self.last_ch[1] as f32 - 1500.0) * scale,
        );
        // Точка прицеливания: цель с упреждением по мировой скорости.
        let aim = self.lead.observe(target_px, dt, platform_vel);
        let err = (
            aim.0 - cx, // вправо — положительно
            aim.1 - cy, // вниз — положительно
        );
        let ch_x = self.ax.update(err.0, cx, dt);
        let ch_y = self.ay.update(err.1, cy, dt);

        let mut ch = msp::center_channels();
        if self.cfg.swap_axes {
            ch[0] = ch_y; // roll ← вертикаль кадра (камера 90°)
            ch[1] = ch_x;
        } else {
            ch[0] = ch_x;
            ch[1] = ch_y;
        }
        ch[3] = self.cfg.throttle_us;
        ch[4] = self.cfg.aux1_us;
        self.last_ch = ch;
        ch
    }

    /// Потеря цели: стики в центр, состояние сброшено.
    pub fn lost(&mut self) -> [u16; 16] {
        self.ax.reset();
        self.ay.reset();
        self.lead.reset();
        let mut ch = msp::center_channels();
        ch[3] = self.cfg.throttle_us;
        ch[4] = self.cfg.aux1_us;
        ch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ax(p: AxisParams) -> Axis {
        Axis::new(p)
    }

    #[test]
    fn deadband_gives_center() {
        let mut a = ax(AxisParams {
            deadband_px: 10.0,
            ..Default::default()
        });
        assert_eq!(a.update(5.0, 320.0, 0.033), 1500);
    }

    #[test]
    fn proportional_sign_and_clamp() {
        let mut a = ax(AxisParams {
            kp: 2.0,
            slew_us: 500.0, // без slew-ограничения для теста
            ..Default::default()
        });
        // Полная ошибка вправо → 1500 + 1.0*2*500 = 2500 → клэмп 2000.
        assert_eq!(a.update(320.0, 320.0, 0.033), 2000);
        // Реверс → в другую сторону.
        let mut r = ax(AxisParams {
            kp: 2.0,
            slew_us: 500.0,
            reverse: true,
            ..Default::default()
        });
        assert_eq!(r.update(320.0, 320.0, 0.033), 1000);
    }

    #[test]
    fn slew_limits_step() {
        let mut a = ax(AxisParams {
            kp: 2.0,
            slew_us: 8.0,
            ..Default::default()
        });
        let out = a.update(320.0, 320.0, 0.033); // огромная ошибка
        assert_eq!(out, 1508, "шаг должен быть ограничен 8 мкс");
        let out2 = a.update(320.0, 320.0, 0.033);
        assert_eq!(out2, 1516);
    }

    #[test]
    fn law_channels_mapping() {
        let mut law = AimLaw::new(AimConfig {
            throttle_us: 1310,
            aux1_us: 1950,
            ..Default::default()
        });
        let ch = law.lost();
        assert_eq!(ch[0], 1500);
        assert_eq!(ch[3], 1310);
        assert_eq!(ch[4], 1950);
    }
}
