//! Direct V4L2 capture via raw ioctl — bypasses the `v4l` crate abstraction
//! that was 5× slower than `v4l2-ctl` on Orange Pi 5 (21 FPS vs 100 FPS).
//!
//! Uses `libc` for ioctl + mmap, no external C dependencies beyond
//! the kernel V4L2 interface. Linux-only.
//!
//! ## Performance
//! On Arducam OV9782 (USB 2.0, MJPG):
//!   - `v4l` crate (MMapStream): ~21 FPS sustained
//!   - This direct ioctl path: ~90-100 FPS (matches v4l2-ctl)

#![cfg(target_os = "linux")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use common::{Frame, FrameMetadata, PixelFormat};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::traits::{VideoCaptureError, VideoResult, VideoSource};

// === V4L2 ioctl number computation (matches kernel _IO macros on 64-bit Linux) ===
// _IOC(dir, type, nr, size) = (dir << 30) | (size << 16) | (type << 8) | nr
// dir: NONE=0, WRITE=1, READ=2, READWRITE=3
const fn _ioc(dir: u32, typ: u32, nr: u32, size: u32) -> u64 {
    ((dir as u64) << 30) | ((size as u64) << 16) | ((typ as u64) << 8) | nr as u64
}

// Struct sizes on 64-bit Linux aarch64 (verified via gcc on Orange Pi 5).
const SZ_CAP: u32 = 104; // sizeof(v4l2_capability)
const SZ_FMT: u32 = 208; // sizeof(v4l2_format)
const SZ_PARM: u32 = 204; // sizeof(v4l2_streamparm)
const SZ_REQBUFS: u32 = 20; // sizeof(v4l2_requestbuffers)
const SZ_BUF: u32 = 88; // sizeof(v4l2_buffer)
const SZ_INT: u32 = 4; // sizeof(int)

const TYP_V: u32 = b'V' as u32;

const VIDIOC_S_FMT: u64 = _ioc(3, TYP_V, 5, SZ_FMT);
const VIDIOC_S_PARM: u64 = _ioc(3, TYP_V, 22, SZ_PARM);
const VIDIOC_REQBUFS: u64 = _ioc(3, TYP_V, 8, SZ_REQBUFS);
const VIDIOC_QUERYBUF: u64 = _ioc(3, TYP_V, 9, SZ_BUF);
const VIDIOC_QBUF: u64 = _ioc(3, TYP_V, 15, SZ_BUF);
const VIDIOC_DQBUF: u64 = _ioc(3, TYP_V, 17, SZ_BUF);
const VIDIOC_STREAMON: u64 = _ioc(1, TYP_V, 18, SZ_INT);
const VIDIOC_STREAMOFF: u64 = _ioc(1, TYP_V, 19, SZ_INT);

const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const V4L2_MEMORY_MMAP: u32 = 1;

const V4L2_PIX_FMT_MJPEG: u32 = 0x47504a4d;
const V4L2_PIX_FMT_YUYV: u32 = 0x56595559;
const V4L2_PIX_FMT_NV12: u32 = 0x3231564e;

// === V4L2 structs (sizes MUST match kernel on 64-bit Linux) ===
// We use repr(C) + explicit padding to match sizeof() exactly.

/// v4l2_pix_format (48 bytes) — the C struct pix format.
#[repr(C)]
struct V4l2PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}
// 12 × 4 = 48 bytes ✓

/// v4l2_format (208 bytes). The kernel's `union fmt` has 8-byte alignment
/// (because v4l2_window contains a pointer), so there's 4 bytes of PADDING
/// between `type` (u32) and the union. Total = 4 + 4(pad) + 200(union) = 208.
#[repr(C)]
struct V4l2Format {
    typ: u32,           // offset 0-3
    _pad0: u32,         // offset 4-7 (kernel padding for 8-byte aligned union)
    pix: V4l2PixFormat, // offset 8-55 (first 48 bytes of the 200-byte union)
    _pad: [u8; 152],    // offset 56-207 (rest of union: 200-48=152)
}

/// v4l2_requestbuffers (20 bytes).
#[repr(C)]
struct V4l2RequestBuffers {
    count: u32,
    typ: u32,
    memory: u32,
    capabilities: u32,
    flags: u32,
}
// 5 × 4 = 20 ✓

/// v4l2_buffer (88 bytes). Field layout matches kernel exactly on 64-bit.
#[repr(C)]
struct V4l2Buffer {
    index: u32,
    typ: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    _pad0: u32, // alignment padding before timeval (8-byte aligned)
    ts_sec: i64,
    ts_usec: i64,
    timecode: [u8; 24],
    sequence: u32,
    memory: u32,
    // union m (offset/userptr/planes/fd) — 8 bytes on 64-bit
    m_offset: u32,
    _pad1: u32,
    length: u32,
    reserved2: u32,
}
// 5×4 + pad(4) + 2×8 + 24 + 2×4 + 4+pad(4) + 2×4
// = 20 + 4 + 16 + 24 + 8 + 8 + 8 = 88 ✓

impl Default for V4l2Buffer {
    fn default() -> Self {
        Self {
            index: 0,
            typ: V4L2_BUF_TYPE_VIDEO_CAPTURE,
            bytesused: 0,
            flags: 0,
            field: 0,
            _pad0: 0,
            ts_sec: 0,
            ts_usec: 0,
            timecode: [0u8; 24],
            sequence: 0,
            memory: V4L2_MEMORY_MMAP,
            m_offset: 0,
            _pad1: 0,
            length: 0,
            reserved2: 0,
        }
    }
}

/// v4l2_streamparm — we only need timeperframe, so use raw bytes.
type V4l2StreamParm = [u8; 204];

/// Mapped buffer.
struct MappedBuffer {
    ptr: *mut std::ffi::c_void,
    length: usize,
}

impl Drop for MappedBuffer {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                let _ = libc::munmap(self.ptr, self.length);
            }
        }
    }
}

unsafe impl Send for MappedBuffer {}
unsafe impl Sync for MappedBuffer {}

/// Direct V4L2 video source via raw ioctl.
pub struct V4l2DirectSource {
    device: String,
    width: u32,
    height: u32,
    fps: u32,
    format: PixelFormat,
    num_buffers: u32,
    stop_flag: Option<Arc<AtomicBool>>,
}

impl V4l2DirectSource {
    pub fn new(device: impl Into<String>, width: u32, height: u32, fps: u32) -> Self {
        Self {
            device: device.into(),
            width,
            height,
            fps,
            format: PixelFormat::Mjpeg,
            num_buffers: 4,
            stop_flag: None,
        }
    }

    pub fn with_format(mut self, format: PixelFormat) -> Self {
        self.format = format;
        self
    }

    pub fn with_buffers(mut self, n: u32) -> Self {
        self.num_buffers = n.clamp(2, 8);
        self
    }

    fn pixel_format_to_fourcc(fmt: PixelFormat) -> u32 {
        match fmt {
            PixelFormat::Mjpeg => V4L2_PIX_FMT_MJPEG,
            PixelFormat::Yuyv => V4L2_PIX_FMT_YUYV,
            PixelFormat::Nv12 => V4L2_PIX_FMT_NV12,
            PixelFormat::Rgb24 => V4L2_PIX_FMT_YUYV,
        }
    }
}

#[async_trait]
impl VideoSource for V4l2DirectSource {
    async fn start(&mut self) -> VideoResult<mpsc::Receiver<Frame>> {
        let (tx, rx) = mpsc::channel(self.num_buffers as usize);

        let stop_flag = Arc::new(AtomicBool::new(false));
        self.stop_flag = Some(Arc::clone(&stop_flag));

        let device_path = self.device.clone();
        let width = self.width;
        let height = self.height;
        let fps = self.fps;
        let format = self.format;
        let num_buffers = self.num_buffers;
        let fourcc = Self::pixel_format_to_fourcc(format);

        info!(
            device = %device_path,
            width, height, fps, num_buffers,
            "starting V4L2 direct-ioctl capture"
        );

        std::thread::spawn(move || {
            let result = run_direct_capture(
                &device_path,
                width,
                height,
                fps,
                fourcc,
                format,
                num_buffers,
                tx,
                stop_flag,
            );
            if let Err(e) = result {
                error!(error = %e, "V4L2 direct capture thread exited with error");
            } else {
                info!("V4L2 direct capture thread exited cleanly");
            }
        });

        Ok(rx)
    }

    async fn stop(&mut self) -> VideoResult<()> {
        if let Some(flag) = &self.stop_flag {
            flag.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "V4l2DirectSource"
    }
}

fn run_direct_capture(
    device_path: &str,
    width: u32,
    height: u32,
    fps: u32,
    fourcc: u32,
    format: PixelFormat,
    num_buffers: u32,
    tx: mpsc::Sender<Frame>,
    stop_flag: Arc<AtomicBool>,
) -> VideoResult<()> {
    // 1. Open device.
    let fd = unsafe {
        let c_path = std::ffi::CString::new(device_path)
            .map_err(|e| VideoCaptureError::DeviceOpen(format!("invalid path: {e}")))?;
        let ret = libc::open(c_path.as_ptr(), libc::O_RDWR);
        if ret < 0 {
            return Err(VideoCaptureError::DeviceOpen(format!(
                "open {device_path}: {}",
                std::io::Error::last_os_error()
            )));
        }
        ret
    };

    // 2. Set format (VIDIOC_S_FMT).
    let mut fmt = V4l2Format {
        typ: V4L2_BUF_TYPE_VIDEO_CAPTURE,
        _pad0: 0,
        pix: V4l2PixFormat {
            width,
            height,
            pixelformat: fourcc,
            field: 0,
            bytesperline: 0,
            sizeimage: 0,
            colorspace: 0,
            priv_: 0,
            flags: 0,
            ycbcr_enc: 0,
            quantization: 0,
            xfer_func: 0,
        },
        _pad: [0u8; 152],
    };
    unsafe { ioctl(fd, VIDIOC_S_FMT, &mut fmt)? };
    let neg_w = fmt.pix.width;
    let neg_h = fmt.pix.height;
    // Buffer size from S_FMT — some drivers don't fill buf.length in QUERYBUF,
    // so we use sizeimage as the mmap length.
    let buf_size = if fmt.pix.sizeimage > 0 {
        fmt.pix.sizeimage as usize
    } else {
        (neg_w as usize * neg_h as usize * 3).max(1024)
    };
    debug!(
        width = neg_w,
        height = neg_h,
        buf_size,
        "V4L2 direct format negotiated"
    );
    info!(
        width = neg_w,
        height = neg_h,
        "V4L2 direct format negotiated"
    );

    // 3. Set frame rate (VIDIOC_S_PARM).
    if fps > 0 {
        let mut parm: V4l2StreamParm = [0u8; 204];
        // struct v4l2_streamparm { type @0; union @4 -> v4l2_captureparm {
        //   capability @4, capturemode @8, timeperframe { num @12, den @16 },
        //   extendedmode @20, readbuffers @24 } }
        parm[0..4].copy_from_slice(&V4L2_BUF_TYPE_VIDEO_CAPTURE.to_ne_bytes());
        parm[12..16].copy_from_slice(&1u32.to_ne_bytes()); // numerator
        parm[16..20].copy_from_slice(&fps.to_ne_bytes()); // denominator
        unsafe { ioctl(fd, VIDIOC_S_PARM, parm.as_mut_ptr() as *mut _)? };
    }

    // 4. Request MMAP buffers.
    let mut req = V4l2RequestBuffers {
        count: num_buffers,
        typ: V4L2_BUF_TYPE_VIDEO_CAPTURE,
        memory: V4L2_MEMORY_MMAP,
        capabilities: 0,
        flags: 0,
    };
    unsafe { ioctl(fd, VIDIOC_REQBUFS, &mut req)? };
    let n_bufs = req.count;
    debug!(buffers = n_bufs, "V4L2 direct buffers allocated");

    // 5. Query + mmap each buffer, then queue.
    // Use raw byte buffers to eliminate struct layout ambiguity.
    // Field offsets verified via C offsetof() on Orange Pi 5 (aarch64, kernel 6.1):
    //   offset 0: index, offset 4: type, offset 8: bytesused, offset 12: flags,
    //   offset 16: field, offset 60: memory, offset 64: m.offset, offset 72: length.
    // NOTE: v4l2_buffer.timestamp uses kernel timeval which is 12 bytes on this
    // platform (not 16 as glibc timeval), so all fields after field(16) are shifted.
    let mut mapped: Vec<MappedBuffer> = Vec::with_capacity(n_bufs as usize);
    for i in 0..n_bufs {
        let mut buf = [0u8; 88];
        buf[0..4].copy_from_slice(&i.to_ne_bytes());
        buf[4..8].copy_from_slice(&V4L2_BUF_TYPE_VIDEO_CAPTURE.to_ne_bytes());
        buf[60..64].copy_from_slice(&V4L2_MEMORY_MMAP.to_ne_bytes());
        unsafe { ioctl_raw(fd, VIDIOC_QUERYBUF, buf.as_mut_ptr())? };

        let offset = u32::from_ne_bytes([buf[64], buf[65], buf[66], buf[67]]) as usize;
        let length = u32::from_ne_bytes([buf[72], buf[73], buf[74], buf[75]]) as usize;
        let mmap_len = if length > 0 { length } else { buf_size };
        debug!(buf_idx = i, offset, length, mmap_len, "QUERYBUF");

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mmap_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                offset as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            let err = std::io::Error::last_os_error();
            return Err(VideoCaptureError::DeviceConfig(format!(
                "mmap buffer {i} failed: len={mmap_len} offset={offset} err={err}"
            )));
        }
        mapped.push(MappedBuffer {
            ptr,
            length: mmap_len,
        });

        // Queue this buffer (VIDIOC_QBUF).
        buf[4..8].copy_from_slice(&V4L2_BUF_TYPE_VIDEO_CAPTURE.to_ne_bytes());
        buf[60..64].copy_from_slice(&V4L2_MEMORY_MMAP.to_ne_bytes());
        unsafe { ioctl_raw(fd, VIDIOC_QBUF, buf.as_mut_ptr())? };
    }

    // 6. Start streaming.
    let mut stream_on = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    unsafe { ioctl(fd, VIDIOC_STREAMON, &mut stream_on)? };
    info!("V4L2 direct streaming started");

    // 7. Capture loop (raw byte buffer for DQBUF/QBUF).
    let mut seq: u64 = 0;
    loop {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        // Dequeue (VIDIOC_DQBUF) — hot path, raw buffer.
        let mut buf = [0u8; 88];
        buf[4..8].copy_from_slice(&V4L2_BUF_TYPE_VIDEO_CAPTURE.to_ne_bytes());
        buf[60..64].copy_from_slice(&V4L2_MEMORY_MMAP.to_ne_bytes());
        match unsafe { ioctl_raw(fd, VIDIOC_DQBUF, buf.as_mut_ptr()) } {
            Ok(()) => {}
            Err(e) => {
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }
                warn!(error = %e, "V4L2 direct dequeue error");
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
        }

        // Read results by offset.
        let bytesused = u32::from_ne_bytes([buf[8], buf[9], buf[10], buf[11]]) as usize;
        let buf_idx = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        // Copy exactly what the driver wrote, clamped to the mapped buffer
        // length so a buggy driver can't make us read out of bounds.
        let data_len = bytesused.min(mapped.get(buf_idx).map(|b| b.length).unwrap_or(0));
        let frame_data = if buf_idx < mapped.len() && data_len > 0 {
            unsafe {
                std::slice::from_raw_parts(mapped[buf_idx].ptr as *const u8, data_len).to_vec()
            }
        } else {
            Vec::new()
        };

        // Re-queue the buffer immediately.
        buf[4..8].copy_from_slice(&V4L2_BUF_TYPE_VIDEO_CAPTURE.to_ne_bytes());
        buf[60..64].copy_from_slice(&V4L2_MEMORY_MMAP.to_ne_bytes());
        unsafe {
            let _ = ioctl_raw(fd, VIDIOC_QBUF, buf.as_mut_ptr());
        }

        // Build + send Frame (drop-old via try_send).
        let frame = Frame {
            data: frame_data,
            metadata: FrameMetadata {
                width: neg_w,
                height: neg_h,
                format,
                captured_at: Utc::now(),
                seq,
            },
        };

        match tx.try_send(frame) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => break,
            Err(mpsc::error::TrySendError::Full(_)) => {
                debug!("V4L2 direct: dropping frame {seq}");
            }
        }

        seq += 1;
    }

    // 8. Stop + cleanup.
    let mut stream_off: u32 = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    unsafe {
        let _ = ioctl(fd, VIDIOC_STREAMOFF, &mut stream_off);
    }
    drop(mapped);
    unsafe {
        libc::close(fd);
    }
    info!(frames_captured = seq, "V4L2 direct capture thread done");
    Ok(())
}

/// Raw ioctl wrapper (typed pointer).
unsafe fn ioctl<T>(fd: libc::c_int, request: u64, arg: *mut T) -> VideoResult<()> {
    let ret = libc::ioctl(fd, request as libc::c_ulong, arg as *mut libc::c_void);
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        return Err(VideoCaptureError::Capture(format!(
            "ioctl(0x{request:x}) failed: {err}"
        )));
    }
    Ok(())
}

/// Raw ioctl wrapper (void pointer, for [u8; N] buffers).
unsafe fn ioctl_raw(fd: libc::c_int, request: u64, arg: *mut u8) -> VideoResult<()> {
    let ret = libc::ioctl(fd, request as libc::c_ulong, arg as *mut libc::c_void);
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        return Err(VideoCaptureError::Capture(format!(
            "ioctl(0x{request:x}) failed: {err}"
        )));
    }
    Ok(())
}
