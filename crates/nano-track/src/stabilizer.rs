//! Стабилизатор бокса — порт filter.Stabilizer из bkb (test_nano_cpu/filter.py).
//! Метод: скользящее среднее с гейтом допустимого отклонения.

#[derive(Debug, Clone)]
pub struct Stabilizer {
    /// Допустимое отклонение по осям (используется размер бокса).
    hw: [f32; 2],
    old_point: Option<(f32, f32)>,
}

impl Default for Stabilizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Stabilizer {
    pub fn new() -> Self {
        Self {
            hw: [30.0, 50.0],
            old_point: None,
        }
    }

    /// Сброс (при реинициализации трекера).
    pub fn clear(&mut self) {
        self.old_point = None;
    }

    /// Задать допуск по осям.
    pub fn set_hw(&mut self, hw: [f32; 2]) {
        self.hw = hw;
    }

    /// Усреднить позицию. Возвращает (применена_фильтрация, точка).
    pub fn predict(&mut self, point: (f32, f32)) -> (bool, (f32, f32)) {
        let Some(old) = self.old_point else {
            self.old_point = Some(point);
            return (false, point);
        };
        let within = old.0 + self.hw[0] > point.0
            && old.0 - self.hw[0] < point.0
            && old.1 + self.hw[1] > point.1
            && old.1 - self.hw[1] < point.1;
        if within {
            let smoothed = ((point.0 + old.0) * 0.5, (point.1 + old.1) * 0.5);
            self.old_point = Some(point);
            (true, smoothed)
        } else {
            (false, point)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_point_passes_through() {
        let mut s = Stabilizer::new();
        let (applied, p) = s.predict((10.0, 20.0));
        assert!(!applied);
        assert_eq!(p, (10.0, 20.0));
    }

    #[test]
    fn smooths_small_jumps() {
        let mut s = Stabilizer::new();
        s.predict((100.0, 100.0));
        let (applied, p) = s.predict((110.0, 100.0));
        assert!(applied);
        assert_eq!(p, (105.0, 100.0));
    }

    #[test]
    fn big_jump_passes_through() {
        let mut s = Stabilizer::new();
        s.predict((0.0, 0.0));
        let (applied, p) = s.predict((500.0, 500.0));
        assert!(!applied);
        assert_eq!(p, (500.0, 500.0));
    }
}
