//! macOS Finder / Launch Services integration.
//!
//! Double-clicking a document associated with Ruffle.app does **not** put the
//! path on `argv`. macOS instead sends `application:openURLs:` to the app
//! delegate. Winit 0.30 owns that delegate and panics if it is replaced, so we
//! install our handler by adding the method onto Winit's existing class.

use crate::custom_event::RuffleEvent;
use crate::player::LaunchOptions;
use crate::preferences::GlobalPreferences;
use objc2::ffi;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::sel;
use objc2_foundation::{NSArray, NSString, NSURL};
use ruffle_frontend_utils::content::ContentDescriptor;
use std::cell::RefCell;
use std::ffi::CStr;
use std::path::PathBuf;
use std::sync::OnceLock;
use winit::event_loop::EventLoopProxy;

struct OpenContext {
    proxy: EventLoopProxy<RuffleEvent>,
    preferences: GlobalPreferences,
}

static OPEN_CONTEXT: OnceLock<OpenContext> = OnceLock::new();

// Paths that arrived before `install_open_urls_handler` stored a proxy.
thread_local! {
    static PENDING_PATHS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// Install `application:openURLs:` on Winit's application delegate class.
///
/// Must be called after [`winit::event_loop::EventLoop`] construction (so the
/// class exists) and before [`winit::event_loop::EventLoop::run_app`].
pub fn install_open_urls_handler(
    proxy: EventLoopProxy<RuffleEvent>,
    preferences: GlobalPreferences,
) {
    let _ = OPEN_CONTEXT.set(OpenContext {
        proxy: proxy.clone(),
        preferences: preferences.clone(),
    });

    // Flush anything that arrived extremely early (unlikely, but cheap).
    for path in take_pending_open_paths() {
        if let Some(ctx) = OPEN_CONTEXT.get() {
            open_path(ctx, path);
        }
    }

    let Some(cls) = AnyClass::get("WinitApplicationDelegate") else {
        tracing::error!(
            "WinitApplicationDelegate class not found; Finder file opens will not work"
        );
        return;
    };

    let sel = sel!(application:openURLs:);
    if cls.instance_method(sel).is_some() {
        tracing::debug!("application:openURLs: already present on WinitApplicationDelegate");
        return;
    }

    // Encoding: void return, object (self), SEL (_cmd), object (app), object (urls)
    let types = CStr::from_bytes_with_nul(b"v@:@@\0").expect("static CStr");
    let class_ptr = std::ptr::from_ref(cls).cast::<ffi::objc_class>().cast_mut();
    let imp: ffi::IMP = Some(unsafe {
        std::mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
            unsafe extern "C" fn(),
        >(handle_open_urls)
    });
    let success = Bool::from_raw(unsafe {
        ffi::class_addMethod(class_ptr, sel.as_ptr(), imp, types.as_ptr())
    });

    if success.as_bool() {
        tracing::info!("Installed macOS application:openURLs: handler for Finder opens");
    } else {
        tracing::error!("Failed to add application:openURLs: to WinitApplicationDelegate");
    }
}

unsafe extern "C-unwind" fn handle_open_urls(
    _this: *mut AnyObject,
    _cmd: Sel,
    _app: *mut AnyObject,
    urls: *mut AnyObject,
) {
    if urls.is_null() {
        return;
    }

    // SAFETY: Launch Services passes an NSArray<NSURL *>.
    let urls = unsafe { &*(urls as *const NSArray<NSURL>) };
    let mut paths = Vec::new();
    let count = urls.count();
    for i in 0..count {
        let url: Retained<NSURL> = unsafe { urls.objectAtIndex(i) };
        if let Some(path) = nsurl_to_path(&url) {
            tracing::info!("macOS openURLs requested: {}", path.display());
            paths.push(path);
        }
    }

    dispatch_open_paths(paths);
}

fn nsurl_to_path(url: &NSURL) -> Option<PathBuf> {
    if !unsafe { url.isFileURL() } {
        return None;
    }
    let path: Retained<NSString> = unsafe { url.path() }?;
    let c_ptr = path.UTF8String();
    if c_ptr.is_null() {
        return None;
    }
    let c_str = unsafe { CStr::from_ptr(c_ptr) };
    Some(PathBuf::from(c_str.to_string_lossy().as_ref()))
}

fn dispatch_open_paths(paths: Vec<PathBuf>) {
    let Some(ctx) = OPEN_CONTEXT.get() else {
        PENDING_PATHS.with(|pending| pending.borrow_mut().extend(paths));
        return;
    };

    for path in paths {
        open_path(ctx, path);
    }
}

fn open_path(ctx: &OpenContext, path: PathBuf) {
    let Some(desc) = ContentDescriptor::new_local(&path, None) else {
        tracing::warn!(
            "Ignoring open request for unsupported path: {}",
            path.display()
        );
        return;
    };

    let options = Box::new(LaunchOptions::from(&ctx.preferences));
    if ctx
        .proxy
        .send_event(RuffleEvent::Open(desc, options))
        .is_err()
    {
        tracing::warn!("Failed to send open event for {}", path.display());
    }
}

/// Drain any paths that were received before the open handler was installed.
pub fn take_pending_open_paths() -> Vec<PathBuf> {
    PENDING_PATHS.with(|pending| std::mem::take(&mut *pending.borrow_mut()))
}
