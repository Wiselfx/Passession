use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use std::ffi::{CStr, CString, c_char, c_int};
use std::sync::LazyLock;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
static SELECTED_LIBRARY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
static LOAD_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "android")]
static ANDROID_NETWORK_MONITOR: std::sync::OnceLock<fn(bool) -> Result<()>> =
    std::sync::OnceLock::new();

#[cfg(target_os = "android")]
pub(crate) fn register_android_network_monitor(provider: fn(bool) -> Result<()>) -> Result<()> {
    ANDROID_NETWORK_MONITOR
        .set(provider)
        .map_err(|_| anyhow::anyhow!("Android network monitor was already registered"))
}

#[cfg(target_os = "android")]
fn android_network_monitor(enabled: bool) -> Result<()> {
    ANDROID_NETWORK_MONITOR
        .get()
        .context("Android network monitor is not registered")?(enabled)
}

/// The caller authenticates the immutable component-store image before startup.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) unsafe fn select_validated_library(path: std::path::PathBuf) -> Result<()> {
    anyhow::ensure!(
        !LOAD_STARTED.load(std::sync::atomic::Ordering::SeqCst),
        "Tailscale library selection cannot change after initialization"
    );
    anyhow::ensure!(
        path.is_absolute(),
        "Selected Tailscale image is not absolute"
    );
    let canonical = std::fs::canonicalize(&path)?;
    #[cfg(not(windows))]
    anyhow::ensure!(
        canonical == path,
        "Selected Tailscale image is not canonical"
    );
    SELECTED_LIBRARY
        .set(canonical)
        .map_err(|_| anyhow::anyhow!("Tailscale library already selected"))
}

fn validate_abi(actual: u32) -> Result<()> {
    anyhow::ensure!(
        actual == 1,
        "Unsupported Tailscale C ABI {actual}; expected 1"
    );
    Ok(())
}

type New = unsafe extern "C" fn(*const c_char, *const u8) -> u64;
type Call = unsafe extern "C" fn(u64, c_int) -> *mut c_char;
type Close = unsafe extern "C" fn(u64) -> *mut c_char;
type Free = unsafe extern "C" fn(*mut c_char);

pub(crate) struct Api {
    new: New,
    call: Call,
    close: Close,
    free: Free,
    identity: unsafe extern "C" fn() -> *mut c_char,
    #[cfg(target_os = "android")]
    network_update: unsafe extern "C" fn(*const c_char) -> *mut c_char,
    #[cfg(not(target_os = "ios"))]
    _library: std::mem::ManuallyDrop<libloading::Library>,
}

#[cfg(target_os = "ios")]
unsafe extern "C" {
    fn PaseoAbiVersion() -> u32;
    fn PaseoIdentity() -> *mut c_char;
    fn PaseoNew(config: *const c_char, key: *const u8) -> u64;
    fn PaseoCall(id: u64, operation: c_int) -> *mut c_char;
    fn PaseoClose(id: u64) -> *mut c_char;
    fn PaseoFree(value: *mut c_char);
}

impl Api {
    pub(crate) fn load() -> Result<&'static Self> {
        static API: LazyLock<Result<Api, String>> =
            LazyLock::new(|| Api::load_once().map_err(|e| format!("{e:#}")));
        API.as_ref().map_err(|e| anyhow::anyhow!("{e}"))
    }

    fn load_once() -> Result<Self> {
        LOAD_STARTED.store(true, std::sync::atomic::Ordering::SeqCst);
        #[cfg(target_os = "ios")]
        validate_abi(unsafe { PaseoAbiVersion() })?;
        #[cfg(target_os = "ios")]
        let api = Self {
            new: PaseoNew,
            call: PaseoCall,
            close: PaseoClose,
            free: PaseoFree,
            identity: PaseoIdentity,
        };
        #[cfg(not(target_os = "ios"))]
        let api = unsafe {
            #[cfg(target_os = "android")]
            let path = std::path::PathBuf::from("libpaseo_tailscale.so");
            #[cfg(not(target_os = "android"))]
            let path = {
                let exe = std::env::current_exe().context("locate app-owned Tailscale library")?;
                let parent = exe
                    .parent()
                    .context("application has no parent directory")?;
                #[cfg(target_os = "windows")]
                let name = "paseo_tailscale.dll";
                #[cfg(target_os = "macos")]
                let name = "libpaseo_tailscale.dylib";
                #[cfg(not(any(target_os = "windows", target_os = "macos")))]
                let name = "libpaseo_tailscale.so";
                SELECTED_LIBRARY
                    .get()
                    .cloned()
                    .unwrap_or_else(|| parent.join(name))
            };
            #[cfg(target_os = "windows")]
            let library: libloading::Library =
                libloading::os::windows::Library::load_with_flags(&path, 0x00000100 | 0x00001000)
                    .with_context(|| format!("load packaged Tailscale library {}", path.display()))?
                    .into();
            #[cfg(not(target_os = "windows"))]
            let library = libloading::Library::new(&path)
                .with_context(|| format!("load packaged Tailscale library {}", path.display()))?;
            // Go runtimes cannot be unloaded, even when no tsnet node remains.
            let library = std::mem::ManuallyDrop::new(library);
            let abi: unsafe extern "C" fn() -> u32 = *library.get(b"PaseoAbiVersion\0")?;
            validate_abi(abi())?;
            Self {
                new: *library.get(b"PaseoNew\0")?,
                call: *library.get(b"PaseoCall\0")?,
                close: *library.get(b"PaseoClose\0")?,
                free: *library.get(b"PaseoFree\0")?,
                identity: *library.get(b"PaseoIdentity\0")?,
                #[cfg(target_os = "android")]
                network_update: *library.get(b"PaseoAndroidNetworkUpdate\0")?,
                _library: library,
            }
        };
        Ok(api)
    }

    #[cfg(target_os = "android")]
    pub(crate) fn android_network_update(&self, snapshot: &str) -> Result<()> {
        let raw = CString::new(snapshot)?;
        self.decode(unsafe { (self.network_update)(raw.as_ptr()) })?;
        Ok(())
    }

    pub(crate) fn identity(&self) -> Result<Value> {
        self.decode(unsafe { (self.identity)() })
    }

    pub(crate) fn new_node(&self, config: &super::Config, key: &[u8; 32]) -> Result<u64> {
        let raw = CString::new(serde_json::to_vec(config)?)?;
        #[cfg(target_os = "android")]
        android_network_monitor(true)?;
        let handle = unsafe { (self.new)(raw.as_ptr(), key.as_ptr()) };
        if handle == 0 {
            #[cfg(target_os = "android")]
            android_network_monitor(false)?;
            bail!("native Tailscale configuration was rejected");
        }
        Ok(handle)
    }

    pub(crate) fn call(&self, handle: u64, op: i32) -> Result<Value> {
        self.decode(unsafe { (self.call)(handle, op) })
    }
    pub(crate) fn close(&self, handle: u64) -> Result<()> {
        self.decode(unsafe { (self.close)(handle) })?;
        #[cfg(target_os = "android")]
        android_network_monitor(false)?;
        Ok(())
    }
    fn decode(&self, ptr: *mut c_char) -> Result<Value> {
        if ptr.is_null() {
            bail!("native Tailscale returned an empty response");
        }
        #[derive(Deserialize)]
        struct Reply {
            #[serde(default)]
            value: Value,
            #[serde(default)]
            error: String,
        }
        let reply = unsafe { serde_json::from_slice::<Reply>(CStr::from_ptr(ptr).to_bytes()) };
        unsafe { (self.free)(ptr) };
        let reply = reply.context("decode native Tailscale response")?;
        if !reply.error.is_empty() {
            bail!("{}", reply.error);
        }
        Ok(reply.value)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_incompatible_c_abi_before_api_creation() {
        assert!(super::validate_abi(1).is_ok());
        assert!(super::validate_abi(0).is_err());
        assert!(super::validate_abi(2).is_err());
    }
}
