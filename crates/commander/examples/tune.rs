//! Предтюнинг закона наведения на симуляторе (задача 2026-09-07):
//! свип kp/kd/lead_s по трём сценариям (ступень, движущаяся цель, инерция
//! тяжёлой платформы) — выбор стартовых коэффициентов до подключения FC.
//!
//!   cargo run -p commander --example tune
use commander::law::{AimConfig, AimLaw};
use commander::sim::PlatformSim;

struct Metrics {
    converge_ms: Option<f32>,
    median_px: f32,
    p90_px: f32,
    overshoot_px: f32,
}

fn run(cfg: AimConfig, tau: f32, scenario: u8) -> Metrics {
    let mut sim = PlatformSim::new(500.0, tau);
    let mut law = AimLaw::new(cfg);
    let frame = (640u32, 480u32);
    let dt = 1.0 / 30.0;
    let mut errs: Vec<f32> = Vec::new();
    let mut converged_at: Option<f32> = None;
    let mut peak_after_converge = 0.0f32;
    for i in 0..900 {
        let t = i as f32 * dt;
        // 1 — ступень 200/140 px; 2/3 — Лиссажу (масштаб 2 — «тяжёлая» цель)
        let target = match scenario {
            1 => (200.0f32, 140.0f32),
            _ => (170.0 * (0.6 * t).sin(), 110.0 * (0.9 * t).cos()),
        };
        let tp = sim.target_in_frame(frame, target);
        let e = (tp.0 - 320.0, tp.1 - 240.0);
        let m = e.0.abs().max(e.1.abs());
        if converged_at.is_none() && m < 30.0 {
            converged_at = Some(t * 1000.0);
        }
        if converged_at.is_some() {
            peak_after_converge = peak_after_converge.max(m);
        }
        errs.push(m);
        let ch = law.update(tp, frame, dt);
        sim.step(&ch, dt);
    }
    let settled = &errs[300..];
    let mut s = settled.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Metrics {
        converge_ms: converged_at,
        median_px: s[s.len() / 2],
        p90_px: s[s.len() * 9 / 10],
        overshoot_px: peak_after_converge,
    }
}

fn axis(kp: f32, kd: f32, slew: f32) -> commander::law::AxisParams {
    commander::law::AxisParams { kp, kd, slew_us: slew, ..Default::default() }
}

fn main() {
    println!("scenario | tau  | kp  | kd   | lead | converge | median | p90  | overshoot");
    println!("---------|------|-----|------|------|----------|--------|------|----------");
    for &(tau, sc, scname) in &[(0.25f32, 1u8, "step"), (0.25, 2, "move"), (0.45, 2, "move+")] {
        for kp in [1.2f32, 1.8, 2.4, 3.0] {
            for &(kd, lead) in &[(0.0f32, 0.0), (0.12, 0.0), (0.12, 0.15), (0.2, 0.2)] {
                let cfg = AimConfig {
                    x: axis(kp, kd, 40.0),
                    y: axis(kp, kd, 40.0),
                    lead_s: lead,
                    stick_rate_px_s: 500.0,
                    ..Default::default()
                };
                let m = run(cfg, tau, sc);
                println!(
                    "{scname:8} | {tau:.2} | {kp:.1} | {kd:.2} | {lead:.2} | {:>6.0}мс | {:>5.1} | {:>5.1} | {:>5.1}",
                    m.converge_ms.unwrap_or(99999.0),
                    m.median_px,
                    m.p90_px,
                    m.overshoot_px
                );
            }
        }
    }
}
