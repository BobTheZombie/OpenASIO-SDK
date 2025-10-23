//! Safe host-side wrapper for OpenASIO v1.0.0
use anyhow::{anyhow, Context, Result};
use openasio_sys as sys;
use std::ffi::{CStr, CString};
use std::os::raw::c_void;
use std::ptr::NonNull;

#[derive(Clone, Copy, Debug)]
pub struct StreamConfig {
    pub sample_rate: u32,
    pub buffer_frames: u32,
    pub in_channels: u16,
    pub out_channels: u16,
    pub interleaved: bool,
}

pub trait HostProcess: Send {
    /// Called on the driver's RT thread. Must be RT-safe.
    fn process(&mut self, inputs: *const c_void, outputs: *mut c_void, frames: u32, cfg: &StreamConfig) -> bool;
}

struct HostThunk {
    inner: Box<dyn HostProcess>,
    cfg: sys::oa_stream_config,
}

pub struct Driver {
    _lib: sys::loader::DriverLib,
    drv: NonNull<sys::oa_driver>,
    _host_thunk: Box<HostThunk>,
}

unsafe extern "C" fn cb_process(
    user: *mut c_void,
    in_ptr: *const c_void,
    out_ptr: *mut c_void,
    frames: u32,
    _time: *const sys::oa_time_info,
    cfg: *const sys::oa_stream_config,
) -> i32 {
    let ctx = &mut *(user as *mut HostThunk);
    let cfg_rust = StreamConfig {
        sample_rate: (*cfg).sample_rate,
        buffer_frames: (*cfg).buffer_frames,
        in_channels: (*cfg).in_channels,
        out_channels: (*cfg).out_channels,
        interleaved: matches!((*cfg).layout, sys::oa_buffer_layout::OA_BUF_INTERLEAVED),
    };
    if ctx.inner.process(in_ptr, out_ptr, frames, &cfg_rust) { sys::OA_TRUE } else { sys::OA_FALSE }
}
unsafe extern "C" fn cb_latency_changed(_user: *mut c_void, _in: u32, _out: u32) {}
unsafe extern "C" fn cb_reset_request(_user: *mut c_void) {}

impl Driver {
    pub fn load(path: &str, host: Box<dyn HostProcess>, default_cfg: StreamConfig, interleaved: bool) -> Result<Self> {
        unsafe {
            let lib = sys::loader::DriverLib::load(path).with_context(|| format!("dlopen({path})"))?;
            let mut drv_ptr: *mut sys::oa_driver = std::ptr::null_mut();
            let callbacks = sys::oa_host_callbacks { process: Some(cb_process), latency_changed: Some(cb_latency_changed), reset_request: Some(cb_reset_request) };
            let mut host_thunk = Box::new(HostThunk{
                inner: host,
                cfg: sys::oa_stream_config{
                    sample_rate: default_cfg.sample_rate,
                    buffer_frames: default_cfg.buffer_frames,
                    in_channels: default_cfg.in_channels,
                    out_channels: default_cfg.out_channels,
                    format: sys::oa_sample_format::OA_SAMPLE_F32,
                    layout: if interleaved { sys::oa_buffer_layout::OA_BUF_INTERLEAVED } else { sys::oa_buffer_layout::OA_BUF_NONINTERLEAVED },
                },
            });
            let params = sys::oa_create_params{ struct_size: std::mem::size_of::<sys::oa_create_params>() as u32, host: &callbacks, host_user: (&mut *host_thunk) as *mut _ as *mut c_void };
            let rc = (lib.create)(&params as *const _, &mut drv_ptr as *mut _);
            if rc < 0 || drv_ptr.is_null(){ return Err(anyhow!("openasio_driver_create rc={rc}")); }
            Ok(Self{ _lib: lib, drv: NonNull::new(drv_ptr).unwrap(), _host_thunk: host_thunk })
        }
    }
    pub fn caps(&self) -> u32 {
        unsafe { let vt = &*(*self.drv.as_ptr()).vt; (vt.get_caps.unwrap())(self.drv.as_ptr()) }
    }
    pub fn enumerate_devices(&self) -> Result<Vec<String>> {
        unsafe {
            let vt = &*(*self.drv.as_ptr()).vt;
            let mut buf = vec![0u8; 16*1024];
            let rc = (vt.query_devices.unwrap())(self.drv.as_ptr(), buf.as_mut_ptr() as *mut i8, buf.len());
            if rc < 0 { return Err(anyhow!("query_devices rc={rc}")); }
            let list = CStr::from_ptr(buf.as_ptr() as *const i8).to_string_lossy().to_string();
            Ok(list.lines().map(|s| s.to_string()).collect())
        }
    }
    pub fn open_default(&mut self) -> Result<()> { self.open_by_name(None) }
    pub fn open_by_name(&mut self, name: Option<&str>) -> Result<()> {
        unsafe {
            let vt = &*(*self.drv.as_ptr()).vt;
            let c = name.map(|s| CString::new(s).unwrap());
            let ptr = c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
            let rc = (vt.open_device.unwrap())(self.drv.as_ptr(), ptr);
            if rc < 0 { return Err(anyhow!("open_device rc={rc}")); }
            Ok(())
        }
    }
    pub fn default_config(&self) -> Result<StreamConfig> {
        unsafe {
            let vt = &*(*self.drv.as_ptr()).vt;
            let mut c = std::mem::MaybeUninit::<sys::oa_stream_config>::uninit();
            let rc = (vt.get_default_config.unwrap())(self.drv.as_ptr(), c.as_mut_ptr());
            if rc < 0 { return Err(anyhow!("get_default_config rc={rc}")); }
            let c = c.assume_init();
            Ok(StreamConfig{
                sample_rate: c.sample_rate, buffer_frames: c.buffer_frames,
                in_channels: c.in_channels, out_channels: c.out_channels,
                interleaved: matches!(c.layout, sys::oa_buffer_layout::OA_BUF_INTERLEAVED),
            })
        }
    }
    pub fn start(&mut self) -> Result<()> { unsafe { let vt = &*(*self.drv.as_ptr()).vt; (vt.start.unwrap())(self.drv.as_ptr(), &(*self._host_thunk).cfg as *const _); Ok(()) } }
    pub fn stop(&mut self) { unsafe { let vt = &*(*self.drv.as_ptr()).vt; let _=(vt.stop.unwrap())(self.drv.as_ptr()); } }
}
impl Drop for Driver { fn drop(&mut self) { unsafe { let vt=&*(*self.drv.as_ptr()).vt; let _=(vt.close_device.unwrap())(self.drv.as_ptr()); } } }
