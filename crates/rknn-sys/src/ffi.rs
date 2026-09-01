//! Сырые C-объявления rknn_api.h (SDK 2.x). Только то, что реально используется.
//!
//! Смещения проверены по официальному заголовку airockchip/rknn-toolkit2
//! (rknpu2/runtime/Linux/librknn_api/include/rknn_api.h).

#![allow(non_camel_case_types, dead_code)]

use std::ffi::c_void;

pub const RKNN_MAX_DIMS: usize = 16;
pub const RKNN_MAX_NAME_LEN: usize = 256;

/// typedef int32_t rknn_context (в SDK — знаковый int; C++-код инициализировал -1).
pub type rknn_context = i32;

// === query commands ===
pub const RKNN_QUERY_IN_OUT_NUM: i32 = 0;
pub const RKNN_QUERY_INPUT_ATTR: i32 = 1;
pub const RKNN_QUERY_OUTPUT_ATTR: i32 = 2;
pub const RKNN_QUERY_SDK_VERSION: i32 = 5;
pub const RKNN_QUERY_INPUT_DYNAMIC_RANGE: i32 = 13;
pub const RKNN_QUERY_CURRENT_INPUT_ATTR: i32 = 14;
pub const RKNN_QUERY_CURRENT_OUTPUT_ATTR: i32 = 15;

// === tensor types (rknn_tensor_type) ===
pub const RKNN_TENSOR_FLOAT32: u32 = 0;
pub const RKNN_TENSOR_FLOAT16: u32 = 1;
pub const RKNN_TENSOR_INT8: u32 = 2;
pub const RKNN_TENSOR_UINT8: u32 = 3;

// === tensor formats (rknn_tensor_format) ===
pub const RKNN_TENSOR_NCHW: u32 = 0;
pub const RKNN_TENSOR_NHWC: u32 = 1;

// === core masks (rknn_core_mask) ===
pub const RKNN_NPU_CORE_0: u32 = 1;

/// rknn_sdk_version
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct rknn_sdk_version {
    pub api_version: [std::ffi::c_char; 256],
    pub drv_version: [std::ffi::c_char; 256],
}

/// rknn_input_output_num
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct rknn_input_output_num {
    pub n_input: u32,
    pub n_output: u32,
}

/// rknn_tensor_attr — выравнивание повторяет C-структуру (все поля <= 4 байт,
/// итоговый размер 376 на aarch64).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct rknn_tensor_attr {
    pub index: u32,
    pub n_dims: u32,
    pub dims: [u32; RKNN_MAX_DIMS],
    pub name: [std::ffi::c_char; RKNN_MAX_NAME_LEN],
    pub n_elems: u32,
    pub size: u32,
    pub fmt: u32,
    pub type_: u32,
    pub qnt_type: u32,
    pub fl: i8,
    pub zp: i32,
    pub scale: f32,
    pub w_stride: u32,
    pub size_with_stride: u32,
    pub pass_through: u8,
    pub h_stride: u32,
}

impl Default for rknn_tensor_attr {
    fn default() -> Self {
        // zeroed() корректен: все поля POD.
        unsafe { std::mem::zeroed() }
    }
}

/// rknn_tensor_mem
#[repr(C)]
pub struct rknn_tensor_mem {
    pub virt_addr: *mut c_void,
    pub phys_addr: u64,
    pub fd: i32,
    pub offset: i32,
    pub size: u32,
    pub flags: u32,
    pub priv_data: *mut c_void,
}

extern "C" {
    pub fn rknn_init(
        context: *mut rknn_context,
        model: *mut c_void,
        size: u32,
        flag: u32,
        extend: *mut c_void,
    ) -> i32;
    pub fn rknn_destroy(context: rknn_context) -> i32;
    pub fn rknn_query(
        context: rknn_context,
        cmd: i32,
        info: *mut c_void,
        size: u32,
    ) -> i32;
    pub fn rknn_set_core_mask(context: rknn_context, core_mask: u32) -> i32;
    pub fn rknn_create_mem(ctx: rknn_context, size: u32) -> *mut rknn_tensor_mem;
    pub fn rknn_destroy_mem(ctx: rknn_context, mem: *mut rknn_tensor_mem) -> i32;
    pub fn rknn_set_io_mem(
        ctx: rknn_context,
        mem: *mut rknn_tensor_mem,
        attr: *mut rknn_tensor_attr,
    ) -> i32;
    pub fn rknn_run(ctx: rknn_context, extend: *mut c_void) -> i32;
    pub fn rknn_set_input_shapes(
        ctx: rknn_context,
        n_inputs: u32,
        attrs: *mut rknn_tensor_attr,
    ) -> i32;
}
