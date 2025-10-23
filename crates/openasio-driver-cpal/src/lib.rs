//! CPAL-backed OpenASIO driver (v0.2.0). Output-only path; inputs reserved.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use openasio_sys as sys;
use std::ffi::CStr;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

struct DriverState {
    host: sys::oa_host_callbacks,
    host_user: *mut c_void,
    device: Option<cpal::Device>,
    stream: Option<cpal::Stream>,
    cfg: sys::oa_stream_config,
    time0: Instant,
    underruns: AtomicU32,
    overruns: AtomicU32,
}
#[repr(C)] struct Driver { vt: sys::oa_driver_vtable, state: DriverState }

unsafe extern "C" fn get_caps(_selfp:*mut sys::oa_driver)->u32 {
    (sys::OA_CAP_OUTPUT | sys::OA_CAP_SET_SAMPLERATE | sys::OA_CAP_SET_BUFFRAMES) as u32
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
    let dev = if name.is_null(){ host.default_output_device() } else {
        let needle = CStr::from_ptr(name).to_string_lossy().to_string();
        let mut found=None; if let Ok(mut it)=host.output_devices(){ while let Some(d)=it.next(){ if let Ok(n)=d.name(){ if n.contains(&needle){ found=Some(d); break; }}}}
        found
    };
    match dev{ Some(d)=>{ s.state.device=Some(d); 0 }, None=>sys::OA_ERR_DEVICE }
}
unsafe extern "C" fn close_device(selfp:*mut sys::oa_driver)->i32{ let s=&mut *(selfp as *mut Driver); s.state.stream=None; s.state.device=None; sys::OA_OK }
unsafe extern "C" fn get_default_config(selfp:*mut sys::oa_driver, out:*mut sys::oa_stream_config)->i32{
    let s = &mut *(selfp as *mut Driver);
    let dev = match &s.state.device{ Some(d)=>d, None=>return sys::OA_ERR_DEVICE };
    if let Ok(c)=dev.default_output_config(){
        (*out).sample_rate = c.sample_rate().0;
        (*out).buffer_frames = 256;
        (*out).in_channels = 0;
        (*out).out_channels = c.channels();
        (*out).format = sys::oa_sample_format::OA_SAMPLE_F32;
        (*out).layout = sys::oa_buffer_layout::OA_BUF_INTERLEAVED;
        sys::OA_OK
    } else { sys::OA_ERR_DEVICE }
}
unsafe extern "C" fn start(selfp:*mut sys::oa_driver, cfg:*const sys::oa_stream_config)->i32{
    let s = &mut *(selfp as *mut Driver);
    let dev = match &s.state.device{ Some(d)=>d.clone(), None=>return sys::OA_ERR_DEVICE };
    s.state.cfg = *cfg;
    let dc = dev.default_output_config().expect("default cfg");
    let mut sc: cpal::StreamConfig = dc.into();
    sc.channels = (*cfg).out_channels;
    sc.sample_rate = cpal::SampleRate((*cfg).sample_rate);
    sc.buffer_size = cpal::BufferSize::Default;
    let state_ptr = selfp as *mut Driver;
    let stream = dev.build_output_stream(&sc,
        move |data:&mut [f32], _| {
            let st = unsafe{ &mut *state_ptr };
            let ti = sys::oa_time_info{ host_time_ns: st.state.time0.elapsed().as_nanos() as u64, device_time_ns: 0, underruns: st.state.underruns.load(Ordering::Relaxed), overruns: st.state.overruns.load(Ordering::Relaxed)};
            if let Some(cb)=st.state.host.process{
                let frames = (data.len() / st.state.cfg.out_channels as usize) as u32;
                let _keep = unsafe{ cb(st.state.host_user, std::ptr::null(), data.as_mut_ptr() as *mut _, frames, &ti as *const _, &st.state.cfg as *const _) };
            }
        },
        move |err| { eprintln!("openasio-cpal stream error: {err}"); }, None
    ).expect("build_output_stream");
    stream.play().expect("play"); s.state.stream=Some(stream); sys::OA_OK
}
unsafe extern "C" fn stop(selfp:*mut sys::oa_driver)->i32{ let s=&mut *(selfp as *mut Driver); s.state.stream=None; sys::OA_OK }
unsafe extern "C" fn get_latency(_:*mut sys::oa_driver, in_lat:*mut u32, out_lat:*mut u32)->i32{ if !in_lat.is_null(){*in_lat=0;} if !out_lat.is_null(){*out_lat=0;} sys::OA_OK }
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
            host: *p.host, host_user: p.host_user, device: None, stream: None,
            cfg: sys::oa_stream_config{ sample_rate:48000, buffer_frames:256, in_channels:0, out_channels:2, format: sys::oa_sample_format::OA_SAMPLE_F32, layout: sys::oa_buffer_layout::OA_BUF_INTERLEAVED },
            time0: Instant::now(), underruns: AtomicU32::new(0), overruns: AtomicU32::new(0),
        },
    });
    *out = Box::into_raw(drv) as *mut sys::oa_driver; sys::OA_OK
}
#[no_mangle] pub unsafe extern "C" fn openasio_driver_destroy(driver:*mut sys::oa_driver){ if !driver.is_null(){ let _ = Box::from_raw(driver as *mut Driver); } }
