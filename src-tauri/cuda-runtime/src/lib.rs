#![allow(non_snake_case)]

//! Sidecar CUDA backend loaded by `crf-viewer` at runtime.
//!
//! The DLL uses only the NVIDIA Driver API and embeds the small PTX kernel.
//! It therefore does not link against CUDA Toolkit or require `cudart.dll`
//! on the target machine. The application distributes this DLL next to its
//! executable and falls back to CPU when it cannot be loaded or executed.

use std::ffi::{c_char, c_void, CString};
use std::ptr;
use std::sync::{Mutex, OnceLock};

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
unsafe impl Send for Driver {}

#[cfg(windows)]
unsafe fn sym<T: Copy>(lib: *mut c_void, name: &str) -> Result<T, R> {
    let c = CString::new(name).expect("CUDA symbol names contain no NUL");
    let p = GetProcAddress(lib, c.as_ptr());
    if p.is_null() {
        Err(-1)
    } else {
        Ok(std::mem::transmute_copy(&p))
    }
}

#[cfg(windows)]
impl Driver {
    unsafe fn load() -> Result<Self, R> {
        let lib = LoadLibraryA(b"nvcuda.dll\0".as_ptr().cast());
        if lib.is_null() {
            return Err(-1);
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
struct Session {
    driver: Driver,
    context: Handle,
    module: Handle,
    function: Handle,
}

#[cfg(windows)]
unsafe impl Send for Session {}

#[cfg(windows)]
impl Session {
    unsafe fn new(device_id: u32) -> Result<Self, R> {
        let driver = Driver::load()?;
        let mut session = Self {
            driver,
            context: ptr::null_mut(),
            module: ptr::null_mut(),
            function: ptr::null_mut(),
        };
        if (session.driver.init)(0) != OK {
            return Err(-1);
        }
        let mut device = 0;
        if (session.driver.device_get)(&mut device, device_id as i32) != OK {
            return Err(-1);
        }
        if (session.driver.ctx_create)(&mut session.context, 0, device) != OK {
            return Err(-1);
        }
        let mut ptx = PTX.to_vec();
        ptx.push(0);
        if (session.driver.module_load)(&mut session.module, ptx.as_ptr().cast()) != OK {
            return Err(-1);
        }
        let name = CString::new("diff_i32").expect("kernel name contains no NUL");
        if (session.driver.function_get)(&mut session.function, session.module, name.as_ptr()) != OK
        {
            return Err(-1);
        }
        Ok(session)
    }
}

#[cfg(windows)]
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            if !self.module.is_null() {
                let _ = (self.driver.module_unload)(self.module);
            }
            if !self.context.is_null() {
                let _ = (self.driver.ctx_destroy)(self.context);
            }
        }
    }
}

#[cfg(windows)]
unsafe fn execute(
    session: &Session,
    a: *const i32,
    b: *const i32,
    out: *mut i32,
    len: usize,
) -> Result<(), R> {
    if (session.driver.ctx_set_current)(session.context) != OK {
        return Err(-1);
    }
    let bytes = len.checked_mul(4).ok_or(-1)?;
    let a_host = std::slice::from_raw_parts(a, len);
    let b_host = std::slice::from_raw_parts(b, len);
    let mut da = 0;
    let mut db = 0;
    let mut d_out = 0;
    let result = (|| {
        if (session.driver.alloc)(&mut da, bytes) != OK {
            return Err(-1);
        }
        if (session.driver.alloc)(&mut db, bytes) != OK {
            return Err(-1);
        }
        if (session.driver.alloc)(&mut d_out, bytes) != OK {
            return Err(-1);
        }
        if (session.driver.hto_d)(da, a_host.as_ptr().cast(), bytes) != OK
            || (session.driver.hto_d)(db, b_host.as_ptr().cast(), bytes) != OK
        {
            return Err(-1);
        }
        let mut n = len as u32;
        let mut args: [*mut c_void; 4] = [
            (&mut da as *mut DevPtr).cast(),
            (&mut db as *mut DevPtr).cast(),
            (&mut d_out as *mut DevPtr).cast(),
            (&mut n as *mut u32).cast(),
        ];
        let block = 256u32;
        let grid = ((n + block - 1) / block).max(1);
        if (session.driver.launch)(
            session.function,
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
        ) != OK
            || (session.driver.sync)() != OK
        {
            return Err(-1);
        }
        if (session.driver.dto_h)(out.cast(), d_out, bytes) != OK {
            return Err(-1);
        }
        Ok(())
    })();
    if da != 0 {
        let _ = (session.driver.free)(da);
    }
    if db != 0 {
        let _ = (session.driver.free)(db);
    }
    if d_out != 0 {
        let _ = (session.driver.free)(d_out);
    }
    result
}

/// Executes `out[i] = a[i] - b[i]` using the cached CUDA session.
/// Returns zero on success and a non-zero code on any driver/device error.
#[no_mangle]
pub unsafe extern "system" fn crf_cuda_diff_i32(
    device_id: u32,
    a: *const i32,
    b: *const i32,
    out: *mut i32,
    len: usize,
) -> R {
    if len == 0 {
        return OK;
    }
    if a.is_null() || b.is_null() || out.is_null() || len > u32::MAX as usize {
        return -2;
    }

    #[cfg(windows)]
    {
        static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
        let lock = SESSION.get_or_init(|| Mutex::new(None));
        let Ok(mut guard) = lock.lock() else {
            return -3;
        };
        if guard.is_none() {
            *guard = match Session::new(device_id) {
                Ok(session) => Some(session),
                Err(error) => return error,
            };
        }
        let Some(session) = guard.as_ref() else {
            return -3;
        };
        return execute(session, a, b, out, len).map_or(-1, |_| OK);
    }

    #[cfg(not(windows))]
    {
        let _ = (device_id, a, b, out, len);
        -4
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(name: *const c_char) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}
