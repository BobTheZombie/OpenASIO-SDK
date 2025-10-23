//! OpenASIO driver for AMD Family 17h HDA controllers (ALSA backend, full-duplex)
#![allow(clippy::missing_safety_doc)]
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction as PcmDir, ValueOr};
use openasio_sys as sys;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::{ffi::CStr, os::raw::c_void, ptr, time::Instant};

const CAP_OUTPUT: u32 = 1 << 0;
const CAP_INPUT: u32 = 1 << 1;
const CAP_FULL_DUPLEX: u32 = 1 << 2;
const CAP_SET_SR: u32 = 1 << 3;
const CAP_SET_BF: u32 = 1 << 4;
const CAPS: u32 = CAP_OUTPUT | CAP_INPUT | CAP_FULL_DUPLEX | CAP_SET_SR | CAP_SET_BF;

struct Io {
    cap: Option<PCM>,
    pb: Option<PCM>,
}

struct DriverState {
    host: *const sys::oa_host_callbacks,
    host_user: *mut c_void,
    dev_name: Option<String>,
    io: Io,
    cfg: sys::oa_stream_config,
    time0: Instant,
    underruns: AtomicU32,
    overruns: AtomicU32,
    in_buf: Vec<f32>,  // interleaved
    out_buf: Vec<f32>, // interleaved
    running: AtomicBool,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[repr(C)]
struct Driver {
    vt: sys::oa_driver_vtable,
    state: DriverState,
}

impl DriverState {
    fn stop_worker(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DriverState {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

unsafe extern "C" fn get_caps(_: *mut sys::oa_driver) -> u32 {
    CAPS
}

unsafe extern "C" fn query_devices(_selfp: *mut sys::oa_driver, buf: *mut i8, len: usize) -> i32 {
    // Minimal enumeration: typical HDA device nodes; host may pass exact ALSA "hw:X,Y"
    let list = "default\nhw:0,0\nhw:1,0\n";
    let bytes = list.as_bytes();
    let n = bytes.len().min(len.saturating_sub(1));
    if n > 0 {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n);
    }
    if len > 0 {
        *buf.add(n) = 0;
    }
    sys::OA_OK
}

unsafe extern "C" fn open_device(selfp: *mut sys::oa_driver, name: *const i8) -> i32 {
    let s = &mut *(selfp as *mut Driver);
    s.state.dev_name = if name.is_null() {
        None
    } else {
        Some(CStr::from_ptr(name).to_string_lossy().to_string())
    };
    sys::OA_OK
}

unsafe extern "C" fn close_device(selfp: *mut sys::oa_driver) -> i32 {
    let s = &mut *(selfp as *mut Driver);
    s.state.stop_worker();
    s.state.io.cap = None;
    s.state.io.pb = None;
    sys::OA_OK
}

fn hw_setup(pcm: &PCM, dir: PcmDir, cfg: &sys::oa_stream_config) -> Result<(), String> {
    let hwp = HwParams::any(pcm).map_err(|e| e.to_string())?;
    hwp.set_access(Access::RWInterleaved)
        .map_err(|e| e.to_string())?;
    hwp.set_channels(match dir {
        PcmDir::Capture => cfg.in_channels as u32,
        PcmDir::Playback => cfg.out_channels as u32,
    })
    .map_err(|e| e.to_string())?;
    hwp.set_rate(cfg.sample_rate as u32, ValueOr::Nearest)
        .map_err(|e| e.to_string())?;
    hwp.set_format(Format::float()).map_err(|e| e.to_string())?;
    let period = cfg.buffer_frames as i64;
    hwp.set_period_size(period, ValueOr::Nearest)
        .map_err(|e| e.to_string())?;
    hwp.set_buffer_size(period * 2).map_err(|e| e.to_string())?; // 2 periods buffer
    pcm.hw_params(&hwp).map_err(|e| e.to_string())?;

    let swp = pcm.sw_params_current().map_err(|e| e.to_string())?;
    swp.set_start_threshold(period).map_err(|e| e.to_string())?;
    swp.set_avail_min(period).map_err(|e| e.to_string())?;
    pcm.sw_params(&swp).map_err(|e| e.to_string())?;
    Ok(())
}

unsafe fn driver_thread(selfp: *mut Driver) {
    loop {
        let driver = &mut *selfp;
        if !driver.state.running.load(Ordering::Acquire) {
            break;
        }

        let frames = driver.state.cfg.buffer_frames as usize;
        let ich = driver.state.cfg.in_channels as usize;
        let och = driver.state.cfg.out_channels as usize;
        let interleaved = matches!(
            driver.state.cfg.layout,
            sys::oa_buffer_layout::OA_BUF_INTERLEAVED
        );

        if let Some(cap) = driver.state.io.cap.as_ref() {
            let res = cap
                .io_f32()
                .and_then(|io| io.readi(&mut driver.state.in_buf[..frames * ich]));
            if let Err(e) = res {
                if e.errno() == nix::errno::Errno::EPIPE as i32 {
                    let _ = cap.prepare();
                    driver.state.underruns.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        let ti = sys::oa_time_info {
            host_time_ns: driver.state.time0.elapsed().as_nanos() as u64,
            device_time_ns: 0,
            underruns: driver.state.underruns.load(Ordering::Relaxed),
            overruns: driver.state.overruns.load(Ordering::Relaxed),
        };
        if !driver.state.host.is_null() {
            let host = &*driver.state.host;
            if let Some(cb) = host.process {
                let in_ptr: *const c_void;
                let out_ptr: *mut c_void;
                if interleaved {
                    in_ptr = if ich > 0 {
                        driver.state.in_buf.as_ptr() as *const c_void
                    } else {
                        ptr::null()
                    };
                    out_ptr = driver.state.out_buf.as_mut_ptr() as *mut c_void;
                } else {
                    let mut in_planes: Vec<*const f32> = (0..ich)
                        .map(|c| driver.state.in_buf.as_ptr().wrapping_add(c))
                        .collect();
                    let mut out_planes: Vec<*mut f32> = (0..och)
                        .map(|c| driver.state.out_buf.as_mut_ptr().wrapping_add(c))
                        .collect();
                    in_ptr = if ich > 0 {
                        in_planes.as_ptr() as *const c_void
                    } else {
                        ptr::null()
                    };
                    out_ptr = out_planes.as_mut_ptr() as *mut c_void;
                }
                cb(
                    driver.state.host_user,
                    in_ptr,
                    out_ptr,
                    frames as u32,
                    &ti as *const _,
                    &driver.state.cfg as *const _,
                );
            }
        }

        if let Some(pb) = driver.state.io.pb.as_ref() {
            let res = pb
                .io_f32()
                .and_then(|io| io.writei(&driver.state.out_buf[..frames * och]));
            if let Err(e) = res {
                if e.errno() == nix::errno::Errno::EPIPE as i32 {
                    let _ = pb.prepare();
                    driver.state.underruns.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

unsafe extern "C" fn get_default_config(
    _selfp: *mut sys::oa_driver,
    out: *mut sys::oa_stream_config,
) -> i32 {
    (*out).sample_rate = 48000;
    (*out).buffer_frames = 128;
    (*out).in_channels = 2;
    (*out).out_channels = 2;
    (*out).format = sys::oa_sample_format::OA_SAMPLE_F32;
    (*out).layout = sys::oa_buffer_layout::OA_BUF_INTERLEAVED;
    sys::OA_OK
}

unsafe extern "C" fn start(selfp: *mut sys::oa_driver, cfg: *const sys::oa_stream_config) -> i32 {
    if cfg.is_null() {
        return sys::OA_ERR_INVALID_ARG;
    }
    let cfg = &*cfg;
    let s = &mut *(selfp as *mut Driver);
    s.state.stop_worker();
    s.state.io.pb = None;
    s.state.io.cap = None;
    s.state.cfg = *cfg;
    s.state.time0 = Instant::now();
    s.state.underruns.store(0, Ordering::Relaxed);
    s.state.overruns.store(0, Ordering::Relaxed);
    let name = s
        .state
        .dev_name
        .clone()
        .unwrap_or_else(|| "default".to_string());

    let pb = match PCM::new(&name, PcmDir::Playback, false) {
        Ok(p) => p,
        Err(_) => return sys::OA_ERR_DEVICE,
    };
    let cap = if cfg.in_channels > 0 {
        match PCM::new(&name, PcmDir::Capture, false) {
            Ok(c) => Some(c),
            Err(_) => return sys::OA_ERR_DEVICE,
        }
    } else {
        None
    };

    if let Some(ref c) = cap {
        if hw_setup(c, PcmDir::Capture, cfg).is_err() {
            return sys::OA_ERR_BACKEND;
        }
    }
    if hw_setup(&pb, PcmDir::Playback, cfg).is_err() {
        return sys::OA_ERR_BACKEND;
    }

    let frames = cfg.buffer_frames as usize;
    let ich = cfg.in_channels as usize;
    let och = cfg.out_channels as usize;
    s.state.in_buf.resize(frames * ich.max(1), 0.0);
    s.state.out_buf.resize(frames * och, 0.0);
    s.state.io.pb = Some(pb);
    s.state.io.cap = cap;
    s.state.running.store(true, Ordering::Release);
    let driver_ptr = selfp as *mut Driver as usize;
    s.state.worker = Some(std::thread::spawn(move || unsafe {
        driver_thread(driver_ptr as *mut Driver);
    }));

    sys::OA_OK
}

unsafe extern "C" fn stop(selfp: *mut sys::oa_driver) -> i32 {
    let s = &mut *(selfp as *mut Driver);
    s.state.stop_worker();
    s.state.io.pb = None;
    s.state.io.cap = None;
    sys::OA_OK
}

unsafe extern "C" fn get_latency(
    _: *mut sys::oa_driver,
    in_lat: *mut u32,
    out_lat: *mut u32,
) -> i32 {
    if !in_lat.is_null() {
        *in_lat = 0;
    }
    if !out_lat.is_null() {
        *out_lat = 0;
    }
    sys::OA_OK
}
unsafe extern "C" fn set_sr(_: *mut sys::oa_driver, _: u32) -> i32 {
    sys::OA_ERR_UNSUPPORTED
}
unsafe extern "C" fn set_buf(_: *mut sys::oa_driver, _: u32) -> i32 {
    sys::OA_ERR_UNSUPPORTED
}

#[no_mangle]
pub unsafe extern "C" fn openasio_driver_create(
    params: *const sys::oa_create_params,
    out: *mut *mut sys::oa_driver,
) -> i32 {
    if params.is_null() || out.is_null() {
        return sys::OA_ERR_INVALID_ARG;
    }
    let p = &*params;
    if p.host.is_null() {
        return sys::OA_ERR_INVALID_ARG;
    }
    let drv = Box::new(Driver {
        vt: sys::oa_driver_vtable {
            struct_size: std::mem::size_of::<sys::oa_driver_vtable>() as u32,
            get_caps: Some(get_caps),
            query_devices: Some(query_devices),
            open_device: Some(open_device),
            close_device: Some(close_device),
            get_default_config: Some(get_default_config),
            start: Some(start),
            stop: Some(stop),
            get_latency: Some(get_latency),
            set_sample_rate: Some(set_sr),
            set_buffer_frames: Some(set_buf),
        },
        state: DriverState {
            host: p.host,
            host_user: p.host_user,
            dev_name: None,
            io: Io {
                cap: None,
                pb: None,
            },
            cfg: sys::oa_stream_config {
                sample_rate: 48000,
                buffer_frames: 128,
                in_channels: 2,
                out_channels: 2,
                format: sys::oa_sample_format::OA_SAMPLE_F32,
                layout: sys::oa_buffer_layout::OA_BUF_INTERLEAVED,
            },
            time0: Instant::now(),
            underruns: AtomicU32::new(0),
            overruns: AtomicU32::new(0),
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            running: AtomicBool::new(false),
            worker: None,
        },
    });
    *out = Box::into_raw(drv) as *mut sys::oa_driver;
    sys::OA_OK
}

#[no_mangle]
pub unsafe extern "C" fn openasio_driver_destroy(driver: *mut sys::oa_driver) {
    if !driver.is_null() {
        let _ = Box::from_raw(driver as *mut Driver);
    }
}
