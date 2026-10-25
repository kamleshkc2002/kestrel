//! Runtime-loaded NVIDIA Management Library adapter.
//!
//! NVML is optional on the host, so this module resolves its shared library and
//! symbols at runtime and keeps the platform crate linkable without NVML.

use std::ffi::CString;
use std::os::raw::{c_char, c_uint, c_void};
use std::ptr;

/// Shared library used by NVML on Linux.
pub const NVML_LIBRARY: &str = "libnvidia-ml.so.1";
/// Maximum number of GPUs sampled by one call.
pub const MAX_NVML_GPUS: usize = 8;

const NAME_BUFFER_SIZE: usize = 96;
const NVML_SUCCESS: i32 = 0;
const NVML_ERROR_NOT_SUPPORTED: i32 = 3;
const NVML_ERROR_NO_PERMISSION: i32 = 4;
const NVML_ERROR_DRIVER_NOT_LOADED: i32 = 9;
const NVML_ERROR_LIBRARY_NOT_FOUND: i32 = 12;
const NVML_ERROR_LIB_RM_VERSION_MISMATCH: i32 = 18;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvmlGpu {
    pub index: u32,
    /// Device name, bounded to 96 UTF-8 bytes at a character boundary.
    pub name: String,
    /// GPU utilization, clamped to 100 percent.
    pub utilization_percent: u8,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NvmlUnavailable {
    LibraryMissing,
    DriverNotLoaded,
    VersionMismatch,
    NoPermission,
    NotSupported,
    NoDevices,
    Failed { function: &'static str, code: i32 },
}

impl NvmlUnavailable {
    pub fn reason(&self) -> String {
        match self {
            Self::LibraryMissing => "NVML (libnvidia-ml.so.1) is not installed".to_string(),
            Self::DriverNotLoaded => "the NVIDIA driver is not loaded".to_string(),
            Self::VersionMismatch => {
                "the NVIDIA kernel driver and NVML library versions differ".to_string()
            }
            Self::NoPermission => "permission to query NVIDIA GPU state was denied".to_string(),
            Self::NotSupported => "NVML does not support this operation".to_string(),
            Self::NoDevices => "no NVIDIA GPUs are available".to_string(),
            Self::Failed { function, code } => {
                format!("NVML function {function} failed with return code {code}")
            }
        }
    }

    pub fn remediation(&self) -> String {
        match self {
            Self::LibraryMissing => {
                "Install the NVIDIA proprietary driver utilities to read NVIDIA GPU activity."
                    .to_string()
            }
            Self::DriverNotLoaded => {
                "Load the NVIDIA driver, then refresh GPU monitoring.".to_string()
            }
            Self::VersionMismatch => {
                "Reboot after an NVIDIA driver update so the loaded driver matches the library."
                    .to_string()
            }
            Self::NoPermission => {
                "Check NVIDIA device permissions for this user, then refresh GPU monitoring."
                    .to_string()
            }
            Self::NotSupported => "Use a driver and GPU that support NVML monitoring.".to_string(),
            Self::NoDevices => {
                "Connect or enable an NVIDIA GPU, then refresh GPU monitoring.".to_string()
            }
            Self::Failed { .. } => {
                "Check the NVIDIA driver installation and retry GPU monitoring.".to_string()
            }
        }
    }
}

#[repr(C)]
struct NvmlUtilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

type NvmlDevice = *mut c_void;
type InitFn = unsafe extern "C" fn() -> c_uint;
type ShutdownFn = unsafe extern "C" fn() -> c_uint;
type GetCountFn = unsafe extern "C" fn(*mut c_uint) -> c_uint;
type GetHandleFn = unsafe extern "C" fn(c_uint, *mut NvmlDevice) -> c_uint;
type GetNameFn = unsafe extern "C" fn(NvmlDevice, *mut c_char, c_uint) -> c_uint;
type GetUtilizationFn = unsafe extern "C" fn(NvmlDevice, *mut NvmlUtilization) -> c_uint;
type GetMemoryFn = unsafe extern "C" fn(NvmlDevice, *mut NvmlMemory) -> c_uint;

pub struct Nvml {
    library: *mut c_void,
    shutdown: ShutdownFn,
    get_count: GetCountFn,
    get_handle: GetHandleFn,
    get_name: GetNameFn,
    get_utilization: GetUtilizationFn,
    get_memory: GetMemoryFn,
}

// SAFETY: NVML's initialized context, device handles, and resolved function pointers remain
// valid when this owner is moved to another thread. Nvml is not Sync, so this permits ownership
// transfer without permitting shared access.
unsafe impl Send for Nvml {}

impl Nvml {
    pub fn open() -> Result<Self, NvmlUnavailable> {
        Self::open_library(NVML_LIBRARY)
    }

    pub fn open_library(name: &str) -> Result<Self, NvmlUnavailable> {
        let name = CString::new(name).map_err(|_| NvmlUnavailable::LibraryMissing)?;
        // SAFETY: `name` is a valid NUL-terminated library name and these flags request
        // immediate symbol resolution with a handle local to this process.
        let library = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if library.is_null() {
            // SAFETY: `dlerror` reads and clears this thread's loader error state.
            let _ = unsafe { libc::dlerror() };
            return Err(NvmlUnavailable::LibraryMissing);
        }

        match Self::from_library(library) {
            Ok(nvml) => Ok(nvml),
            Err(error) => {
                close_library(library);
                Err(error)
            }
        }
    }

    fn from_library(library: *mut c_void) -> Result<Self, NvmlUnavailable> {
        let init: InitFn = load_symbol(library, b"nvmlInit_v2\0", "nvmlInit_v2")?;
        let shutdown: ShutdownFn = load_symbol(library, b"nvmlShutdown\0", "nvmlShutdown")?;
        let get_count: GetCountFn =
            load_symbol(library, b"nvmlDeviceGetCount_v2\0", "nvmlDeviceGetCount_v2")?;
        let get_handle: GetHandleFn = load_symbol(
            library,
            b"nvmlDeviceGetHandleByIndex_v2\0",
            "nvmlDeviceGetHandleByIndex_v2",
        )?;
        let get_name: GetNameFn =
            load_symbol(library, b"nvmlDeviceGetName\0", "nvmlDeviceGetName")?;
        let get_utilization: GetUtilizationFn = load_symbol(
            library,
            b"nvmlDeviceGetUtilizationRates\0",
            "nvmlDeviceGetUtilizationRates",
        )?;
        let get_memory: GetMemoryFn = load_symbol(
            library,
            b"nvmlDeviceGetMemoryInfo\0",
            "nvmlDeviceGetMemoryInfo",
        )?;

        // SAFETY: `init` was obtained from the loaded NVML library with the exact C ABI.
        let code = unsafe { init() } as i32;
        if code != NVML_SUCCESS {
            return Err(map_init_code(code));
        }

        Ok(Self {
            library,
            shutdown,
            get_count,
            get_handle,
            get_name,
            get_utilization,
            get_memory,
        })
    }

    pub fn sample(&self) -> Result<Vec<NvmlGpu>, NvmlUnavailable> {
        let mut count = 0_u32;
        // SAFETY: `self.get_count` is a resolved NVML function and `count` is writable storage.
        let code = unsafe { (self.get_count)(&mut count) } as i32;
        if code != NVML_SUCCESS {
            return Err(map_api_code("nvmlDeviceGetCount_v2", code));
        }
        if count == 0 {
            return Err(NvmlUnavailable::NoDevices);
        }

        let mut devices = Vec::with_capacity((count as usize).min(MAX_NVML_GPUS));
        for index in 0..count.min(MAX_NVML_GPUS as u32) {
            let mut device: NvmlDevice = ptr::null_mut();
            // SAFETY: `device` is writable storage and the index is within NVML's reported count.
            let code = unsafe { (self.get_handle)(index, &mut device) } as i32;
            if code != NVML_SUCCESS {
                return Err(map_api_code("nvmlDeviceGetHandleByIndex_v2", code));
            }

            let mut utilization = NvmlUtilization { gpu: 0, memory: 0 };
            // SAFETY: `device` came from NVML and `utilization` is writable storage.
            let code = unsafe { (self.get_utilization)(device, &mut utilization) } as i32;
            if code == NVML_ERROR_NOT_SUPPORTED {
                continue;
            }
            if code != NVML_SUCCESS {
                return Err(map_api_code("nvmlDeviceGetUtilizationRates", code));
            }

            let mut memory = NvmlMemory {
                total: 0,
                free: 0,
                used: 0,
            };
            // SAFETY: `device` came from NVML and `memory` is writable storage.
            let code = unsafe { (self.get_memory)(device, &mut memory) } as i32;
            if code == NVML_ERROR_NOT_SUPPORTED {
                continue;
            }
            if code != NVML_SUCCESS {
                return Err(map_api_code("nvmlDeviceGetMemoryInfo", code));
            }

            let mut name_buffer = [0_u8; NAME_BUFFER_SIZE];
            // SAFETY: `device` came from NVML; the buffer is writable and its length is exact.
            let code = unsafe {
                (self.get_name)(
                    device,
                    name_buffer.as_mut_ptr().cast::<c_char>(),
                    NAME_BUFFER_SIZE as c_uint,
                )
            } as i32;
            if code != NVML_SUCCESS {
                return Err(map_api_code("nvmlDeviceGetName", code));
            }

            let name_end = name_buffer
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(NAME_BUFFER_SIZE);
            let name = truncate_name(&String::from_utf8_lossy(&name_buffer[..name_end]));
            devices.push(NvmlGpu {
                index,
                name,
                utilization_percent: utilization.gpu.min(100) as u8,
                memory_used_bytes: memory.used,
                memory_total_bytes: memory.total,
            });
        }

        if devices.is_empty() {
            Err(NvmlUnavailable::NoDevices)
        } else {
            Ok(devices)
        }
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: NVML was initialized successfully for this handle and this is its owner.
        let _ = unsafe { (self.shutdown)() };
        close_library(self.library);
    }
}

fn load_symbol<T>(
    library: *mut c_void,
    symbol: &'static [u8],
    function: &'static str,
) -> Result<T, NvmlUnavailable> {
    // SAFETY: `dlerror` clears any earlier loader error for this thread.
    let _ = unsafe { libc::dlerror() };
    // SAFETY: `library` is a live dlopen handle and `symbol` is NUL-terminated static storage.
    let pointer = unsafe { libc::dlsym(library, symbol.as_ptr().cast::<c_char>()) };
    // SAFETY: `dlerror` reads the result of the preceding dlsym call.
    let error = unsafe { libc::dlerror() };
    if pointer.is_null() || !error.is_null() {
        return Err(NvmlUnavailable::Failed { function, code: -1 });
    }
    // SAFETY: Each call site supplies the C function-pointer type declared by NVML for this symbol.
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&pointer) })
}

fn close_library(library: *mut c_void) {
    // SAFETY: `library` is a handle returned by dlopen and is closed exactly once.
    unsafe {
        libc::dlclose(library);
    }
}

fn map_init_code(code: i32) -> NvmlUnavailable {
    match code {
        NVML_ERROR_DRIVER_NOT_LOADED => NvmlUnavailable::DriverNotLoaded,
        NVML_ERROR_LIB_RM_VERSION_MISMATCH => NvmlUnavailable::VersionMismatch,
        NVML_ERROR_NO_PERMISSION => NvmlUnavailable::NoPermission,
        NVML_ERROR_NOT_SUPPORTED => NvmlUnavailable::NotSupported,
        NVML_ERROR_LIBRARY_NOT_FOUND => NvmlUnavailable::LibraryMissing,
        code => NvmlUnavailable::Failed {
            function: "nvmlInit_v2",
            code,
        },
    }
}

fn map_api_code(function: &'static str, code: i32) -> NvmlUnavailable {
    match code {
        NVML_ERROR_DRIVER_NOT_LOADED => NvmlUnavailable::DriverNotLoaded,
        NVML_ERROR_LIB_RM_VERSION_MISMATCH => NvmlUnavailable::VersionMismatch,
        NVML_ERROR_NO_PERMISSION => NvmlUnavailable::NoPermission,
        NVML_ERROR_NOT_SUPPORTED => NvmlUnavailable::NotSupported,
        NVML_ERROR_LIBRARY_NOT_FOUND => NvmlUnavailable::LibraryMissing,
        code => NvmlUnavailable::Failed { function, code },
    }
}

fn truncate_name(name: &str) -> String {
    if name.len() <= NAME_BUFFER_SIZE {
        return name.to_string();
    }
    let mut end = NAME_BUFFER_SIZE;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::{NAME_BUFFER_SIZE, Nvml, NvmlUnavailable, map_init_code, truncate_name};

    #[test]
    fn missing_library_is_reported_without_hardware() {
        assert_eq!(
            Nvml::open_library("libkestrel-missing-nvml.so").err(),
            Some(NvmlUnavailable::LibraryMissing)
        );
    }

    #[test]
    fn init_codes_map_to_typed_unavailability() {
        for (code, expected) in [
            (9, NvmlUnavailable::DriverNotLoaded),
            (18, NvmlUnavailable::VersionMismatch),
            (4, NvmlUnavailable::NoPermission),
            (3, NvmlUnavailable::NotSupported),
            (
                1234,
                NvmlUnavailable::Failed {
                    function: "nvmlInit_v2",
                    code: 1234,
                },
            ),
        ] {
            assert_eq!(map_init_code(code), expected, "NVML code {code}");
        }
    }

    #[test]
    fn name_truncation_keeps_utf8_character_boundary() {
        let name = "x".repeat(NAME_BUFFER_SIZE - 1) + "é";
        let truncated = truncate_name(&name);
        assert_eq!(truncated.len(), NAME_BUFFER_SIZE - 1);
        assert!(truncated.is_char_boundary(truncated.len()));
        assert!(name.starts_with(&truncated));
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn nvml_is_send() {
        assert_send::<Nvml>();
    }
}
