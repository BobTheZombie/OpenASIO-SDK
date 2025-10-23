//! Raw FFI for OpenASIO v1.0.0
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]
use std::os::raw::{c_char, c_int, c_void};

pub const OA_VERSION_MAJOR: u32 = 1;
pub const OA_VERSION_MINOR: u32 = 0;
pub const OA_VERSION_PATCH: u32 = 0;

pub type oa_bool = i32;
pub const OA_FALSE: oa_bool = 0;
pub const OA_TRUE: oa_bool = 1;

pub type oa_result = i32;
pub const OA_OK: oa_result = 0;
pub const OA_ERR_GENERIC: oa_result = -1;
pub const OA_ERR_UNSUPPORTED: oa_result = -2;
pub const OA_ERR_INVALID_ARG: oa_result = -3;
pub const OA_ERR_DEVICE: oa_result = -4;
pub const OA_ERR_BACKEND: oa_result = -5;
pub const OA_ERR_STATE: oa_result = -6;

pub const OA_CAP_OUTPUT: u32 = 1<<0;
pub const OA_CAP_INPUT: u32 = 1<<1;
pub const OA_CAP_FULL_DUPLEX: u32 = 1<<2;
pub const OA_CAP_SET_SAMPLERATE: u32 = 1<<3;
pub const OA_CAP_SET_BUFFRAMES: u32 = 1<<4;

#[repr(C)] #[derive(Clone, Copy, Debug)]
pub enum oa_sample_format { OA_SAMPLE_F32 = 1, OA_SAMPLE_I16 = 2 }

#[repr(C)] #[derive(Clone, Copy, Debug)]
pub enum oa_buffer_layout { OA_BUF_INTERLEAVED = 1, OA_BUF_NONINTERLEAVED = 2 }

#[repr(C)] #[derive(Clone, Copy)]
pub struct oa_stream_config {
    pub sample_rate: u32,
    pub buffer_frames: u32,
    pub in_channels: u16,
    pub out_channels: u16,
    pub format: oa_sample_format,
    pub layout: oa_buffer_layout,
}

#[repr(C)] #[derive(Clone, Copy)]
pub struct oa_time_info {
    pub host_time_ns: u64, pub device_time_ns: u64, pub underruns: u32, pub overruns: u32,
}

#[repr(C)] #[derive(Clone, Copy)]
pub struct oa_host_callbacks {
    pub process: Option<unsafe extern "C" fn(user:*mut c_void,in_ptr:*const c_void,out_ptr:*mut c_void,frames:u32,time:*const oa_time_info,cfg:*const oa_stream_config)->oa_bool>,
    pub latency_changed: Option<unsafe extern "C" fn(user:*mut c_void,in_latency:u32,out_latency:u32)>,
    pub reset_request: Option<unsafe extern "C" fn(user:*mut c_void)>,
}

#[repr(C)] pub struct oa_create_params { pub struct_size:u32, pub host:*const oa_host_callbacks, pub host_user:*mut c_void }

#[repr(C)]
pub struct oa_driver_vtable {
    pub struct_size: u32,
    pub get_caps: Option<unsafe extern "C" fn(*mut oa_driver)->u32>,
    pub query_devices: Option<unsafe extern "C" fn(*mut oa_driver,*mut c_char,usize)->i32>,
    pub open_device: Option<unsafe extern "C" fn(*mut oa_driver,*const i8)->i32>,
    pub close_device: Option<unsafe extern "C" fn(*mut oa_driver)->i32>,
    pub get_default_config: Option<unsafe extern "C" fn(*mut oa_driver,*mut oa_stream_config)->i32>,
    pub start: Option<unsafe extern "C" fn(*mut oa_driver,*const oa_stream_config)->i32>,
    pub stop: Option<unsafe extern "C" fn(*mut oa_driver)->i32>,
    pub get_latency: Option<unsafe extern "C" fn(*mut oa_driver,*mut u32,*mut u32)->i32>,
    pub set_sample_rate: Option<unsafe extern "C" fn(*mut oa_driver,u32)->i32>,
    pub set_buffer_frames: Option<unsafe extern "C" fn(*mut oa_driver,u32)->i32>,
}

#[repr(C)] pub struct oa_driver { pub vt: *const oa_driver_vtable }

pub type openasio_driver_create_fn = unsafe extern "C" fn(params:*const oa_create_params,out:*mut *mut oa_driver)->c_int;
pub type openasio_driver_destroy_fn = unsafe extern "C" fn(driver:*mut oa_driver);

pub mod loader {
    use super::*; use libloading::{Library, Symbol};
    pub struct DriverLib { pub lib: Library, pub create: openasio_driver_create_fn, pub destroy: openasio_driver_destroy_fn }
    impl DriverLib {
        pub unsafe fn load(path:&str)->Result<Self,libloading::Error>{
            let lib = Library::new(path)?;
            let create = {
                let symbol: Symbol<openasio_driver_create_fn> = lib.get(b"openasio_driver_create\0")?;
                *symbol
            };
            let destroy = {
                let symbol: Symbol<openasio_driver_destroy_fn> = lib.get(b"openasio_driver_destroy\0")?;
                *symbol
            };
            Ok(Self{lib,create,destroy})
        }
    }
}
