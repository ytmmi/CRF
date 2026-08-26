//! Minimal CUDA Driver API runtime used by the optional `nvidia-cuda` feature.

use crate::crf::backend::BackendError;
use std::ffi::{c_char, c_void, CString};
use std::ptr;

type R = i32;
type Dev = i32;
type DevPtr = u64;
type Handle = *mut c_void;
const OK: R = 0;

const PTX: &[u8] = br#".version 7.0
.target sm_50
.address_size 64
.visible .entry diff_i32(.param .u64 a,.param .u64 b,.param .u64 out,.param .u32 n) {
 .reg .pred %p; .reg .b32 %r<7>; .reg .b64 %rd<8>;
 ld.param.u64 %rd1,[a]; ld.param.u64 %rd2,[b]; ld.param.u64 %rd3,[out]; ld.param.u32 %r1,[n];
 mov.u32 %r2,%tid.x; mov.u32 %r3,%ctaid.x; mov.u32 %r4,%ntid.x; mad.lo.u32 %r5,%r3,%r4,%r2;
 setp.ge.u32 %p,%r5,%r1; @%p bra DONE; mul.wide.u32 %rd4,%r5,4;
 add.u64 %rd5,%rd1,%rd4; add.u64 %rd6,%rd2,%rd4; add.u64 %rd7,%rd3,%rd4;
 ld.global.s32 %r6,[%rd5]; ld.global.s32 %r2,[%rd6]; sub.s32 %r6,%r6,%r2; st.global.s32 [%rd7],%r6;
DONE: ret;
}
"#;

#[cfg(windows)]
struct Driver {
    lib: *mut c_void,
    init: unsafe extern "system" fn(u32) -> R,
    device_get: unsafe extern "system" fn(*mut Dev, i32) -> R,
    ctx_create: unsafe extern "system" fn(*mut Handle, u32, Dev) -> R,
    ctx_destroy: unsafe extern "system" fn(Handle) -> R,
    ctx_set_current: unsafe extern "system" fn(Handle) -> R,
    module_load: unsafe extern "system" fn(*mut Handle, *const c_void) -> R,
    module_unload: unsafe extern "system" fn(Handle) -> R,
    function_get: unsafe extern "system" fn(*mut Handle, Handle, *const c_char) -> R,
    alloc: unsafe extern "system" fn(*mut DevPtr, usize) -> R,
    free: unsafe extern "system" fn(DevPtr) -> R,
    hto_d: unsafe extern "system" fn(DevPtr, *const c_void, usize) -> R,
    dto_h: unsafe extern "system" fn(*mut c_void, DevPtr, usize) -> R,
    launch: unsafe extern "system" fn(
        Handle,
        u32,
        u32,
        u32,
        u32,
        u32,
        u32,
        u32,
        *mut c_void,
        *mut *mut c_void,
        *mut *mut c_void,
    ) -> R,
    sync: unsafe extern "system" fn() -> R,
}

#[cfg(windows)]
unsafe fn sym<T: Copy>(lib: *mut c_void, name: &str) -> Result<T, BackendError> {
    let c = CString::new(name).unwrap();
    let p = GetProcAddress(lib, c.as_ptr());
    if p.is_null() {
        Err(BackendError::DeviceError(format!(
            "missing CUDA symbol {name}"
        )))
    } else {
        Ok(std::mem::transmute_copy(&p))
    }
}

#[cfg(windows)]
impl Driver {
    unsafe fn load() -> Result<Self, BackendError> {
        let lib = LoadLibraryA(b"nvcuda.dll\0".as_ptr().cast());
        if lib.is_null() {
            return Err(BackendError::DeviceError(
                "nvcuda.dll could not be loaded".into(),
            ));
        }
        let loaded = (|| {
            Ok(Self {
                lib,
                init: sym(lib, "cuInit")?,
                device_get: sym(lib, "cuDeviceGet")?,
                ctx_create: sym(lib, "cuCtxCreate_v2")?,
                ctx_destroy: sym(lib, "cuCtxDestroy_v2")?,
                ctx_set_current: sym(lib, "cuCtxSetCurrent")?,
                module_load: sym(lib, "cuModuleLoadData")?,
                module_unload: sym(lib, "cuModuleUnload")?,
                function_get: sym(lib, "cuModuleGetFunction")?,
                alloc: sym(lib, "cuMemAlloc_v2")?,
                free: sym(lib, "cuMemFree_v2")?,
                hto_d: sym(lib, "cuMemcpyHtoD_v2")?,
                dto_h: sym(lib, "cuMemcpyDtoH_v2")?,
                launch: sym(lib, "cuLaunchKernel")?,
                sync: sym(lib, "cuCtxSynchronize")?,
            })
        })();
        match loaded {
            Ok(driver) => Ok(driver),
            Err(error) => {
                FreeLibrary(lib);
                Err(error)
            }
        }
    }
}

#[cfg(windows)]
impl Drop for Driver {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.lib);
        }
    }
}

#[cfg(windows)]
fn check(code: R, op: &str) -> Result<(), BackendError> {
    if code == OK {
        Ok(())
    } else {
        Err(BackendError::DeviceError(format!(
            "{op} failed with CUDA error {code}"
        )))
    }
}

#[cfg(windows)]
struct Session {
    driver: Driver,
    context: Handle,
    module: Handle,
    function: Handle,
}

// CUDA handles are process-owned opaque values. Access to the cached session
// is serialized by the mutex below, so moving it between worker threads is
// safe as long as each call makes its context current first.
#[cfg(windows)]
unsafe impl Send for Driver {}

#[cfg(windows)]
unsafe impl Send for Session {}

#[cfg(windows)]
impl Session {
    unsafe fn new(device_id: u32) -> Result<Self, BackendError> {
        let driver = Driver::load()?;
        let mut session = Self {
            driver,
            context: ptr::null_mut(),
            module: ptr::null_mut(),
            function: ptr::null_mut(),
        };
        check((session.driver.init)(0), "cuInit")?;
        let mut device = 0;
        check(
            (session.driver.device_get)(&mut device, device_id as i32),
            "cuDeviceGet",
        )?;
        check(
            (session.driver.ctx_create)(&mut session.context, 0, device),
            "cuCtxCreate",
        )?;
        let mut ptx = PTX.to_vec();
        ptx.push(0);
        check(
            (session.driver.module_load)(&mut session.module, ptx.as_ptr().cast()),
            "cuModuleLoadData",
        )?;
        let name = CString::new("diff_i32").unwrap();
        check(
            (session.driver.function_get)(&mut session.function, session.module, name.as_ptr()),
            "cuModuleGetFunction",
        )?;
        Ok(session)
    }
}

#[cfg(windows)]
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            if !self.module.is_null() {
                let _ = (self.driver.module_unload)(self.module);
                self.module = ptr::null_mut();
            }
            if !self.context.is_null() {
                let _ = (self.driver.ctx_destroy)(self.context);
                self.context = ptr::null_mut();
            }
        }
    }
}

#[cfg(windows)]
pub fn run_diff_i32(device_id: u32, a: &[i32], b: &[i32]) -> Result<Vec<i32>, BackendError> {
    if a.is_empty() {
        return Ok(Vec::new());
    }
    use std::sync::{Mutex, OnceLock};
    static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
    let lock = SESSION.get_or_init(|| Mutex::new(None));
    let mut guard = lock
        .lock()
        .map_err(|_| BackendError::DeviceError("CUDA session lock poisoned".into()))?;
    if guard.is_none() {
        *guard = Some(unsafe { Session::new(device_id)? });
    }
    let session = guard.as_mut().expect("CUDA session initialized");
    unsafe {
        check(
            (session.driver.ctx_set_current)(session.context),
            "cuCtxSetCurrent",
        )?;
    }
    unsafe { execute_diff(&session.driver, session.function, a, b) }
}

#[cfg(windows)]
unsafe fn execute_diff(
    d: &Driver,
    fun: Handle,
    a: &[i32],
    b: &[i32],
) -> Result<Vec<i32>, BackendError> {
    let bytes = a.len().checked_mul(4).ok_or(BackendError::AllocFailed)?;
    let mut da = 0;
    let mut db = 0;
    let mut out = 0;
    let result = (|| {
        check((d.alloc)(&mut da, bytes), "cuMemAlloc(a)")?;
        check((d.alloc)(&mut db, bytes), "cuMemAlloc(b)")?;
        check((d.alloc)(&mut out, bytes), "cuMemAlloc(out)")?;
        check((d.hto_d)(da, a.as_ptr().cast(), bytes), "cuMemcpyHtoD(a)")?;
        check((d.hto_d)(db, b.as_ptr().cast(), bytes), "cuMemcpyHtoD(b)")?;
        let mut len = a.len() as u32;
        let mut args: [*mut c_void; 4] = [
            (&mut da as *mut DevPtr).cast(),
            (&mut db as *mut DevPtr).cast(),
            (&mut out as *mut DevPtr).cast(),
            (&mut len as *mut u32).cast(),
        ];
        let block = 256u32;
        let grid = ((len + block - 1) / block).max(1);
        check(
            (d.launch)(
                fun,
                grid,
                1,
                1,
                block,
                1,
                1,
                0,
                ptr::null_mut(),
                args.as_mut_ptr(),
                ptr::null_mut(),
            ),
            "cuLaunchKernel",
        )?;
        check((d.sync)(), "cuCtxSynchronize")?;
        let mut result = vec![0i32; a.len()];
        check(
            (d.dto_h)(result.as_mut_ptr().cast(), out, bytes),
            "cuMemcpyDtoH",
        )?;
        Ok(result)
    })();
    if da != 0 {
        let _ = (d.free)(da);
    }
    if db != 0 {
        let _ = (d.free)(db);
    }
    if out != 0 {
        let _ = (d.free)(out);
    }
    result
}

#[cfg(not(windows))]
pub fn run_diff_i32(_device_id: u32, _a: &[i32], _b: &[i32]) -> Result<Vec<i32>, BackendError> {
    Err(BackendError::Unsupported(
        "CUDA Driver API runtime is only implemented on Windows",
    ))
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(name: *const c_char) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}
