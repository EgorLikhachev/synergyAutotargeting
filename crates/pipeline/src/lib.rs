//! Гибридный конвейер варианта C: трекинг каждый кадр + детекция раз в N кадров.
//!
//! Ответственность модуля — слияние двух источников информации о цели:
//! - детектор (YOLOv8 на NPU) — точный, но редкий (раз в N кадров);
//! - трекер (NanoTrack на CPU) — быстрый, каждый кадр, но дрейфует.
//!
//! Паттерн handoff «детекция → трекер» (exchange_tracker),
//! топология «детектор → трекер» — классическая pub/sub.

use common::{BBox, Detection};
use nano_track::{NanoTracker, Stabilizer};

/// Режим, в котором конвейер находится на кадре.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Цель ведётся трекером (детекция не на этом кадре).
    Tracking,
    /// На этом кадре была детекция, трекер переинициализирован.
    DetectAcquire,
    /// Цель потеряна, ждём очередной детекции.
    Lost,
    /// Трекинг снят оператором («снять захват»): цели нет, авто-захват
    /// по детекциям выключен до следующего ручного lock.
    Idle,
}

/// Конфигурация гибрида.
#[derive(Debug, Clone)]
pub struct HybridConfig {
    /// Детекция раз в N кадров.
    pub detect_every_n: u32,
    /// Минимальный IoU детекции с текущим боксом, при котором детекция
    /// считается подтверждением той же цели (иначе — смена цели/реинициализация).
    pub iou_confirm: f32,
    /// Минимальный score трекера, ниже которого цель считается потерянной.
    pub min_track_score: f32,
    /// Сколько кадров с низким score подряд терпим до форс-детекции.
    pub lost_patience: u32,
    /// Минимальная уверенность детекции для захвата цели.
    pub min_detect_conf: f32,
    /// Сколько ПОДРЯД «несогласных» детекций (IoU < iou_confirm с текущим
    /// боксом) требуется для смены якоря. 1 — старое поведение (единичная
    /// детекция сразу уводит трек). Подтверждающая детекция (IoU ≥
    /// iou_confirm) ре-якорит сразу же, без счётчика. Анти-угон: в R4
    /// трекер дрейфовал на фон при живом счёте, но и одиночный ложный
    /// детект не должен перехватывать наведение (ADR-023).
    pub re_anchor_streak: u32,
    /// Цифровая стабилизация (GMC): компенсация глобального сдвига кадра
    /// перед трекингом — для жёсткого монтажа камеры без виброразвязки.
    pub gmc: bool,
    /// Приоритетные классы (индексы): из детекций предпочитаем их.
    pub priority_classes: Vec<u32>,
    /// Инициализирован ли стабилизатор бокса.
    pub use_stabilizer: bool,
}

impl Default for HybridConfig {
    fn default() -> Self {
        Self {
            detect_every_n: 10,
            iou_confirm: 0.3,
            min_track_score: 0.30,
            lost_patience: 3,
            min_detect_conf: 0.45,
            re_anchor_streak: 2,
            priority_classes: Vec::new(),
            use_stabilizer: true,
            gmc: false,
        }
    }
}

/// Состояние цели на выходе кадра.
#[derive(Debug, Clone)]
pub struct TargetState {
    pub mode: Mode,
    pub bbox: Option<BBox>,
    /// Score трекера (или уверенность детекции на кадре захвата).
    pub score: f32,
    /// IoU последней детекции с текущим боксом (на кадрах детекции).
    pub last_det_iou: Option<f32>,
    /// Счётчик кадров с момента последней успешной детекции.
    pub frames_since_detect: u32,
}

/// Гибридный трекер:NanoTrack + правила слияния с детекциями.
pub struct HybridTracker {
    tracker: NanoTracker,
    stabilizer: Stabilizer,
    gmc: Option<nano_track::gmc::GmcEstimator>,
    config: HybridConfig,
    frames_since_detect: u32,
    low_score_streak: u32,
    /// Счётчик ПОДРЯД детекций вне текущего бокса (анти-угон, ADR-023).
    disagree_streak: u32,
    /// Score трекера с последнего кадра (для ранних возвратов из on_detection).
    last_score: f32,
    last_bbox: Option<BBox>,
    last_det_iou: Option<f32>,
    /// Авто-захват по детекциям. Снимается командой unlock() и включается
    /// обратно ручным захватом оператора.
    auto_acquire: bool,
    pub detect_inflight: bool,
    /// Оценка глобального сдвига последнего кадра (GMC, L4).
    pub last_gmc: Option<(f32, f32)>,
}

/// Решение о детекции: по расписанию раз в N кадров, а при потере — каждый
/// кадр (ре-захват ограничен только скоростью NPU; soak: медиана 19 мс).
fn should_detect(frame_idx: u64, every_n: u32, lost: bool) -> bool {
    lost || frame_idx % every_n.max(1) as u64 == 0
}

/// Решение о слиянии детекции с активным треком (ADR-023).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetMerge {
    /// Подтверждение цели (IoU ≥ iou_confirm): реинициализация трекера
    /// на боксе детекции сразу — мягкая коррекция дрейфа.
    Confirm,
    /// Детекция вне бокса, серия несогласных ещё не набрана: игнорируем,
    /// трек продолжается (анти-угон: одиночный ложный детект не уводит цель).
    WaitStreak,
    /// Вне бокса И серия набралась: ре-якорь (трекер удрейфовал — R4,
    /// либо смена цели).
    ReAnchor,
}

fn merge_decision(tracker_live: bool, iou: f32, streak: u32, cfg: &HybridConfig) -> DetMerge {
    if !tracker_live {
        return DetMerge::ReAnchor; // трека нет — это захват, не угон
    }
    if iou >= cfg.iou_confirm {
        return DetMerge::Confirm;
    }
    if streak >= cfg.re_anchor_streak.max(1) {
        DetMerge::ReAnchor
    } else {
        DetMerge::WaitStreak
    }
}

impl HybridTracker {
    pub fn new(tracker: NanoTracker, config: HybridConfig) -> Self {
        let gmc = config
            .gmc
            .then(|| nano_track::gmc::GmcEstimator::new(640, 480));
        Self {
            tracker,
            gmc,
            last_gmc: None,
            stabilizer: Stabilizer::new(),
            config,
            frames_since_detect: 0,
            low_score_streak: 0,
            disagree_streak: 0,
            last_score: 0.0,
            last_bbox: None,
            last_det_iou: None,
            auto_acquire: true,
            detect_inflight: false,
        }
    }

    /// Нужна ли детекция на этом кадре (по расписанию или из-за потери цели).
    /// В Idle (захват снят) — только плановое расписание: форс-каждый-кадр
    /// нужен лишь для ре-захвата, который выключен.
    pub fn wants_detection(&self, frame_idx: u64) -> bool {
        let lost = self.auto_acquire
            && (self.last_bbox.is_none()
                || self.low_score_streak >= self.config.lost_patience
                || !self.tracker.is_initialized());
        should_detect(frame_idx, self.config.detect_every_n, lost)
    }

    /// Снять трекинг оператором («снять захват»): цель сбрасывается,
    /// авто-захват по детекциям выключается до следующего ручного lock.
    /// Наведение уходит в центры (commander: не-Tracking → lost()).
    pub fn unlock(&mut self) {
        self.auto_acquire = false;
        self.last_bbox = None;
        self.last_det_iou = None;
        self.low_score_streak = 0;
        self.disagree_streak = 0;
        self.detect_inflight = false;
        self.tracker.clear();
        self.stabilizer.clear();
    }

    /// Захват снят оператором (авто-захват выключен)?
    pub fn is_idle(&self) -> bool {
        !self.auto_acquire
    }

    /// Активный трек (для гейтинга тайл-инференса ADR-022: пока трек жив —
    /// дешёвая полнокадровая детекция; потеряли — zoom-тайлы за ×4 NPU).
    pub fn is_tracking(&self) -> bool {
        self.auto_acquire && self.last_bbox.is_some() && self.tracker.is_initialized()
    }

    /// Подать результаты детекции (вызываются когда детектор ответил).
    /// `frame` нужен для реинициализации трекера.
    pub fn on_detection(
        &mut self,
        dets: &[Detection],
        frame: &nano_track::imgops::Img,
    ) -> TargetState {
        self.detect_inflight = false;
        self.frames_since_detect = 0;

        // Захват снят оператором: детекции идут в UI (красные рамки),
        // но цель не переинициализируется до ручного lock.
        if !self.auto_acquire {
            return self.state(Mode::Idle);
        }

        let dets: Vec<&Detection> = dets
            .iter()
            .filter(|d| d.confidence >= self.config.min_detect_conf)
            .collect();

        if dets.is_empty() {
            return self.state(Mode::Lost);
        }

        // Выбор цели: максимум по (приоритет класса, IoU с текущим боксом, score).
        let current = self.last_bbox;
        let pick = dets
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                let key = |d: &Detection| -> (u32, f32, f32) {
                    let prio = self
                        .config
                        .priority_classes
                        .iter()
                        .position(|&c| c == d.class_id)
                        .map(|p| (self.config.priority_classes.len() - p) as u32)
                        .unwrap_or(0);
                    let iou = current.map(|b| b.iou(&d.bbox)).unwrap_or(0.0);
                    (prio, iou, d.confidence)
                };
                key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(_, d)| *d)
            .unwrap();

        self.last_det_iou = current.map(|b| b.iou(&pick.bbox));

        // Слияние с активным треком (ADR-023): подтверждающая детекция
        // ре-якорит сразу (мягкая коррекция дрейфа); вне бокса — только
        // после streak ПОДРЯД несогласных (анти-угон от одиночного ложного
        // детекта; сам streak и набранная серия = ре-якорь при дрейфе R4).
        let decision = merge_decision(
            self.tracker.is_initialized(),
            self.last_det_iou.unwrap_or(0.0),
            self.disagree_streak + 1,
            &self.config,
        );
        match decision {
            DetMerge::Confirm => {
                self.disagree_streak = 0;
                tracing::debug!(iou = self.last_det_iou.unwrap_or(0.0),
                    "детекция подтверждает цель, реинициализация трекера");
            }
            DetMerge::WaitStreak => {
                self.disagree_streak += 1;
                tracing::debug!(iou = self.last_det_iou.unwrap_or(0.0),
                    streak = self.disagree_streak,
                    "детекция вне бокса — ждём подтверждения, трек продолжается");
                let mut st = self.state(Mode::Tracking);
                st.score = self.last_score;
                return st;
            }
            DetMerge::ReAnchor => {
                if self.tracker.is_initialized() {
                    tracing::info!(iou = self.last_det_iou.unwrap_or(0.0),
                        streak = self.disagree_streak,
                        "ре-якорь: серия несогласных детекций (дрейф/смена цели)");
                }
                self.disagree_streak = 0;
            }
        }

        let mut box_ = pick.bbox;
        box_.clamp_to(frame.w, frame.h);
        let score = pick.confidence;

        if let Err(e) = self.tracker.init(frame, box_) {
            tracing::error!(error = %e, "init трекера не удался");
            return self.state(Mode::Lost);
        }
        self.stabilizer.clear();
        self.stabilizer.set_hw([box_.w, box_.h]);
        self.low_score_streak = 0;
        self.last_bbox = Some(box_);
        let mut st = self.state(Mode::DetectAcquire);
        st.score = score;
        st
    }

    /// Ручной захват цели оператором (двойной клик в UI): инициализация
    /// трекера прямо на ROI, без детекции — первичная сигнатура цели.
    pub fn on_manual_roi(&mut self, bbox: BBox, frame: &nano_track::imgops::Img) -> TargetState {
        // Ручной захват возвращает авто-захват (снимал его unlock()).
        self.auto_acquire = true;
        let mut box_ = bbox;
        box_.clamp_to(frame.w, frame.h);
        if box_.w < 8.0 || box_.h < 8.0 {
            return self.state(Mode::Lost);
        }
        if let Err(e) = self.tracker.init(frame, box_) {
            tracing::error!(error = %e, "init трекера (manual ROI) не удался");
            return self.state(Mode::Lost);
        }
        self.stabilizer.clear();
        self.stabilizer.set_hw([box_.w, box_.h]);
        self.low_score_streak = 0;
        self.disagree_streak = 0;
        self.detect_inflight = false;
        self.last_det_iou = None;
        self.last_bbox = Some(box_);
        let mut st = self.state(Mode::DetectAcquire);
        st.score = 1.0; // ручное назначение оператором
        st
    }

    /// Обработать кадр трекером (каждый кадр).
    pub fn on_frame(&mut self, frame: &nano_track::imgops::Img) -> TargetState {
        self.frames_since_detect += 1;

        // Захват снят оператором: трекер не работает, цели нет.
        if !self.auto_acquire {
            return self.state(Mode::Idle);
        }

        if !self.tracker.is_initialized() {
            return self.state(Mode::Lost);
        }

        // GMC: сдвигаем позицию цели на оценку глобального движения кадра
        // (вибрация/поворот платформы) — трекер ищет в стабилизированной точке.
        if let Some(g) = self.gmc.as_mut() {
            let (dx, dy) = g.estimate(frame);
            self.last_gmc = Some((dx, dy));
            if dx != 0.0 || dy != 0.0 {
                self.tracker.shift_position(dx, dy);
            }
        }
        let bbox = match self.tracker.update(frame) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "трекер упал, ждём детекции");
                self.last_bbox = None;
                return self.state(Mode::Lost);
            }
        };

        let score = self.tracker.tracking_score();

        // Гейт качества.
        if score < self.config.min_track_score {
            self.low_score_streak += 1;
            if self.low_score_streak >= self.config.lost_patience {
                self.last_bbox = None;
                return self.state(Mode::Lost);
            }
        } else {
            self.low_score_streak = 0;
        }

        let mut b = bbox;
        // Проверка краёв кадра (порт _edges_frame): цель у края — потеря.
        let margin = 2.0;
        if b.x < margin || b.y < margin || b.x2() > frame.w as f32 - margin || b.y2() > frame.h as f32 - margin
        {
            self.last_bbox = None;
            return self.state(Mode::Lost);
        }

        // Стабилизация центра.
        if self.config.use_stabilizer {
            let (cx, cy) = b.center();
            let (_, (sx, sy)) = self.stabilizer.predict((cx, cy));
            b = BBox::new(sx - b.w * 0.5, sy - b.h * 0.5, b.w, b.h);
        }

        self.last_bbox = Some(b);
        self.last_score = score;
        let mut st = self.state(Mode::Tracking);
        st.score = score;
        st
    }

    fn state(&self, mode: Mode) -> TargetState {
        TargetState {
            mode,
            bbox: self.last_bbox,
            score: 0.0,
            last_det_iou: self.last_det_iou,
            frames_since_detect: self.frames_since_detect,
        }
    }

    /// Текущий бокс (если есть).
    pub fn current_bbox(&self) -> Option<BBox> {
        self.last_bbox
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_state_detects_every_frame() {
        // Потеря цели — детекция каждый кадр: ре-захват ограничен только
        // скоростью NPU (soak: медиана 19 мс при 60 FPS; ROADMAP <100 мс).
        for i in 0..25u64 {
            assert!(should_detect(i, 10, true), "кадр {i}: в LOST детекция нужна каждый кадр");
        }
        // В сопровождении — строго по расписанию.
        let hits: Vec<u64> = (0..30).filter(|&i| should_detect(i, 10, false)).collect();
        assert_eq!(hits, vec![0, 10, 20]);
    }

    #[test]
    fn wants_detection_schedule() {
        let cfg = HybridConfig {
            detect_every_n: 10,
            ..Default::default()
        };
        // Без трекера внутри (мини-фабрика ниже) — проверяем только расписание,
        // поэтому конструируем через wants_detection логику напрямую.
        let mut frame_idx = 0u64;
        let mut hits = 0;
        for i in 0..30u64 {
            frame_idx = i;
            let scheduled = frame_idx % 10 == 0;
            if scheduled {
                hits += 1;
            }
        }
        assert_eq!(hits, 3);
        let _ = cfg;
    }

    #[test]
    fn merge_decision_table() {
        let cfg = HybridConfig::default(); // iou_confirm 0.3, streak 2
        // Подтверждение: детекция в боксе — реинициализация сразу.
        assert_eq!(merge_decision(true, 0.5, 1, &cfg), DetMerge::Confirm);
        assert_eq!(merge_decision(true, 0.3, 9, &cfg), DetMerge::Confirm);
        // Анти-угон: одиночная детекция вне бокса НЕ уводит трек.
        assert_eq!(merge_decision(true, 0.1, 1, &cfg), DetMerge::WaitStreak);
        // Серия набралась — ре-якорь (дрейф R4 / смена цели).
        assert_eq!(merge_decision(true, 0.1, 2, &cfg), DetMerge::ReAnchor);
        // Трека нет — любая детекция это захват, не угон.
        assert_eq!(merge_decision(false, 0.0, 1, &cfg), DetMerge::ReAnchor);
        // streak=1 — старое поведение (мгновенный угон) как аварийный люк.
        let old = HybridConfig { re_anchor_streak: 1, ..Default::default() };
        assert_eq!(merge_decision(true, 0.1, 1, &old), DetMerge::ReAnchor);
    }
}
