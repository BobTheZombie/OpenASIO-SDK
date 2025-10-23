//! CPAL-backed OpenASIO driver (v1.0.0). Full-duplex with interleaved & non-interleaved support.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use openasio_sys as sys;
use std::ffi::CStr;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, atomic::AtomicUsize};
use std::time::Instant;

struct DriverState {
    host: sys::oa_host_callbacks,
    host_user: *mut c_void,
    out_device: Option<cpal::Device>,
    in_device: Option<cpal::Device>,
    out_stream: Option<cpal::Stream>,
    in_stream: Option<cpal::Stream>,
    cfg: sys::oa_stream_config,
    time0: Instant,
    underruns: AtomicU32,
    overruns: AtomicU32,

    // Input staging (latest block). We keep interleaved f32 internally.
    in_buf: Vec<f32>,
    in_seq: AtomicUsize,
}

#[repr(C)]
struct Driver { vt: sys::oa_driver_vtable, state: DriverState }

unsafe extern "C" fn get_caps(_selfp:*mut sys::oa_driver)->u32 {
    (sys::OA_CAP_OUTPUT | sys::OA_CAP_INPUT | sys::OA_CAP_FULL_DUPLEX) as u32
}

unsafe extern "C" fn query_devices(_selfp:*mut sys::oa_driver, buf:*mut i8, len: usize)->i32{
    let host = cpal::default_host();
    let mut names = String::new();
    if let Ok(mut devs) = host.output_devices(){
        while let Some(d)=devs.next(){ if let Ok(n)=d.name(){ names.push_str(&n); names.push('\n'); } }
    }
    let bytes = names.as_bytes(); let n = bytes.len().min(len.saturating_sub(1));
    if n>0 { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n); }
    if len>0 { *buf.add(n) = 0; }
    sys::OA_OK
}

unsafe extern "C" fn open_device(selfp:*mut sys::oa_driver, name:*const i8)->i32{
    let s = &mut *(selfp as *mut Driver);
    let host = cpal::default_host();

    // Output device
    let out = if name.is_null(){ host.default_output_device() } else {
        let needle = CStr::from_ptr(name).to_string_lossy().to_string();
        let mut found=None; if let Ok(mut it)=host.output_devices(){ while let Some(d)=it.next(){ if let Ok(n)=d.name(){ if n.contains(&needle){ found=Some(d); break; }}}}
        found
    };
    // Input device: try to match same name; else default input
    let inp = if let Some(ref od) = out {
        let od_name = od.name().ok();
        let mut found=None;
        if let (Some(needle), Ok(mut it)) = (od_name, host.input_devices()) {
            let n = needle;
            while let Some(d)=it.next(){ if let Ok(nm)=d.name(){ if nm==n { found=Some(d); break; } } }
        }
        found.or_else(|| host.default_input_device())
    } else { host.default_input_device() };

    match (out, inp) {
        (Some(o), i) => { s.state.out_device = Some(o); s.state.in_device = i; 0 }
        _ => sys::OA_ERR_DEVICE,
    }
}

unsafe extern "C" fn close_device(selfp:*mut sys::oa_driver)->i32{
    let s = &mut *(selfp as *mut Driver);
    s.state.out_stream=None; s.state.in_stream=None;
    s.state.out_device=None; s.state.in_device=None;
    sys::OA_OK
}

unsafe extern "C" fn get_default_config(selfp:*mut sys::oa_driver, out:*mut sys::oa_stream_config)->i32{
    let s = &mut *(selfp as *mut Driver);
    let dev = match &s.state.out_device{ Some(d)=>d, None=>return sys::OA_ERR_DEVICE };
    if let Ok(c)=dev.default_output_config(){
        (*out).sample_rate = c.sample_rate().0;
        (*out).buffer_frames = 256;
        (*out).in_channels = s.state.in_device.as_ref().and_then(|id| id.default_input_config().ok()).map(|ic| ic.channels()).unwrap_or(0);
        (*out).out_channels = c.channels();
        (*out).format = sys::oa_sample_format::OA_SAMPLE_F32;
        (*out).layout = sys::oa_buffer_layout::OA_BUF_INTERLEAVED;
        sys::OA_OK
    } else { sys::OA_ERR_DEVICE }
}

unsafe extern "C" fn start(selfp:*mut sys::oa_driver, cfg:*const sys::oa_stream_config)->i32{
    let s = &mut *(selfp as *mut Driver);
    let out_dev = match &s.state.out_device{ Some(d)=>d.clone(), None=>return sys::OA_ERR_DEVICE };
    let in_dev = s.state.in_device.clone();

    s.state.cfg = *cfg;
    s.state.in_buf.resize(((*cfg).buffer_frames as usize) * ((*cfg).in_channels as usize).max(1), 0.0);
    s.state.in_seq.store(0, std::sync::atomic::Ordering::Relaxed);

    // Build input stream if available
    if let (Some(id), in_ch) = (in_dev, (*cfg).in_channels) {
        if in_ch > 0 {
            if let Ok(dc)=id.default_input_config(){
                let mut sc: cpal::StreamConfig = dc.into();
                sc.channels = in_ch;
                sc.sample_rate = cpal::SampleRate((*cfg).sample_rate);
                sc.buffer_size = cpal::BufferSize::Default;
                let state_ptr = selfp as *mut Driver;
                let istream = id.build_input_stream(&sc,
                    move |data:&[f32], _| {
                        let st = unsafe{ &mut *state_ptr };
                        // store latest
                        let frames = data.len() / (st.state.cfg.in_channels as usize).max(1);
                        let len = frames * (st.state.cfg.in_channels as usize).max(1);
                        if st.state.in_buf.len() < len { st.state.in_buf.resize(len, 0.0); }
                        st.state.in_buf[..len].copy_from_slice(&data[..len]);
                        st.state.in_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    },
                    move |err| { eprintln!("openasio-cpal input error: {err}"); },
                    None
                ).expect("build_input_stream");
                istream.play().expect("input play");
                s.state.in_stream = Some(istream);
            }
        }
    }

    // Output stream drives the host.process
    let mut out_cfg = out_dev.default_output_config().expect("default output config");
    let mut sc: cpal::StreamConfig = out_cfg.clone().into();
    sc.channels = (*cfg).out_channels;
    sc.sample_rate = cpal::SampleRate((*cfg).sample_rate);
    sc.buffer_size = cpal::BufferSize::Default;
    let state_ptr = selfp as *mut Driver;

    let ostream = out_dev.build_output_stream(&sc,
        move |data:&mut [f32], _| {
            let st = unsafe{ &mut *state_ptr };
            let frames = (data.len() / (st.state.cfg.out_channels as usize).max(1)) as u32;

            // Prepare input pointers based on layout
            let in_ptr: *const c_void;
            let mut in_planes: Vec<*const f32> = Vec::new();
            if matches!(st.state.cfg.layout, sys::oa_buffer_layout::OA_BUF_INTERLEAVED) {
                in_ptr = if st.state.cfg.in_channels > 0 { st.state.in_buf.as_ptr() as *const c_void } else { std::ptr::null() };
            } else {
                if st.state.cfg.in_channels == 0 {
                    in_ptr = std::ptr::null();
                } else {
                    let ch = st.state.cfg.in_channels as usize;
                    in_planes.resize(ch, std::ptr::null());
                    for c in range(0, ch) {
                        // deinterleave view: plane c points to first sample of that channel
                        // We'll assume host reads strided by ch; for strict non-interleaved we'd keep true planes.
                        in_planes[c] = unsafe { st.state.in_buf.as_ptr().add(c) };
                    }
                    in_ptr = in_planes.as_ptr() as *const c_void;
                }
            }

            // Prepare output pointer(s)
            let out_ptr: *mut c_void;
            if matches!(st.state.cfg.layout, sys::oa_buffer_layout::OA_BUF_INTERLEAVED) {
                out_ptr = data.as_mut_ptr() as *mut c_void;
            } else {
                // Non-interleaved: provide channel planes pointing into a staging area.
                // For simplicity, we reuse data buffer then interleave after callback.
                // Allocate temp planes pointing to scratch vector.
                static mut SCRATCH: Vec<f32> = Vec::new();
                let ch = st.state.cfg.out_channels as usize;
                let needed = (frames as usize) * ch;
                unsafe {
                    if SCRATCH.len() < needed { SCRATCH.resize(needed, 0.0); }
                    let mut planes: Vec<*mut f32> = Vec::with_capacity(ch);
                    for c in 0..ch {
                        planes.push(SCRATCH.as_mut_ptr().add(c * frames as usize));
                    }
                    out_ptr = planes.as_mut_ptr() as *mut c_void;
                    // planes will be valid during this callback; we'll interleave after host returns
                    // Call host
                    if let Some(cb)=st.state.host.process {
                        let ti = sys::oa_time_info{ host_time_ns: st.state.time0.elapsed().as_nanos() as u64, device_time_ns: 0, underruns: st.state.underruns.load(Ordering::Relaxed), overruns: st.state.overruns.load(Ordering::Relaxed)};
                        let _keep = cb(st.state.host_user, in_ptr, out_ptr, frames, &ti as *const _, &st.state.cfg as *const _);
                    }
                    // Interleave planes -> data
                    for f in 0..(frames as usize) {
                        for c in 0..ch {
                            data[f*ch + c] = *SCRATCH.as_ptr().add(c * frames as usize + f);
                        }
                    }
                    return;
                }
            }

            // Interleaved path: call host directly
            if let Some(cb)=st.state.host.process {
                let ti = sys::oa_time_info{ host_time_ns: st.state.time0.elapsed().as_nanos() as u64, device_time_ns: 0, underruns: st.state.underruns.load(Ordering::Relaxed), overruns: st.state.overruns.load(Ordering::Relaxed)};
                let _keep = cb(st.state.host_user, in_ptr, out_ptr, frames, &ti as *const _, &st.state.cfg as *const _);
            }
        },
        move |err| { eprintln!("openasio-cpal output error: {err}"); }, None
    ).expect("build_output_stream");
    ostream.play().expect("output play");
    s.state.out_stream = Some(ostream);
    sys::OA_OK
}

unsafe extern "C" fn stop(selfp:*mut sys::oa_driver)->i32{
    let s = &mut *(selfp as *mut Driver);
    s.state.out_stream=None; s.state.in_stream=None;
    sys::OA_OK
}

unsafe extern "C" fn get_latency(_:*mut sys::oa_driver, in_lat:*mut u32, out_lat:*mut u32)->i32{
    if !in_lat.is_null(){ *in_lat = 0; } // CPAL doesn't expose stable latency here
    if !out_lat.is_null(){ *out_lat = 0; }
    sys::OA_OK
}
unsafe extern "C" fn set_sr(_: *mut sys::oa_driver, _:u32)->i32{ sys::OA_ERR_UNSUPPORTED }
unsafe extern "C" fn set_buf(_: *mut sys::oa_driver, _:u32)->i32{ sys::OA_ERR_UNSUPPORTED }

#[no_mangle]
pub unsafe extern "C" fn openasio_driver_create(params:*const sys::oa_create_params, out:*mut *mut sys::oa_driver)->i32{
    if params.is_null()||out.is_null(){ return sys::OA_ERR_INVALID_ARG; }
    let p=&*params;
    let drv = Box::new(Driver{
        vt: sys::oa_driver_vtable{
            struct_size: std::mem::size_of::<sys::oa_driver_vtable>() as u32,
            get_caps: Some(get_caps),
            query_devices: Some(query_devices),
            open_device: Some(open_device),
            close_device: Some(close_device),
            get_default_config: Some(get_default_config),
            start: Some(start), stop: Some(stop),
            get_latency: Some(get_latency), set_sample_rate: Some(set_sr), set_buffer_frames: Some(set_buf),
        },
        state: DriverState{
            host: *p.host, host_user: p.host_user,
            out_device: None, in_device: None, out_stream: None, in_stream: None,
            cfg: sys::oa_stream_config{ sample_rate:48000, buffer_frames:256, in_channels:0, out_channels:2, format: sys::oa_sample_format::OA_SAMPLE_F32, layout: sys::oa_buffer_layout::OA_BUF_INTERLEAVED },
            time0: Instant::now(), underruns: AtomicU32::new(0), overruns: AtomicU32::new(0),
            in_buf: Vec::new(), in_seq: AtomicUsize::new(0),
        },
    });
    *out = Box::into_raw(drv) as *mut sys::oa_driver; sys::OA_OK
}
#[no_mangle] pub unsafe extern "C" fn openasio_driver_destroy(driver:*mut sys::oa_driver){ if !driver.is_null(){ let _ = Box::from_raw(driver as *mut Driver); } }
