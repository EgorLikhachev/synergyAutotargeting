//! FFI к librknnrt.so (Rockchip RKNN runtime) + безопасная обёртка.
//!
//! ## Происхождение логики
//! Это Rust-порт обкатанного на железе C++ `rknn-bridge` из Autotargeting
//! (rknn_model.cpp, SDK 2.3.0). Сохранены три критических приёма:
//! 1. Вход форсируется в UINT8/NHWC — NPU сам делает нормализацию и квантование
//!    (ADR D-007 из Autotargeting).
//! 2. Выход форсируется в FLOAT32 через `rknn_set_io_mem` — runtime конвертирует
//!    нативный fp16/int8 сам (zero-copy трюк из официального демо, ADR D-010).
//! 3. Привязка к NPU_CORE_0 — без неё rknn_run на драйвере 2.3.0 наблюдался
//!    segfault при AUTO-планировке.
//!
//! Заголовок `rknn_api.h` (SDK 2.x, airockchip/rknn-toolkit2) лежит рядом
//! в этом крейте — источник всех смещений структур.

pub mod ffi;

#[cfg(feature = "npu")]
pub mod safe;

#[cfg(feature = "npu")]
pub use safe::{RknnModel, RknnError};

/// Константы, общие для стаба и реального бэкенда.
pub mod consts {
    /// RKNN_MAX_DIMS
    pub const MAX_DIMS: usize = 16;
    /// RKNN_MAX_NAME_LEN
    pub const MAX_NAME_LEN: usize = 256;
}

/// Форма тензора, как её видит рантайм (после форсирования типов).
#[derive(Debug, Clone)]
pub struct TensorInfo {
    /// Имя тензора (может быть пустым).
    pub name: String,
    /// Число элементов.
    pub n_elems: usize,
    /// Длина в байтах с учётом stride.
    pub size_with_stride: usize,
    /// Шаг по ширине в байтах (0 == width).
    pub w_stride: usize,
    /// Длина в байтах без stride.
    pub size: usize,
}

#[cfg(test)]
mod tests {
    #[test]
    fn stub_builds() {
        // Без фичи npu крейт собирается на любой платформе — это и проверяем.
        assert_eq!(super::consts::MAX_DIMS, 16);
    }
}
