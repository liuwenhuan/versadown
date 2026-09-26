use super::runtime::{is_supported_axdl_fingerprint, sha256_file, LinuxXunleiRuntime};
use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_LAZY};
use std::{
    ffi::{c_char, c_void, CString},
    path::Path,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const REQUIRED_AXDL_SYMBOLS: &[&[u8]] = &[
    b"_Z9AXDL_InitP30TAG_XL_DOWNLOAD_LIB_INIT_PARAMPKcPFiiPvES3_\0",
    b"_Z11AXDL_Uninitv\0",
    b"_Z16AXDL_SetUserInfoPKcS0_\0",
    b"_Z19AXDL_CreateP2spTaskPKcS0_S0_S0_S0_Pl\0",
    b"_Z14AXDL_StartTaskl\0",
    b"_Z13AXDL_StopTaskl\0",
    b"_Z15AXDL_DeleteTaskl\0",
    b"_Z18AXDL_QueryTaskInfolPFiliiPK19TAG_XL_TASK_INFO_EXPvES2_\0",
];

const AXDL_INIT: &[u8] = b"_Z9AXDL_InitP30TAG_XL_DOWNLOAD_LIB_INIT_PARAMPKcPFiiPvES3_\0";
const AXDL_UNINIT: &[u8] = b"_Z11AXDL_Uninitv\0";
const AXDL_SET_USER_INFO: &[u8] = b"_Z16AXDL_SetUserInfoPKcS0_\0";
const AXDL_CREATE_P2SP_TASK: &[u8] = b"_Z19AXDL_CreateP2spTaskPKcS0_S0_S0_S0_Pl\0";
const AXDL_ENABLE_DCDN_WITH_VIP_CERT: &[u8] = b"_Z26AXDL_EnableDcdnWithVipCertliPKc\0";
const AXDL_START_TASK: &[u8] = b"_Z14AXDL_StartTaskl\0";
const AXDL_STOP_TASK: &[u8] = b"_Z13AXDL_StopTaskl\0";
const AXDL_DELETE_TASK: &[u8] = b"_Z15AXDL_DeleteTaskl\0";
const AXDL_QUERY_TASK_INFO: &[u8] = b"_Z18AXDL_QueryTaskInfolPFiliiPK19TAG_XL_TASK_INFO_EXPvES2_\0";
const UV_DEFAULT_LOOP: &[u8] = b"uv_default_loop\0";
const UV_RUN: &[u8] = b"uv_run\0";
const UV_RUN_NOWAIT: i32 = 2;
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);
const AXDL_APP_KEY: &str = "bGludXhfdGh1bmRlcgClFwE=";
const AXDL_APP_NAME: &str = "linux_thunder";

type AxdlInitCallback = unsafe extern "C" fn(i32, *mut c_void) -> i32;
type AxdlInit =
    unsafe extern "C" fn(*mut AxdlInitParameters, *const c_char, AxdlInitCallback, *mut c_void);
type AxdlUninit = unsafe extern "C" fn();
// The AXDL task-management exports queue commands onto the client's private
// command processor. They deliberately have no synchronous result value; the
// result is delivered later through its task/callback machinery.
type AxdlSetUserInfo = unsafe extern "C" fn(*const c_char, *const c_char);
type AxdlCreateP2spTask = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut i64,
);
type AxdlEnableDcdnWithVipCert = unsafe extern "C" fn(i64, i32, *const c_char);
type AxdlTaskAction = unsafe extern "C" fn(i64);
type AxdlQueryCallback = unsafe extern "C" fn(i64, i32, i32, *const u8, *mut c_void) -> i32;
type AxdlQueryTaskInfo = unsafe extern "C" fn(i64, AxdlQueryCallback, *mut c_void);
type UvDefaultLoop = unsafe extern "C" fn() -> *mut c_void;
type UvRun = unsafe extern "C" fn(*mut c_void, i32) -> i32;

fn start_with_acceleration_token(
    start_task: AxdlTaskAction,
    enable_dcdn_with_vip_cert: AxdlEnableDcdnWithVipCert,
    task_id: u32,
    token: Option<&CString>,
) {
    // The Linux SDK rejects XLSetAccelerateToken with 0x2393 until its task
    // reaches the running state. AXDL processes these calls in queue order.
    unsafe { start_task(i64::from(task_id)) };
    if let Some(token) = token {
        unsafe { enable_dcdn_with_vip_cert(i64::from(task_id), -1, token.as_ptr()) };
    }
}

// This is TAG_XL_DOWNLOAD_LIB_INIT_PARAM from the observed Linux client ABI.
// It is kept here rather than inferred from Rust layout so every pointer and
// length is explicit and remains auditable against future client updates.
#[repr(C)]
struct AxdlInitParameters {
    app_key: *const c_char,
    app_key_length: u32,
    _app_key_padding: u32,
    app_name: *const c_char,
    app_name_length: u32,
    _app_name_padding: u32,
    client_version: *const c_char,
    client_version_length: u32,
    _client_version_padding: u32,
    reserved_before_peer: [u64; 2],
    peer_id: *const c_char,
    peer_id_length: u32,
    _peer_id_padding: u32,
    reserved_after_peer: [u64; 2],
    data_path: *const c_char,
    data_path_length: u32,
    _data_path_padding: u32,
    data_path_copy: *const c_char,
    packed_data_path_length: u32,
    constant_nine: u32,
    constant_three: u64,
}

struct AxdlInitStrings {
    app_key: CString,
    app_name: CString,
    client_version: CString,
    peer_id: CString,
    data_path: CString,
    system_info: CString,
}

impl AxdlInitStrings {
    fn new(client_version: &str, peer_id: &str, data_path: &Path) -> Result<Self, AxdlLoadError> {
        let data_path = data_path.to_str().ok_or(AxdlLoadError::InvalidInput)?;
        Ok(Self {
            app_key: CString::new(AXDL_APP_KEY).map_err(|_| AxdlLoadError::InvalidInput)?,
            app_name: CString::new(AXDL_APP_NAME).map_err(|_| AxdlLoadError::InvalidInput)?,
            client_version: CString::new(client_version)
                .map_err(|_| AxdlLoadError::InvalidInput)?,
            peer_id: CString::new(peer_id).map_err(|_| AxdlLoadError::InvalidInput)?,
            data_path: CString::new(data_path).map_err(|_| AxdlLoadError::InvalidInput)?,
            system_info: CString::new(system_info()).map_err(|_| AxdlLoadError::InvalidInput)?,
        })
    }

    fn parameters(&self) -> AxdlInitParameters {
        AxdlInitParameters {
            app_key: self.app_key.as_ptr(),
            app_key_length: self.app_key.as_bytes().len() as u32,
            _app_key_padding: 0,
            app_name: self.app_name.as_ptr(),
            app_name_length: self.app_name.as_bytes().len() as u32,
            _app_name_padding: 0,
            client_version: self.client_version.as_ptr(),
            client_version_length: self.client_version.as_bytes().len() as u32,
            _client_version_padding: 0,
            reserved_before_peer: [0; 2],
            peer_id: self.peer_id.as_ptr(),
            peer_id_length: self.peer_id.as_bytes().len() as u32,
            _peer_id_padding: 0,
            reserved_after_peer: [0; 2],
            data_path: self.data_path.as_ptr(),
            data_path_length: self.data_path.as_bytes().len() as u32,
            _data_path_padding: 0,
            data_path_copy: self.data_path.as_ptr(),
            packed_data_path_length: self.data_path.as_bytes().len() as u32,
            constant_nine: 9,
            constant_three: 3,
        }
    }
}

struct InitCallbackState {
    result: Mutex<Option<i32>>,
    ready: Condvar,
    #[cfg(test)]
    exit_process_after_callback: bool,
}

#[cfg(test)]
extern "C" {
    fn _exit(status: i32) -> !;
}

unsafe extern "C" fn on_initialized(result: i32, context: *mut c_void) -> i32 {
    let state = unsafe { &*(context.cast::<InitCallbackState>()) };
    if let Ok(mut value) = state.result.lock() {
        *value = Some(result);
        state.ready.notify_all();
    }
    #[cfg(test)]
    if state.exit_process_after_callback {
        // The isolated test helper has proven the callback. The vendor SDK
        // owns native threads that cannot be safely joined by a unit-test
        // process, so bypass all teardown in that helper process.
        unsafe { _exit(0) };
    }
    0
}

const AXDL_TASK_INFO_BYTES: usize = 200;

struct QueryCallbackState {
    pending: AtomicBool,
    result: Mutex<Option<(i64, i32, [u8; AXDL_TASK_INFO_BYTES])>>,
}

unsafe extern "C" fn on_task_info(
    task_id: i64,
    _query_status: i32,
    result: i32,
    info: *const u8,
    context: *mut c_void,
) -> i32 {
    let state = unsafe { &*context.cast::<QueryCallbackState>() };
    let mut bytes = [0_u8; AXDL_TASK_INFO_BYTES];
    if !info.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(info, bytes.as_mut_ptr(), bytes.len()) };
    }
    if let Ok(mut slot) = state.result.lock() {
        *slot = Some((task_id, result, bytes));
    }
    state.pending.store(false, Ordering::Release);
    0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AxdlLoadError {
    UnsupportedFingerprint,
    FileFingerprintChanged,
    NodeLibrary,
    StatLibrary,
    SdkLibrary,
    Addon,
    RequiredSymbol,
    InvalidInput,
    InitializationFailed,
    InitializationTimedOut,
    OperationFailed,
}

/// Loaded private runtime. The declaration order deliberately unloads the
/// AXDL add-on before its two dependencies.
pub(super) struct AxdlRuntime {
    _addon: Library,
    _sdk: Library,
    _stat: Library,
    _node: Library,
    init: AxdlInit,
    #[allow(dead_code)]
    uninit: AxdlUninit,
    uv_default_loop: UvDefaultLoop,
    uv_run: UvRun,
    set_user_info: AxdlSetUserInfo,
    create_p2sp_task: AxdlCreateP2spTask,
    enable_dcdn_with_vip_cert: AxdlEnableDcdnWithVipCert,
    start_task: AxdlTaskAction,
    stop_task: AxdlTaskAction,
    delete_task: AxdlTaskAction,
    query_task_info: AxdlQueryTaskInfo,
    query_state: &'static QueryCallbackState,
    init_strings: Option<AxdlInitStrings>,
}

impl AxdlRuntime {
    pub(super) fn load(runtime: &LinuxXunleiRuntime) -> Result<Self, AxdlLoadError> {
        if !runtime.supports_axdl_bridge() {
            return Err(AxdlLoadError::UnsupportedFingerprint);
        }
        Self::load_verified(
            &runtime.stat_library,
            &runtime.sdk_library,
            &runtime.axdl_addon,
            &runtime.node_library,
            &runtime.node_sha256,
            &runtime.stat_sha256,
            &runtime.sdk_sha256,
            &runtime.axdl_addon_sha256,
        )
    }

    pub(super) fn load_verified(
        stat: &Path,
        sdk: &Path,
        addon: &Path,
        node: &Path,
        node_sha256: &str,
        stat_sha256: &str,
        sdk_sha256: &str,
        addon_sha256: &str,
    ) -> Result<Self, AxdlLoadError> {
        if !is_supported_axdl_fingerprint(node_sha256, stat_sha256, sdk_sha256, addon_sha256) {
            return Err(AxdlLoadError::UnsupportedFingerprint);
        }
        if sha256_file(node).as_deref() != Some(node_sha256)
            || sha256_file(stat).as_deref() != Some(stat_sha256)
            || sha256_file(sdk).as_deref() != Some(sdk_sha256)
            || sha256_file(addon).as_deref() != Some(addon_sha256)
        {
            return Err(AxdlLoadError::FileFingerprintChanged);
        }

        // The add-on registers itself through node_module_register when it is
        // loaded. Loading the packaged libnode resolves that registration
        // symbol, but this worker creates neither a Node process nor a JS
        // isolate. Lazy relocation avoids resolving unused V8 entry points.
        let flags = RTLD_LAZY | RTLD_GLOBAL;
        let node =
            unsafe { Library::open(Some(node), flags) }.map_err(|_| AxdlLoadError::NodeLibrary)?;
        let stat =
            unsafe { Library::open(Some(stat), flags) }.map_err(|_| AxdlLoadError::StatLibrary)?;
        let sdk =
            unsafe { Library::open(Some(sdk), flags) }.map_err(|_| AxdlLoadError::SdkLibrary)?;
        let addon =
            unsafe { Library::open(Some(addon), flags) }.map_err(|_| AxdlLoadError::Addon)?;
        for symbol in REQUIRED_AXDL_SYMBOLS {
            let _: libloading::os::unix::Symbol<unsafe extern "C" fn()> =
                unsafe { addon.get(symbol) }.map_err(|_| AxdlLoadError::RequiredSymbol)?;
        }
        let init = *unsafe { addon.get::<AxdlInit>(AXDL_INIT) }
            .map_err(|_| AxdlLoadError::RequiredSymbol)?;
        let uninit = *unsafe { addon.get::<AxdlUninit>(AXDL_UNINIT) }
            .map_err(|_| AxdlLoadError::RequiredSymbol)?;
        let uv_default_loop = *unsafe { node.get::<UvDefaultLoop>(UV_DEFAULT_LOOP) }
            .map_err(|_| AxdlLoadError::NodeLibrary)?;
        let uv_run =
            *unsafe { node.get::<UvRun>(UV_RUN) }.map_err(|_| AxdlLoadError::NodeLibrary)?;
        let set_user_info = *unsafe { addon.get::<AxdlSetUserInfo>(AXDL_SET_USER_INFO) }
            .map_err(|_| AxdlLoadError::RequiredSymbol)?;
        let create_p2sp_task = *unsafe { addon.get::<AxdlCreateP2spTask>(AXDL_CREATE_P2SP_TASK) }
            .map_err(|_| AxdlLoadError::RequiredSymbol)?;
        let enable_dcdn_with_vip_cert =
            *unsafe { addon.get::<AxdlEnableDcdnWithVipCert>(AXDL_ENABLE_DCDN_WITH_VIP_CERT) }
                .map_err(|_| AxdlLoadError::RequiredSymbol)?;
        let start_task = *unsafe { addon.get::<AxdlTaskAction>(AXDL_START_TASK) }
            .map_err(|_| AxdlLoadError::RequiredSymbol)?;
        let stop_task = *unsafe { addon.get::<AxdlTaskAction>(AXDL_STOP_TASK) }
            .map_err(|_| AxdlLoadError::RequiredSymbol)?;
        let delete_task = *unsafe { addon.get::<AxdlTaskAction>(AXDL_DELETE_TASK) }
            .map_err(|_| AxdlLoadError::RequiredSymbol)?;
        let query_task_info = *unsafe { addon.get::<AxdlQueryTaskInfo>(AXDL_QUERY_TASK_INFO) }
            .map_err(|_| AxdlLoadError::RequiredSymbol)?;
        Ok(Self {
            _addon: addon,
            _sdk: sdk,
            _stat: stat,
            _node: node,
            init,
            uninit,
            uv_default_loop,
            uv_run,
            set_user_info,
            create_p2sp_task,
            enable_dcdn_with_vip_cert,
            start_task,
            stop_task,
            delete_task,
            query_task_info,
            // Vendor callbacks may outlive an abandoned request or Rust-side
            // teardown. This one state object lives for the worker process.
            query_state: Box::leak(Box::new(QueryCallbackState {
                pending: AtomicBool::new(false),
                result: Mutex::new(None),
            })),
            init_strings: None,
        })
    }

    pub(super) fn initialize(
        &mut self,
        client_version: &str,
        peer_id: &str,
        data_path: &Path,
    ) -> Result<(), AxdlLoadError> {
        if self.init_strings.is_some() {
            return Ok(());
        }

        let strings = AxdlInitStrings::new(client_version, peer_id, data_path)?;
        let mut parameters = strings.parameters();
        // The SDK owns the callback context until it invokes the callback. A
        // timed-out worker is terminated rather than freeing it prematurely.
        let callback = Box::leak(Box::new(InitCallbackState {
            result: Mutex::new(None),
            ready: Condvar::new(),
            #[cfg(test)]
            exit_process_after_callback: std::env::var_os("VERSADOWN_AXDL_EXIT_IN_CALLBACK")
                .is_some(),
        }));
        unsafe {
            (self.init)(
                &mut parameters,
                strings.system_info.as_ptr(),
                on_initialized,
                (callback as *mut InitCallbackState).cast::<c_void>(),
            );
        }
        self.init_strings = Some(strings);

        let deadline = Instant::now() + INITIALIZE_TIMEOUT;
        loop {
            unsafe {
                (self.uv_run)((self.uv_default_loop)(), UV_RUN_NOWAIT);
            }
            let mut result = callback
                .result
                .lock()
                .map_err(|_| AxdlLoadError::InitializationFailed)?;
            if let Some(code) = *result {
                return (code == 0)
                    .then_some(())
                    .ok_or(AxdlLoadError::InitializationFailed);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(AxdlLoadError::InitializationTimedOut);
            }
            let wait = (deadline - now).min(Duration::from_millis(10));
            let (next, _) = callback
                .ready
                .wait_timeout(result, wait)
                .map_err(|_| AxdlLoadError::InitializationFailed)?;
            result = next;
            drop(result);
            thread::yield_now();
        }
    }

    #[cfg(test)]
    fn initialize_for_test(
        &mut self,
        client_version: &str,
        data_path: &Path,
    ) -> Result<(), AxdlLoadError> {
        self.initialize(
            client_version,
            "0000000000000000000000000000000000000000",
            data_path,
        )
    }

    pub(super) fn set_user_identity(&self, user_id: &str) -> Result<(), AxdlLoadError> {
        if self.init_strings.is_none() {
            return Err(AxdlLoadError::InitializationFailed);
        }
        let user_id = CString::new(user_id).map_err(|_| AxdlLoadError::InvalidInput)?;
        let empty = CString::new("").map_err(|_| AxdlLoadError::InvalidInput)?;
        // This only acknowledges that the command was accepted by AXDL's
        // private queue. Authentication is verified by the actual task
        // outcome, never by treating an ABI-undefined register as a result.
        unsafe { (self.set_user_info)(user_id.as_ptr(), empty.as_ptr()) };
        Ok(())
    }

    pub(super) fn create_and_start_task(
        &mut self,
        url: &str,
        referer: &str,
        output_directory: &Path,
        output_name: &str,
        acceleration_token: Option<&str>,
    ) -> Result<u32, AxdlLoadError> {
        if self.init_strings.is_none() {
            return Err(AxdlLoadError::InitializationFailed);
        }
        let url = CString::new(url).map_err(|_| AxdlLoadError::InvalidInput)?;
        let referer = CString::new(referer).map_err(|_| AxdlLoadError::InvalidInput)?;
        let cookie = CString::new("").map_err(|_| AxdlLoadError::InvalidInput)?;
        let output_directory = output_directory
            .to_str()
            .ok_or(AxdlLoadError::InvalidInput)
            .and_then(|path| CString::new(path).map_err(|_| AxdlLoadError::InvalidInput))?;
        let output_name = CString::new(output_name).map_err(|_| AxdlLoadError::InvalidInput)?;
        let acceleration_token = acceleration_token
            .map(CString::new)
            .transpose()
            .map_err(|_| AxdlLoadError::InvalidInput)?;
        // The vendor export assigns its client-visible virtual task ID before
        // returning, then queues the network operation. It is the same ID
        // accepted by AXDL_StartTask / StopTask / DeleteTask; it is *not* the
        // later internal SDK task ID shown in the official-client logs.
        let mut task_id = 0_i64;
        unsafe {
            (self.create_p2sp_task)(
                url.as_ptr(),
                referer.as_ptr(),
                cookie.as_ptr(),
                output_directory.as_ptr(),
                output_name.as_ptr(),
                &mut task_id,
            )
        }
        let task_id = u32::try_from(task_id).map_err(|_| AxdlLoadError::OperationFailed)?;
        if task_id == 0 {
            return Err(AxdlLoadError::OperationFailed);
        }
        start_with_acceleration_token(
            self.start_task,
            self.enable_dcdn_with_vip_cert,
            task_id,
            acceleration_token.as_ref(),
        );
        Ok(task_id)
    }

    pub(super) fn stop_task(&self, task_id: u32) -> Result<(), AxdlLoadError> {
        unsafe { (self.stop_task)(i64::from(task_id)) };
        Ok(())
    }

    pub(super) fn start_task(&self, task_id: u32) -> Result<(), AxdlLoadError> {
        unsafe { (self.start_task)(i64::from(task_id)) };
        Ok(())
    }

    pub(super) fn query_task_info(
        &self,
        task_id: u32,
    ) -> Option<(i32, [u8; AXDL_TASK_INFO_BYTES])> {
        if let Ok(mut slot) = self.query_state.result.lock() {
            if let Some((returned_id, result, bytes)) = slot.take() {
                if returned_id == i64::from(task_id) {
                    return Some((result, bytes));
                }
            }
        }
        if !self.query_state.pending.swap(true, Ordering::AcqRel) {
            unsafe {
                (self.query_task_info)(
                    i64::from(task_id),
                    on_task_info,
                    (self.query_state as *const QueryCallbackState)
                        .cast_mut()
                        .cast::<c_void>(),
                )
            };
        }
        for _ in 0..10 {
            unsafe { (self.uv_run)((self.uv_default_loop)(), UV_RUN_NOWAIT) };
            if let Ok(mut slot) = self.query_state.result.lock() {
                if let Some((returned_id, result, bytes)) = slot.take() {
                    if returned_id == i64::from(task_id) {
                        return Some((result, bytes));
                    }
                }
            }
            thread::sleep(Duration::from_millis(1));
        }
        None
    }

    pub(super) fn delete_task(&mut self, task_id: u32) -> Result<(), AxdlLoadError> {
        unsafe { (self.delete_task)(i64::from(task_id)) };
        Ok(())
    }
}

fn system_info() -> String {
    let distribution = Command::new("lsb_release")
        .arg("-ds")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().trim_matches('"').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Linux".into());
    format!("{distribution}-{}", std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::{start_with_acceleration_token, AxdlRuntime};
    use crate::xunlei_native::linux::runtime::discover_best_runtime;
    use std::{
        ffi::{c_char, CStr, CString},
        io::{Read, Write},
        net::TcpListener,
        path::PathBuf,
        process::Command,
        sync::Mutex,
    };

    static TASK_CALLS: Mutex<Vec<(&'static str, i64, i32, String)>> = Mutex::new(Vec::new());

    unsafe extern "C" fn record_start(task_id: i64) {
        TASK_CALLS
            .lock()
            .unwrap()
            .push(("start", task_id, 0, String::new()));
    }

    unsafe extern "C" fn record_certificate(task_id: i64, kind: i32, token: *const c_char) {
        TASK_CALLS.lock().unwrap().push((
            "certificate",
            task_id,
            kind,
            unsafe { CStr::from_ptr(token) }
                .to_string_lossy()
                .into_owned(),
        ));
    }

    #[test]
    fn acceleration_certificate_is_queued_after_starting_the_native_task() {
        TASK_CALLS.lock().unwrap().clear();
        let token = CString::new("certificate").unwrap();
        start_with_acceleration_token(record_start, record_certificate, 17, Some(&token));
        assert_eq!(
            *TASK_CALLS.lock().unwrap(),
            [
                ("start", 17, 0, String::new()),
                ("certificate", 17, -1, "certificate".into()),
            ]
        );
    }

    #[test]
    #[ignore = "requires the supported local DEB or Linyaps Xunlei runtime"]
    fn supported_installed_runtime_loads_all_axdl_symbols_without_node_process() {
        let runtime = discover_best_runtime().expect("installed runtime");
        AxdlRuntime::load(&runtime).expect("supported AXDL runtime");
    }

    #[test]
    #[ignore = "initializes the supported local runtime in an isolated temporary directory"]
    fn supported_installed_runtime_initializes_without_a_node_process() {
        if std::env::var_os("VERSADOWN_AXDL_INIT_TEST_CHILD").is_some() {
            let runtime = discover_best_runtime().expect("installed runtime");
            let output = PathBuf::from(
                std::env::var_os("VERSADOWN_AXDL_INIT_TEST_DATA")
                    .expect("isolated AXDL output path"),
            );
            let mut axdl = AxdlRuntime::load(&runtime).expect("supported AXDL runtime");
            axdl.initialize_for_test(&runtime.version, &output)
                .expect("AXDL initialization callback");
            // Keep the initialized runtime alive long enough to expose an
            // immediate background-thread fault, then bypass Rust and dynamic
            // library teardown in this dedicated helper process.
            std::thread::sleep(std::time::Duration::from_secs(2));
            unsafe { super::_exit(0) };
        }

        let runtime = discover_best_runtime().expect("installed runtime");
        let output = tempfile::tempdir().expect("temporary AXDL output");
        let test_binary = std::env::current_exe().expect("current test binary");
        let status = Command::new(test_binary)
            .args([
                "--exact",
                "xunlei_native::linux::axdl::tests::supported_installed_runtime_initializes_without_a_node_process",
                "--ignored",
            ])
            .env("VERSADOWN_AXDL_INIT_TEST_CHILD", "1")
            .env("VERSADOWN_AXDL_INIT_TEST_DATA", output.path())
            .status()
            .expect("spawn isolated AXDL initializer");
        assert!(
            status.success(),
            "isolated AXDL initializer failed: {status}"
        );
        assert!(output.path().join("CidStore.DB").exists());
        assert!(runtime.supports_axdl_bridge());
    }

    #[test]
    #[ignore = "creates a real local HTTP task through the supported AXDL runtime"]
    fn supported_installed_runtime_writes_direct_link_bytes() {
        if std::env::var_os("VERSADOWN_AXDL_DIRECT_TASK_TEST_CHILD").is_some() {
            let output = PathBuf::from(
                std::env::var_os("VERSADOWN_AXDL_DIRECT_TASK_TEST_DATA")
                    .expect("isolated AXDL output path"),
            );
            let state = output.join("state");
            std::fs::create_dir(&state).expect("isolated AXDL state");
            let listener = TcpListener::bind("127.0.0.1:0").expect("local HTTP listener");
            let address = listener.local_addr().expect("local HTTP address");
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    std::thread::spawn(move || {
                        let mut stream = stream;
                        let mut request = [0_u8; 4096];
                        let _ = stream.read(&mut request);
                        let body = vec![0x5a_u8; 64 * 1024];
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body);
                    });
                }
            });

            let runtime = discover_best_runtime().expect("installed runtime");
            let mut axdl = AxdlRuntime::load(&runtime).expect("supported AXDL runtime");
            axdl.initialize_for_test(&runtime.version, &state)
                .expect("AXDL initialization callback");
            let target = output.join("payload.bin");
            let task_id = axdl
                .create_and_start_task(
                    &format!("http://{address}/payload.bin"),
                    "",
                    &output,
                    "payload.bin",
                    None,
                )
                .expect("AXDL direct task start");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::fs::metadata(&target)
                .map(|metadata| metadata.len())
                .unwrap_or(0)
                != 64 * 1024
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            assert_eq!(std::fs::metadata(&target).unwrap().len(), 64 * 1024);
            for _ in 0..3 {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                let (result, info) = loop {
                    if let Some(info) = axdl.query_task_info(task_id) {
                        break info;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "AXDL query callback timed out"
                    );
                };
                let values: Vec<(usize, u32)> = (0..200)
                    .step_by(4)
                    .filter_map(|offset| {
                        let value =
                            u32::from_le_bytes(info[offset..offset + 4].try_into().unwrap());
                        (value != 0).then_some((offset, value))
                    })
                    .collect();
                eprintln!("AXDL query result={result}, nonzero u32 fields={values:?}");
                assert_eq!(u32::from_le_bytes(info[..4].try_into().unwrap()), 200);
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            unsafe { super::_exit(0) };
        }

        let output = tempfile::tempdir().expect("temporary AXDL output");
        let test_binary = std::env::current_exe().expect("current test binary");
        let status = Command::new(test_binary)
            .args([
                "--exact",
                "xunlei_native::linux::axdl::tests::supported_installed_runtime_writes_direct_link_bytes",
                "--ignored",
                "--nocapture",
            ])
            .env("VERSADOWN_AXDL_DIRECT_TASK_TEST_CHILD", "1")
            .env("VERSADOWN_AXDL_DIRECT_TASK_TEST_DATA", output.path())
            .status()
            .expect("spawn isolated AXDL direct-download helper");
        assert!(
            status.success(),
            "isolated AXDL direct task failed: {status}"
        );
        assert_eq!(
            std::fs::metadata(output.path().join("payload.bin"))
                .expect("direct task output")
                .len(),
            64 * 1024
        );
    }

    #[test]
    fn observed_initialization_parameter_layout_is_stable() {
        assert_eq!(std::mem::size_of::<super::AxdlInitParameters>(), 0x88);
        assert_eq!(
            std::mem::offset_of!(super::AxdlInitParameters, peer_id),
            0x40
        );
        assert_eq!(
            std::mem::offset_of!(super::AxdlInitParameters, data_path),
            0x60
        );
    }
}
