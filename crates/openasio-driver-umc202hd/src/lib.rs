//! OpenASIO driver specialized for the Behringer UMC202HD USB interface (ALSA backend).
#![allow(clippy::missing_safety_doc)]
use alsa::device_name::HintIter;
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction as PcmDir, ValueOr};
use openasio_sys as sys;
use std::ffi::CStr;
use std::os::raw::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

type Result<T> = std::result::Result<T, String>;

const CAP_OUTPUT: u32 = sys::OA_CAP_OUTPUT as u32;
const CAP_INPUT: u32 = sys::OA_CAP_INPUT as u32;
const CAP_FULL_DUPLEX: u32 = sys::OA_CAP_FULL_DUPLEX as u32;
const CAPS: u32 = CAP_OUTPUT | CAP_INPUT | CAP_FULL_DUPLEX;

const SUPPORTED_SAMPLE_RATES: &[u32] = &[44100, 48000, 88200, 96000, 176400, 192000];

struct Io {
    cap: Option<PCM>,
    pb: Option<PCM>,
}

struct DriverState {
    host: sys::oa_host_callbacks,
    host_user: *mut c_void,
    dev_name: Option<String>,
    io: Io,
    cfg: sys::oa_stream_config,
    time0: Instant,
    underruns: AtomicU32,
    overruns: AtomicU32,
    in_hw: Vec<i32>,
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
    out_hw: Vec<i32>,
    scratch_out: Vec<f32>,
    in_planes: Vec<*const f32>,
    out_planes: Vec<*mut f32>,
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

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_ascii_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn hint_matches_umc202hd(name: Option<&str>, desc: Option<&str>) -> bool {
    let needle = "umc202hd";
    name.iter()
        .chain(desc.iter())
        .map(|s| normalize(s))
        .any(|s| s.contains(needle))
}

fn enumerate_umc202hd_devices() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(iter) = HintIter::new_str(None, "pcm") {
        for hint in iter {
            let name = hint.name.clone();
            let desc = hint.desc.clone();
            if hint_matches_umc202hd(name.as_deref(), desc.as_deref()) {
                if let Some(n) = name {
                    out.push(n);
                }
            }
        }
    }
    if out.is_empty() {
        out.push("hw:UMC202HD".to_string());
    }
    out.sort();
    out.dedup();
    out
}

fn default_device_name() -> String {
    enumerate_umc202hd_devices()
        .into_iter()
        .next()
        .unwrap_or_else(|| "hw:UMC202HD".to_string())
}

fn hw_setup(pcm: &PCM, dir: PcmDir, cfg: &sys::oa_stream_config) -> Result<()> {
    let hwp = HwParams::any(pcm).map_err(|e| e.to_string())?;
    hwp.set_access(Access::RWInterleaved)
        .map_err(|e| e.to_string())?;
    let channels = match dir {
        PcmDir::Capture => cfg.in_channels,
        PcmDir::Playback => cfg.out_channels,
    } as u32;
    hwp.set_channels(channels).map_err(|e| e.to_string())?;
    hwp.set_rate(cfg.sample_rate, ValueOr::Nearest)
        .map_err(|e| e.to_string())?;
    hwp.set_format(Format::s32()).map_err(|e| e.to_string())?;
    let period = cfg.buffer_frames as i64;
    if period <= 0 {
        return Err("invalid buffer size".into());
    }
    hwp.set_period_size(period, ValueOr::Nearest)
        .map_err(|e| e.to_string())?;
    hwp.set_buffer_size(period * 2).map_err(|e| e.to_string())?;
    pcm.hw_params(&hwp).map_err(|e| e.to_string())?;

    let swp = pcm.sw_params_current().map_err(|e| e.to_string())?;
    swp.set_start_threshold(period).map_err(|e| e.to_string())?;
    swp.set_avail_min(period).map_err(|e| e.to_string())?;
    pcm.sw_params(&swp).map_err(|e| e.to_string())?;
    Ok(())
}

fn i32_to_f32(src: &[i32], dst: &mut [f32]) {
    const SCALE: f32 = 1.0 / 2147483648.0;
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        *d = (*s as f32) * SCALE;
    }
}

fn f32_to_i32(src: &[f32], dst: &mut [i32]) {
    const MAX: f32 = 2147483647.0;
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        let mut v = *s;
        if v >= 1.0 {
            *d = i32::MAX;
        } else if v <= -1.0 {
            *d = i32::MIN;
        } else {
            v *= MAX;
            *d = v.round() as i32;
        }
    }
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
            let total = frames * ich;
            let res = cap
                .io_i32()
                .and_then(|io| io.readi(&mut driver.state.in_hw[..total]));
            match res {
                Ok(read) => {
                    let samples = read * ich;
                    i32_to_f32(
                        &driver.state.in_hw[..samples],
                        &mut driver.state.in_buf[..samples],
                    );
                    if samples < total {
                        driver.state.in_buf[samples..total].fill(0.0);
                    }
                }
                Err(e) => {
                    if e.errno() == nix::errno::Errno::EPIPE as i32 {
                        let _ = cap.prepare();
                        driver.state.overruns.fetch_add(1, Ordering::Relaxed);
                    }
                    driver.state.in_buf[..total].fill(0.0);
                }
            }
        }

        if interleaved {
            driver.state.out_buf[..frames * och].fill(0.0);
        } else {
            driver.state.scratch_out[..frames * och].fill(0.0);
        }

        let ti = sys::oa_time_info {
            host_time_ns: driver.state.time0.elapsed().as_nanos() as u64,
            device_time_ns: 0,
            underruns: driver.state.underruns.load(Ordering::Relaxed),
            overruns: driver.state.overruns.load(Ordering::Relaxed),
        };

        if let Some(cb) = driver.state.host.process {
            let in_ptr: *const c_void = if ich == 0 {
                ptr::null()
            } else if interleaved {
                driver.state.in_buf.as_ptr() as *const c_void
            } else {
                driver.state.in_planes.as_ptr() as *const c_void
            };
            let out_ptr: *mut c_void = if interleaved {
                driver.state.out_buf.as_mut_ptr() as *mut c_void
            } else {
                driver.state.out_planes.as_mut_ptr() as *mut c_void
            };
            let keep = cb(
                driver.state.host_user,
                in_ptr,
                out_ptr,
                frames as u32,
                &ti as *const _,
                &driver.state.cfg as *const _,
            );
            if keep == sys::OA_FALSE {
                driver.state.running.store(false, Ordering::Release);
                continue;
            }
        }

        if !interleaved {
            let frames_usize = frames;
            for f in 0..frames_usize {
                for c in 0..och {
                    let plane = driver.state.scratch_out.as_ptr().add(c * frames_usize);
                    driver.state.out_buf[f * och + c] = *plane.add(f);
                }
            }
        }

        f32_to_i32(
            &driver.state.out_buf[..frames * och],
            &mut driver.state.out_hw[..frames * och],
        );

        if let Some(pb) = driver.state.io.pb.as_ref() {
            let res = pb
                .io_i32()
                .and_then(|io| io.writei(&driver.state.out_hw[..frames * och]));
            if let Err(e) = res {
                if e.errno() == nix::errno::Errno::EPIPE as i32 {
                    let _ = pb.prepare();
                    driver.state.underruns.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

unsafe extern "C" fn get_caps(_: *mut sys::oa_driver) -> u32 {
    CAPS
}

unsafe extern "C" fn query_devices(_selfp: *mut sys::oa_driver, buf: *mut i8, len: usize) -> i32 {
    let names = enumerate_umc202hd_devices().join("\n");
    let bytes = names.as_bytes();
    let n = bytes.len().min(len.saturating_sub(1));
    if n > 0 {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n);
    }
    if len > 0 {
        *buf.add(n) = 0;
    }
    sys::OA_OK
}

unsafe extern "C" fn open_device(selfp: *mut sys::oa_driver, name: *const i8) -> i32 {
    let driver = &mut *(selfp as *mut Driver);
    let chosen = if name.is_null() {
        default_device_name()
    } else {
        CStr::from_ptr(name).to_string_lossy().to_string()
    };
    driver.state.dev_name = Some(chosen);
    sys::OA_OK
}

unsafe extern "C" fn close_device(selfp: *mut sys::oa_driver) -> i32 {
    let driver = &mut *(selfp as *mut Driver);
    driver.state.stop_worker();
    driver.state.io.cap = None;
    driver.state.io.pb = None;
    sys::OA_OK
}

unsafe extern "C" fn get_default_config(
    _selfp: *mut sys::oa_driver,
    out: *mut sys::oa_stream_config,
) -> i32 {
    if out.is_null() {
        return sys::OA_ERR_INVALID_ARG;
    }
    (*out).sample_rate = 48000;
    (*out).buffer_frames = 128;
    (*out).in_channels = 2;
    (*out).out_channels = 2;
    (*out).format = sys::oa_sample_format::OA_SAMPLE_F32;
    (*out).layout = sys::oa_buffer_layout::OA_BUF_INTERLEAVED;
    sys::OA_OK
}

fn validate_config(cfg: &sys::oa_stream_config) -> Result<()> {
    if cfg.format != sys::oa_sample_format::OA_SAMPLE_F32 {
        return Err("UMC202HD driver only supports float32".into());
    }
    if cfg.out_channels != 2 {
        return Err("UMC202HD playback requires 2 channels".into());
    }
    if cfg.in_channels != 0 && cfg.in_channels != 2 {
        return Err("UMC202HD capture supports 0 or 2 channels".into());
    }
    if !SUPPORTED_SAMPLE_RATES.contains(&cfg.sample_rate) {
        return Err("unsupported sample rate".into());
    }
    if cfg.buffer_frames == 0 {
        return Err("buffer must be > 0".into());
    }
    Ok(())
}

unsafe extern "C" fn start(selfp: *mut sys::oa_driver, cfg: *const sys::oa_stream_config) -> i32 {
    if cfg.is_null() {
        return sys::OA_ERR_INVALID_ARG;
    }
    let cfg = &*cfg;
    let driver = &mut *(selfp as *mut Driver);
    if validate_config(cfg).is_err() {
        return sys::OA_ERR_UNSUPPORTED;
    }

    driver.state.stop_worker();
    driver.state.io.cap = None;
    driver.state.io.pb = None;

    let name = driver
        .state
        .dev_name
        .clone()
        .unwrap_or_else(default_device_name);

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

    if hw_setup(&pb, PcmDir::Playback, cfg).is_err() {
        return sys::OA_ERR_BACKEND;
    }
    if let Some(ref c) = cap {
        if hw_setup(c, PcmDir::Capture, cfg).is_err() {
            return sys::OA_ERR_BACKEND;
        }
    }

    let frames = cfg.buffer_frames as usize;
    let ich = cfg.in_channels as usize;
    let och = cfg.out_channels as usize;

    driver.state.in_hw.resize(frames * ich.max(1), 0);
    driver.state.in_buf.resize(frames * ich.max(1), 0.0);
    driver.state.out_buf.resize(frames * och, 0.0);
    driver.state.out_hw.resize(frames * och, 0);
    driver.state.scratch_out.resize(frames * och, 0.0);
    driver.state.in_planes.clear();
    if ich > 0 {
        for c in 0..ich {
            let ptr = driver.state.in_buf.as_ptr().wrapping_add(c);
            driver.state.in_planes.push(ptr);
        }
    }
    driver.state.out_planes.clear();
    if och > 0 {
        for c in 0..och {
            let ptr = driver
                .state
                .scratch_out
                .as_mut_ptr()
                .wrapping_add(c * frames);
            driver.state.out_planes.push(ptr);
        }
    }

    driver.state.cfg = *cfg;
    driver.state.time0 = Instant::now();
    driver.state.underruns.store(0, Ordering::Relaxed);
    driver.state.overruns.store(0, Ordering::Relaxed);
    driver.state.io.pb = Some(pb);
    driver.state.io.cap = cap;
    driver.state.running.store(true, Ordering::Release);
    let driver_ptr = selfp as *mut Driver;
    driver.state.worker = Some(std::thread::spawn(move || unsafe {
        driver_thread(driver_ptr);
    }));

    sys::OA_OK
}

unsafe extern "C" fn stop(selfp: *mut sys::oa_driver) -> i32 {
    let driver = &mut *(selfp as *mut Driver);
    driver.state.stop_worker();
    driver.state.io.cap = None;
    driver.state.io.pb = None;
    sys::OA_OK
}

unsafe extern "C" fn get_latency(
    selfp: *mut sys::oa_driver,
    in_lat: *mut u32,
    out_lat: *mut u32,
) -> i32 {
    let driver = &mut *(selfp as *mut Driver);
    if !in_lat.is_null() {
        *in_lat = if driver.state.cfg.in_channels > 0 {
            driver.state.cfg.buffer_frames
        } else {
            0
        };
    }
    if !out_lat.is_null() {
        *out_lat = driver.state.cfg.buffer_frames;
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
            host: *p.host,
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
            in_hw: Vec::new(),
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            out_hw: Vec::new(),
            scratch_out: Vec::new(),
            in_planes: Vec::new(),
            out_planes: Vec::new(),
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
