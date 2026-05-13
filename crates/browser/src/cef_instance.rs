//! CEF Instance Management
//!
//! Manages the CEF (Chromium Embedded Framework) lifecycle as a singleton.
//! Handles initialization, message loop pumping, and shutdown.
//!
//! CEF initialization is split into two phases:
//! 1. `handle_subprocess()` - Must be called very early in main(), before any GUI
//!    initialization. This handles CEF subprocess execution.
//! 2. `initialize()` - Called later to complete CEF setup for the browser process.

use anyhow::{Result, anyhow};
use cef::{
    App, BrowserProcessHandler, ImplApp, ImplBrowserProcessHandler, ImplCommandLine,
    RenderProcessHandler, WrapApp, WrapBrowserProcessHandler, api_hash, rc::Rc as _, sys, wrap_app,
    wrap_browser_process_handler,
};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use crate::page_chrome::PageChromeRenderProcessHandlerBuilder;

/// File-based startup trace for diagnosing silent-exit issues on Windows.
/// Writes to Glass_startup.log next to the executable.
#[cfg(target_os = "windows")]
fn cef_trace(msg: &str) {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true)
                .open(dir.join("Glass_startup.log")) {
                use std::io::Write;
                let _ = writeln!(f, "[{:?}] [cef] {}", std::time::SystemTime::now(), msg);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn cef_trace(_msg: &str) {}

static CEF_SUBPROCESS_HANDLED: AtomicBool = AtomicBool::new(false);
static CEF_INITIALIZED: AtomicBool = AtomicBool::new(false);
static CEF_CONTEXT_READY: AtomicBool = AtomicBool::new(false);
static CEF_INSTANCE: Mutex<Option<Arc<CefInstance>>> = Mutex::new(None);
static CEF_APP: Mutex<Option<cef::App>> = Mutex::new(None);
#[cfg(target_os = "windows")]
static WINDOWS_CEF_RUNTIME_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(target_os = "macos")]
static CEF_LIBRARY_LOADER: Mutex<Option<cef::library_loader::LibraryLoader>> = Mutex::new(None);

// Pump scheduling: absolute time (microseconds since PUMP_EPOCH) when
// the next do_message_loop_work() call should happen. u64::MAX = idle.
static PUMP_EPOCH: OnceLock<Instant> = OnceLock::new();
static NEXT_PUMP_AT_US: AtomicU64 = AtomicU64::new(u64::MAX);

fn elapsed_us() -> u64 {
    PUMP_EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

// ── Browser Process Handler ──────────────────────────────────────────
// Defined before GlassApp so a cached instance can be stored in it.

#[derive(Clone)]
struct GlassBrowserProcessHandler {}

impl GlassBrowserProcessHandler {
    fn new() -> Self {
        Self {}
    }
}

wrap_browser_process_handler! {
    struct GlassBrowserProcessHandlerBuilder {
        handler: GlassBrowserProcessHandler,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            CEF_CONTEXT_READY.store(true, Ordering::SeqCst);
        }

        fn on_before_child_process_launch(&self, command_line: Option<&mut cef::CommandLine>) {
            let Some(command_line) = command_line else {
                return;
            };
            command_line.append_switch(Some(&"disable-session-crashed-bubble".into()));
        }

        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            let target_us = elapsed_us() + (delay_ms.max(0) as u64) * 1000;
            NEXT_PUMP_AT_US.fetch_min(target_us, Ordering::Release);
        }
    }
}

impl GlassBrowserProcessHandlerBuilder {
    fn build(handler: GlassBrowserProcessHandler) -> BrowserProcessHandler {
        Self::new(handler)
    }
}

// ── CEF App ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct GlassApp {
    browser_process_handler: cef::BrowserProcessHandler,
    render_process_handler: cef::RenderProcessHandler,
}

impl GlassApp {
    fn new() -> Self {
        let handler = GlassBrowserProcessHandlerBuilder::build(GlassBrowserProcessHandler::new());
        let render_process_handler = PageChromeRenderProcessHandlerBuilder::build();
        Self {
            browser_process_handler: handler,
            render_process_handler,
        }
    }
}

wrap_app! {
    struct GlassAppBuilder {
        app: GlassApp,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&cef::CefStringUtf16>,
            command_line: Option<&mut cef::CommandLine>,
        ) {
            let Some(command_line) = command_line else {
                return;
            };

            command_line.append_switch(Some(&"no-startup-window".into()));
            command_line.append_switch(Some(&"noerrdialogs".into()));
            command_line.append_switch(Some(&"hide-crash-restore-bubble".into()));
            // Keep Chromium on the real keychain path in signed production
            // builds. The mock keychain path is only for local ad-hoc bundles.
            #[cfg(debug_assertions)]
            command_line.append_switch(Some(&"use-mock-keychain".into()));
            command_line.append_switch(Some(&"disable-gpu-sandbox".into()));
            command_line.append_switch_with_value(
                Some(&"autoplay-policy".into()),
                Some(&"no-user-gesture-required".into()),
            );
            command_line.append_switch_with_value(
                Some(&"component-updater".into()),
                Some(&"fast-update".into()),
            );
            // GPU rendering performance: use Metal backend on macOS and bypass
            // the GPU blocklist so hardware acceleration is always active.
            #[cfg(target_os = "macos")]
            {
                command_line.append_switch_with_value(
                    Some(&"use-angle".into()),
                    Some(&"metal".into()),
                );
            }
            command_line.append_switch(Some(&"ignore-gpu-blocklist".into()));
            command_line.append_switch(Some(&"enable-gpu-rasterization".into()));
            command_line.append_switch(Some(&"enable-zero-copy".into()));
            // Hardware video decoding: uses VideoToolbox on macOS for H.264/HEVC,
            // offloading decode from CPU to the media engine.
            command_line.append_switch(Some(&"enable-accelerated-video-decode".into()));
            command_line.append_switch(Some(&"enable-accelerated-mjpeg-decode".into()));
            command_line.append_switch_with_value(
                Some(&"enable-features".into()),
                Some(&"PlatformHEVCDecoderSupport,PlatformEncryptedDolbyVision".into()),
            );
            #[cfg(target_os = "windows")]
            command_line.append_switch_with_value(
                Some(&"disable-features".into()),
                Some(&"WebUsbDeviceDetection".into()),
            );
            #[cfg(debug_assertions)]
            {
                if std::env::var_os("GLASS_CEF_DEBUG").is_some() {
                    command_line.append_switch(Some(&"enable-logging=stderr".into()));
                    command_line.append_switch_with_value(
                        Some(&"remote-debugging-port".into()),
                        Some(&"9222".into()),
                    );
                }
            }
        }

        fn browser_process_handler(&self) -> Option<cef::BrowserProcessHandler> {
            Some(self.app.browser_process_handler.clone())
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(self.app.render_process_handler.clone())
        }
    }
}

impl GlassAppBuilder {
    fn build(app: GlassApp) -> cef::App {
        Self::new(app)
    }
}

pub fn build_cef_app() -> cef::App {
    GlassAppBuilder::build(GlassApp::new())
}

// ── CEF path resolution ──────────────────────────────────────────────

/// Resolve the CEF directory from `CEF_PATH` env var, falling back to `~/.local/share/cef`.
#[cfg(target_os = "macos")]
fn resolve_cef_dir_from_env() -> Option<PathBuf> {
    let cef_dir = match std::env::var("CEF_PATH") {
        Ok(path) => PathBuf::from(path),
        Err(_) => {
            let home = std::env::var("HOME").ok()?;
            PathBuf::from(home).join(".local/share/cef")
        }
    };
    if cef_dir
        .join("Chromium Embedded Framework.framework/Chromium Embedded Framework")
        .exists()
    {
        Some(cef_dir)
    } else {
        None
    }
}

/// Load the CEF framework directly from a directory path (bypasses LibraryLoader
/// which only supports bundle-relative paths).
#[cfg(target_os = "macos")]
fn load_cef_framework_from_dir(cef_dir: &std::path::Path) -> bool {
    let framework_path =
        cef_dir.join("Chromium Embedded Framework.framework/Chromium Embedded Framework");
    use std::os::unix::ffi::OsStrExt;
    let Ok(path_cstr) = std::ffi::CString::new(framework_path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { cef::load_library(Some(&*path_cstr.as_ptr().cast())) == 1 }
}

#[cfg(target_os = "windows")]
fn contains_windows_cef_runtime(root: &Path) -> bool {
    root.join("libcef.dll").exists()
}

#[cfg(target_os = "windows")]
fn required_windows_runtime_files() -> &'static [&'static str] {
    &[
        "libcef.dll",
        "chrome_elf.dll",
        "icudtl.dat",
        "resources.pak",
        "chrome_100_percent.pak",
        "chrome_200_percent.pak",
        "v8_context_snapshot.bin",
    ]
}

#[cfg(target_os = "windows")]
fn required_windows_runtime_dirs() -> &'static [&'static str] {
    &["locales"]
}

#[cfg(target_os = "windows")]
fn canonicalize_if_exists(path: &Path) -> Option<PathBuf> {
    if path.exists() {
        path.canonicalize().ok()
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn resolve_windows_cef_runtime_root() -> Option<PathBuf> {
    if let Some(existing) = WINDOWS_CEF_RUNTIME_ROOT.lock().clone() {
        return Some(existing);
    }

    let exe_path = std::env::current_exe().ok()?;
    let exe_dir = exe_path.parent()?.to_path_buf();
    let canonical_exe_dir = canonicalize_if_exists(&exe_dir)?;
    if has_required_windows_runtime_layout(&exe_dir) {
        *WINDOWS_CEF_RUNTIME_ROOT.lock() = Some(canonical_exe_dir.clone());
        return Some(canonical_exe_dir);
    }

    let staged_runtime = exe_dir.join("cef_runtime");
    if has_required_windows_runtime_layout(&staged_runtime) {
        let canonical_staged = canonicalize_if_exists(&staged_runtime)?;
        *WINDOWS_CEF_RUNTIME_ROOT.lock() = Some(canonical_staged.clone());
        return Some(canonical_staged);
    }

    if let Ok(cef_path) = std::env::var("CEF_PATH") {
        let from_env = PathBuf::from(cef_path);
        if has_required_windows_runtime_layout(&from_env) {
            let canonical_env = canonicalize_if_exists(&from_env)?;
            *WINDOWS_CEF_RUNTIME_ROOT.lock() = Some(canonical_env.clone());
            return Some(canonical_env);
        }
        if contains_windows_cef_runtime(&from_env) {
            log::warn!(
                "[browser::cef_instance] Ignoring incomplete CEF_PATH runtime root on Windows: {}",
                from_env.display()
            );
        }
    }

    None
}

#[cfg(target_os = "windows")]
pub fn has_windows_cef_runtime() -> bool {
    resolve_windows_cef_runtime_root()
        .as_deref()
        .is_some_and(has_required_windows_runtime_layout)
}

#[cfg(target_os = "windows")]
fn has_required_windows_runtime_layout(runtime_root: &Path) -> bool {
    for file_name in required_windows_runtime_files() {
        if !runtime_root.join(file_name).exists() {
            return false;
        }
    }
    required_windows_runtime_dirs()
        .iter()
        .all(|dir_name| runtime_root.join(dir_name).is_dir())
}

#[cfg(target_os = "windows")]
fn ensure_runtime_on_path(runtime_root: &Path) {
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = std::env::split_paths(&current_path).collect::<Vec<_>>();
    if !path_entries.iter().any(|entry| entry == runtime_root) {
        path_entries.insert(0, runtime_root.to_path_buf());
        if let Ok(joined_path) = std::env::join_paths(path_entries) {
            unsafe {
                std::env::set_var("PATH", joined_path);
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn log_windows_runtime_layout(runtime_root: &Path) {
    for file_name in required_windows_runtime_files() {
        let file_path = runtime_root.join(file_name);
        if !file_path.exists() {
            log::warn!(
                "[browser::cef_instance] Missing required CEF runtime file: {}",
                file_path.display()
            );
        }
    }

    for dir_name in required_windows_runtime_dirs() {
        let dir_path = runtime_root.join(dir_name);
        if !dir_path.exists() {
            log::warn!(
                "[browser::cef_instance] Missing required CEF runtime directory: {}",
                dir_path.display()
            );
        }
    }
}

// ── CefInstance ──────────────────────────────────────────────────────

pub struct CefInstance {}

impl CefInstance {
    pub fn global() -> Option<Arc<CefInstance>> {
        CEF_INSTANCE.lock().clone()
    }

    /// Check if CEF context is initialized and ready for browser creation
    pub fn is_context_ready() -> bool {
        CEF_CONTEXT_READY.load(Ordering::SeqCst)
    }

    /// Handle CEF subprocess execution. This MUST be called very early in main(),
    /// before any GUI initialization.
    ///
    /// If this process is a CEF subprocess (renderer, GPU, etc.), this function
    /// will NOT return - it will call std::process::exit().
    ///
    /// If this is the main browser process, it returns Ok(()) and normal
    /// initialization should continue.
    pub fn handle_subprocess() -> Result<()> {
        if CEF_SUBPROCESS_HANDLED.load(Ordering::SeqCst) {
            return Ok(());
        }

        #[cfg(target_os = "macos")]
        {
            let exe_path = std::env::current_exe()
                .map_err(|e| anyhow!("Failed to get current executable path: {}", e))?;

            let framework_path = exe_path
                .parent()
                .map(|p| p.join("../Frameworks/Chromium Embedded Framework.framework/Chromium Embedded Framework"));

            match framework_path {
                Some(path) if path.exists() => {
                    let loader = cef::library_loader::LibraryLoader::new(&exe_path, false);
                    if !loader.load() {
                        log::warn!("[browser::cef_instance] LibraryLoader::load() failed");
                        return Ok(());
                    }
                    *CEF_LIBRARY_LOADER.lock() = Some(loader);
                }
                _ => {
                    // Not running from a bundle - try CEF_PATH env var or ~/.local/share/cef
                    match resolve_cef_dir_from_env() {
                        Some(cef_dir) => {
                            if !load_cef_framework_from_dir(&cef_dir) {
                                log::warn!(
                                    "[browser::cef_instance] Failed to load CEF from {}",
                                    cef_dir.display()
                                );
                                return Ok(());
                            }
                        }
                        None => {
                            return Ok(());
                        }
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            cef_trace("resolve_windows_cef_runtime_root...")
            if let Some(runtime_root) = resolve_windows_cef_runtime_root() {
                cef_trace(&format!("CEF runtime root found: {}", runtime_root.display()));
                log::info!(
                    "[browser::cef_instance] Windows CEF runtime root: {}",
                    runtime_root.display()
                );
                let layout_ok = has_required_windows_runtime_layout(&runtime_root);
                cef_trace(&format!("has_required_windows_runtime_layout: {}", layout_ok));
                if layout_ok {
                    ensure_runtime_on_path(&runtime_root);
                    cef_trace("ensure_runtime_on_path done");
                } else {
                    cef_trace("runtime root MISSING required files");
                    log_windows_runtime_layout(&runtime_root);
                    log::warn!(
                        "[browser::cef_instance] Runtime root is missing required Windows CEF files: {}",
                        runtime_root.display()
                    );
                }
            } else {
                cef_trace("CEF runtime root NOT FOUND (libcef.dll missing or layout incomplete)");
                log::warn!(
                    "[browser::cef_instance] Unable to resolve Windows CEF runtime root. \
                     Expected libcef.dll in executable directory or executable-directory/cef_runtime."
                );
            }
        }

        cef_trace(">> api_hash check...");
        let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
        cef_trace("<< api_hash done");

        cef_trace(">> cef::args::Args::new()...");
        let args = cef::args::Args::new();
        cef_trace("<< Args created");

        cef_trace(">> build_cef_app()...");
        let mut app = build_cef_app();
        cef_trace("<< build_cef_app done");

        cef_trace(">> cef::execute_process() — if ret>=0, this process is a CEF subprocess and will exit...");

        let ret = cef::execute_process(
            Some(args.as_main_args()),
            Some(&mut app),
            std::ptr::null_mut(),
        );

        cef_trace(&format!("<< cef::execute_process returned: {}", ret));

        if ret >= 0 {
            cef_trace(&format!("SUBPROCESS DETECTED (ret={}). Calling process::exit({}). This is normal for CEF helper processes.", ret, ret));
            std::process::exit(ret);
        }

        cef_trace("Main browser process confirmed (ret < 0). Continuing startup.");

        *CEF_APP.lock() = Some(app);
        CEF_SUBPROCESS_HANDLED.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Initialize CEF for the browser process. Call this after handle_subprocess()
    /// has returned successfully and after GPUI is set up.
    pub fn initialize(_cx: &mut gpui::App) -> Result<Arc<CefInstance>> {
        if CEF_INITIALIZED.load(Ordering::SeqCst) {
            if let Some(instance) = Self::global() {
                return Ok(instance);
            }
        }

        if !CEF_SUBPROCESS_HANDLED.load(Ordering::SeqCst) {
            return Err(anyhow!(
                "CEF subprocess handling was not done. Call CefInstance::handle_subprocess() early in main()."
            ));
        }

        Self::initialize_cef()?;

        CEF_INITIALIZED.store(true, Ordering::SeqCst);
        let instance = Arc::new(CefInstance {});
        *CEF_INSTANCE.lock() = Some(instance.clone());

        Ok(instance)
    }

    fn initialize_cef() -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            crate::macos_protocol::add_cef_protocols_to_nsapp();
        }

        let args = cef::args::Args::new();

        let mut app_guard = CEF_APP.lock();
        let app = app_guard.as_mut().ok_or_else(|| {
            anyhow!("CEF App not found. handle_subprocess() must be called first.")
        })?;

        let mut settings = cef::Settings::default();

        settings.windowless_rendering_enabled = 1;
        settings.external_message_pump = 1;
        settings.no_sandbox = 1;
        settings.log_severity = cef::sys::cef_log_severity_t::LOGSEVERITY_WARNING.into();

        // Override the user-agent to match the current stable Chrome release.
        // CEF 146 is based on a Chromium version that hasn't reached Chrome
        // stable yet, and Google's sign-in endpoints reject unrecognized
        // browser versions with 400 errors on the browserinfo fingerprint
        // check. Using Chrome 145's stable UA keeps auth flows working.
        #[cfg(target_os = "windows")]
        {
            settings.user_agent = cef::CefString::from(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                 AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/145.0.7632.75 Safari/537.36",
            );
        }

        #[cfg(not(target_os = "windows"))]
        {
            settings.user_agent = cef::CefString::from(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                 AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/145.0.7632.75 Safari/537.36",
            );
        }

        #[cfg(target_os = "macos")]
        {
            if let Ok(exe_path) = std::env::current_exe() {
                if let Some(exe_dir) = exe_path.parent() {
                    // Try bundle path first: .app/Contents/Frameworks/Glass Helper.app/...
                    let bundle_helper =
                        exe_dir.join("../Frameworks/Glass Helper.app/Contents/MacOS/Glass Helper");
                    // Fall back to glass_helper next to the executable (cargo run)
                    let dev_helper = exe_dir.join("glass_helper");

                    let helper_path = if bundle_helper.exists() {
                        bundle_helper.canonicalize().ok()
                    } else if dev_helper.exists() {
                        dev_helper.canonicalize().ok()
                    } else {
                        None
                    };

                    if let Some(path) = helper_path {
                        if let Some(path_str) = path.to_str() {
                            settings.browser_subprocess_path = cef::CefString::from(path_str);
                        }
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(exe_path) = std::env::current_exe() {
                if let Some(exe_path_str) = exe_path.to_str() {
                    settings.browser_subprocess_path = cef::CefString::from(exe_path_str);
                }
            }

            if let Some(runtime_root) = resolve_windows_cef_runtime_root() {
                log::info!(
                    "[browser::cef_instance] Initializing CEF using runtime root: {}",
                    runtime_root.display()
                );
                if !has_required_windows_runtime_layout(&runtime_root) {
                    log_windows_runtime_layout(&runtime_root);
                    return Err(anyhow!(
                        "Windows CEF runtime root is missing required files/directories: {}",
                        runtime_root.display()
                    ));
                }
                ensure_runtime_on_path(&runtime_root);
                log_windows_runtime_layout(&runtime_root);

                if let Some(runtime_root_str) = runtime_root.to_str() {
                    settings.resources_dir_path = cef::CefString::from(runtime_root_str);
                }

                let locales_dir = runtime_root.join("locales");
                if let Some(locales_dir_str) = locales_dir.to_str() {
                    settings.locales_dir_path = cef::CefString::from(locales_dir_str);
                }
            } else {
                log::warn!(
                    "[browser::cef_instance] CEF runtime root could not be resolved during initialize_cef()."
                );
                return Err(anyhow!(
                    "Windows CEF runtime root could not be resolved during initialize_cef()."
                ));
            }
        }

        // Set framework_dir_path and main_bundle_path only when not running from a
        // bundle (e.g. cargo run with CEF_PATH). When running from a .app bundle, CEF
        // discovers the bundle automatically via NSBundle; setting main_bundle_path to
        // the wrong directory (Contents/MacOS/) would cause CEF to fail reading the
        // CFBundleIdentifier.
        #[cfg(target_os = "macos")]
        {
            let running_from_bundle = CEF_LIBRARY_LOADER.lock().is_some();
            if !running_from_bundle {
                if let Some(cef_dir) = resolve_cef_dir_from_env() {
                    let fw_path = cef_dir.join("Chromium Embedded Framework.framework");
                    if fw_path.exists() {
                        if let Some(fw_str) = fw_path.to_str() {
                            settings.framework_dir_path = cef::CefString::from(fw_str);
                        }
                    }
                    if let Ok(exe_path) = std::env::current_exe() {
                        if let Some(exe_dir) = exe_path.parent() {
                            if let Some(dir_str) = exe_dir.to_str() {
                                settings.main_bundle_path = cef::CefString::from(dir_str);
                            }
                        }
                    }
                }
            }
        }

        let cache_dir = paths::data_dir().join("browser_cache");
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            log::warn!(
                "[browser::cef_instance] Failed to create browser cache directory: {}",
                e
            );
        }
        if let Some(cache_path_str) = cache_dir.to_str() {
            settings.cache_path = cef::CefString::from(cache_path_str);
            settings.root_cache_path = cef::CefString::from(cache_path_str);
        }
        settings.persist_session_cookies = 1;

        #[cfg(debug_assertions)]
        {
            if std::env::var_os("GLASS_CEF_DEBUG").is_some() {
                settings.remote_debugging_port = 9222;
            }
        }

        let result = cef::initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(app),
            std::ptr::null_mut(),
        );

        if result != 1 {
            return Err(anyhow!("Failed to initialize CEF (error code: {})", result));
        }

        Ok(())
    }

    /// Returns true when CEF has requested a pump and the delay has elapsed.
    pub fn should_pump() -> bool {
        if !CEF_CONTEXT_READY.load(Ordering::SeqCst) {
            return false;
        }
        elapsed_us() >= NEXT_PUMP_AT_US.load(Ordering::Acquire)
    }

    /// Microseconds until the next scheduled pump, or 0 if overdue.
    pub fn time_until_next_pump_us() -> u64 {
        NEXT_PUMP_AT_US
            .load(Ordering::Acquire)
            .saturating_sub(elapsed_us())
    }

    /// Pump CEF message loop. Only call when `should_pump()` returns true.
    pub fn pump_messages() {
        if !CEF_CONTEXT_READY.load(Ordering::SeqCst) {
            return;
        }
        // Clear schedule before pumping. CEF will call
        // on_schedule_message_pump_work during do_message_loop_work
        // if more work is needed.
        NEXT_PUMP_AT_US.store(u64::MAX, Ordering::Release);
        cef::do_message_loop_work();
        // If CEF didn't schedule new work during the pump, set a
        // fallback so we check back periodically (~30 Hz idle).
        let _ = NEXT_PUMP_AT_US.compare_exchange(
            u64::MAX,
            elapsed_us() + 33_000,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
    }

    pub fn shutdown() {
        if !CEF_INITIALIZED.load(Ordering::SeqCst) {
            return;
        }

        log::info!("[browser::cef_instance] shutdown: starting");

        // Force-close all tracked browsers and drop their CEF handles.
        // This must happen before cef::shutdown() because CEF asserts
        // that all BrowserContext instances are destroyed (DCHECK all_.empty()).
        // Dropping the Rust cef::Browser handles releases the ref counts
        // that keep BrowserContext alive.
        //
        // We cannot rely on BrowserTab::Drop for this — GPUI's entity
        // lifecycle doesn't guarantee entities are dropped before quit
        // futures run.
        let closed_count = crate::tab::close_all_browsers();

        // Prevent regular pump scheduling from interfering.
        CEF_CONTEXT_READY.store(false, Ordering::SeqCst);

        // Pump the message loop to let CEF process close events.
        // With external_message_pump=1, close_browser() is async and
        // requires message loop iterations to fully complete.
        if closed_count > 0 {
            for _ in 0..10 {
                cef::do_message_loop_work();
            }
        }

        CEF_INITIALIZED.store(false, Ordering::SeqCst);
        *CEF_INSTANCE.lock() = None;

        log::info!("[browser::cef_instance] shutdown: calling cef::shutdown()");
        cef::shutdown();
        log::info!("[browser::cef_instance] shutdown: cef::shutdown() returned");

        *CEF_APP.lock() = None;
        #[cfg(target_os = "windows")]
        {
            *WINDOWS_CEF_RUNTIME_ROOT.lock() = None;
        }
    }
}

impl Drop for CefInstance {
    fn drop(&mut self) {
        Self::shutdown();
    }
}
