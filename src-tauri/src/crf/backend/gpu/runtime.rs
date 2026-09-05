//! Runtime loader for the distributable `crf_cuda.dll` sidecar.
//!
//! The CUDA implementation lives in a separate DLL so release packages use a
//! conventional `crf-viewer.exe` + `crf_cuda.dll` layout. The sidecar itself
//! dynamically loads the NVIDIA driver API and therefore still does not
//! require CUDA Toolkit or `cudart.dll` on the target machine.

use crate::crf::backend::BackendError;

#[cfg(windows)]
use std::ffi::{c_char, c_void, CString};
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::ptr;
#[cfg(windows)]
use std::sync::{Mutex, OnceLock};

#[cfg(windows)]
type DiffFn = unsafe extern "system" fn(u32, *const i32, *const i32, *mut i32, usize) -> i32;

#[cfg(windows)]
type RctForwardFn = unsafe extern "system" fn(u32, *mut i32, usize) -> i32;

#[cfg(windows)]
struct Sidecar {
    lib: *mut c_void,
    diff: DiffFn,
    rct_forward: RctForwardFn,
    rct_inverse: RctForwardFn,
}

#[cfg(windows)]
unsafe impl Send for Sidecar {}

#[cfg(windows)]
impl Drop for Sidecar {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.lib);
        }
    }
}

#[cfg(windows)]
impl Sidecar {
    unsafe fn load() -> Result<Self, BackendError> {
        let mut candidates = Vec::new();
        if let Ok(path) = std::env::var("CRF_CUDA_DLL") {
            candidates.push(PathBuf::from(path));
        }
        candidates.push(PathBuf::from("crf_cuda.dll"));
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("crf_cuda.dll"));
                // Cargo test executables live in target/{debug,release}/deps.
                if dir.file_name().is_some_and(|name| name == "deps") {
                    if let Some(target_dir) = dir.parent() {
                        candidates.push(target_dir.join("crf_cuda.dll"));
                    }
                }
            }
        }
        let mut lib = ptr::null_mut();
        for candidate in candidates {
            let Ok(raw) = CString::new(candidate.to_string_lossy().as_bytes()) else {
                continue;
            };
            lib = LoadLibraryA(raw.as_ptr().cast());
            if !lib.is_null() {
                break;
            }
        }
        if lib.is_null() {
            return Err(BackendError::DeviceError(
                "crf_cuda.dll could not be loaded".into(),
            ));
        }
        let symbol = CString::new("crf_cuda_diff_i32").unwrap();
        let address = GetProcAddress(lib, symbol.as_ptr());
        if address.is_null() {
            FreeLibrary(lib);
            return Err(BackendError::DeviceError(
                "crf_cuda_diff_i32 export is missing".into(),
            ));
        }
        let rct_symbol = CString::new("crf_cuda_rct_forward").unwrap();
        let rct_address = GetProcAddress(lib, rct_symbol.as_ptr());
        if rct_address.is_null() {
            FreeLibrary(lib);
            return Err(BackendError::DeviceError(
                "crf_cuda_rct_forward export is missing".into(),
            ));
        }
        let rct_inv_symbol = CString::new("crf_cuda_rct_inverse").unwrap();
        let rct_inv_address = GetProcAddress(lib, rct_inv_symbol.as_ptr());
        if rct_inv_address.is_null() {
            FreeLibrary(lib);
            return Err(BackendError::DeviceError(
                "crf_cuda_rct_inverse export is missing".into(),
            ));
        }
        Ok(Self {
            lib,
            diff: std::mem::transmute_copy(&address),
            rct_forward: std::mem::transmute_copy(&rct_address),
            rct_inverse: std::mem::transmute_copy(&rct_inv_address),
        })
    }
}

#[cfg(windows)]
fn with_sidecar<T>(f: impl FnOnce(&Sidecar) -> Result<T, BackendError>) -> Result<T, BackendError> {
    static SIDECAR: OnceLock<Mutex<Option<Sidecar>>> = OnceLock::new();
    let lock = SIDECAR.get_or_init(|| Mutex::new(None));
    let mut guard = lock
        .lock()
        .map_err(|_| BackendError::DeviceError("CUDA sidecar lock poisoned".into()))?;
    if guard.is_none() {
        *guard = Some(unsafe { Sidecar::load()? });
    }
    let sidecar = guard.as_ref().expect("CUDA sidecar initialized");
    f(sidecar)
}

#[cfg(windows)]
pub fn run_diff_i32(device_id: u32, a: &[i32], b: &[i32]) -> Result<Vec<i32>, BackendError> {
    if a.is_empty() {
        return Ok(Vec::new());
    }
    if a.len() != b.len() {
        return Err(BackendError::Unsupported(
            "diff inputs have different lengths",
        ));
    }
    with_sidecar(|sidecar| {
        let mut output = vec![0i32; a.len()];
        let code = unsafe {
            (sidecar.diff)(
                device_id,
                a.as_ptr(),
                b.as_ptr(),
                output.as_mut_ptr(),
                a.len(),
            )
        };
        if code == 0 {
            Ok(output)
        } else {
            Err(BackendError::DeviceError(format!(
                "crf_cuda.dll diff failed with code {code}"
            )))
        }
    })
}

/// 原地 RCT 变换的公共执行路径：校验 3 分量交织、阈值、惰性加载 sidecar、调用 kernel。
#[cfg(windows)]
fn run_inplace_rct(
    device_id: u32,
    pixels: &mut [i32],
    pick: impl Fn(&Sidecar) -> RctForwardFn,
    label: &str,
) -> Result<(), BackendError> {
    if pixels.len() % 3 != 0 {
        return Err(BackendError::Unsupported(
            "rct requires interleaved 3-component pixels",
        ));
    }
    let npix = pixels.len() / 3;
    if npix == 0 {
        return Ok(());
    }
    with_sidecar(|sidecar| {
        let code = unsafe { (pick(sidecar))(device_id, pixels.as_mut_ptr(), npix) };
        if code == 0 {
            Ok(())
        } else {
            Err(BackendError::DeviceError(format!(
                "crf_cuda.dll {label} failed with code {code}"
            )))
        }
    })
}

/// 执行 YCoCg-R 正向变换（3 分量交织，原地）。`pixels` 长度必须是 3 的倍数。
#[cfg(windows)]
pub fn run_rct_forward(device_id: u32, pixels: &mut [i32]) -> Result<(), BackendError> {
    run_inplace_rct(device_id, pixels, |s| s.rct_forward, "rct_forward")
}

/// 执行 YCoCg-R 逆向变换（3 分量交织，原地）。`pixels` 长度必须是 3 的倍数。
#[cfg(windows)]
pub fn run_rct_inverse(device_id: u32, pixels: &mut [i32]) -> Result<(), BackendError> {
    run_inplace_rct(device_id, pixels, |s| s.rct_inverse, "rct_inverse")
}

#[cfg(not(windows))]
pub fn run_diff_i32(_device_id: u32, _a: &[i32], _b: &[i32]) -> Result<Vec<i32>, BackendError> {
    Err(BackendError::Unsupported(
        "NVIDIA CUDA sidecar is only available on Windows",
    ))
}

#[cfg(not(windows))]
pub fn run_rct_forward(_device_id: u32, _pixels: &mut [i32]) -> Result<(), BackendError> {
    Err(BackendError::Unsupported(
        "NVIDIA CUDA sidecar is only available on Windows",
    ))
}

#[cfg(not(windows))]
pub fn run_rct_inverse(_device_id: u32, _pixels: &mut [i32]) -> Result<(), BackendError> {
    Err(BackendError::Unsupported(
        "NVIDIA CUDA sidecar is only available on Windows",
    ))
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(name: *const c_char) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}
