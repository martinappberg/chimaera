//! A page's `window.print()` in a macOS daemon window.
//!
//! WKWebView hands a frame's print request to its UI delegate's
//! `_webView:printFrame:pdfFirstPageSize:completionHandler:` and drops it when
//! the delegate doesn't answer. wry's doesn't, so the slides' and the
//! documents' print buttons (each prints a one-off same-origin iframe) did
//! nothing. This answers it the way Safari does: the requesting frame alone,
//! through the standard print panel (Save as PDF is in its PDF menu). Both
//! selectors are WebKit SPI; a WebKit without either falls back to the old
//! silent no-op, never a crash. WebView2 and WebKitGTK print on their own.
//!
//! The page's `print()` stays blocked until the completion handler runs — the
//! panel's did-run callback — so the caller's frame outlives the panel (WebKit
//! keeps servicing the page's print drawing meanwhile). Running the panel
//! synchronously instead deadlocks against that wait.

use std::ffi::{c_void, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;

use block2::{Block, RcBlock};
use objc2::encode::Encode;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{class, msg_send, sel};
use tauri::WebviewWindow;

#[repr(C)]
struct CGSize {
    width: f64,
    height: f64,
}

/// What the did-run callback needs, carried through `contextInfo`.
struct Pending {
    completion: RcBlock<dyn Fn()>,
    // The operation stays alive until its panel is done.
    _operation: Retained<AnyObject>,
}

/// Answer frame print requests in `window`'s webview.
pub(super) fn install(window: &WebviewWindow) {
    let result = window.with_webview(|webview| {
        let webview = webview.inner().cast::<AnyObject>();
        // SAFETY: on macOS `inner()` is the live WKWebView, and `with_webview`
        // runs this on the main thread, where WebKit and AppKit must be used.
        unsafe { install_on(webview) };
    });
    if let Err(error) = result {
        tracing::warn!(%error, "print hook: could not reach the webview");
    }
}

unsafe fn install_on(webview: *mut AnyObject) {
    if webview.is_null() {
        return;
    }
    let delegate: *mut AnyObject = msg_send![webview, UIDelegate];
    if delegate.is_null() {
        tracing::warn!("print hook: the webview has no UI delegate");
        return;
    }
    // wry gives every webview a delegate of the same class: add once.
    static ADDED: OnceLock<bool> = OnceLock::new();
    let added = *ADDED.get_or_init(|| add_methods(unsafe { &*delegate }.class()));
    if !added {
        return;
    }
    // WebKit reads which hooks a delegate answers when it is assigned, so
    // assign it again now that it answers one more.
    let _: () = msg_send![webview, setUIDelegate: delegate];
    tracing::debug!("print hook: installed");
}

fn add_methods(class: &AnyClass) -> bool {
    let print_frame: extern "C-unwind" fn(
        &AnyObject,
        Sel,
        *mut AnyObject,
        *mut AnyObject,
        CGSize,
        *mut Block<dyn Fn()>,
    ) = print_frame;
    let did_run: extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject, Bool, *mut c_void) = did_run;
    // SAFETY: each function has its method's C ABI — (id self, SEL _cmd,
    // WKWebView *, _WKFrameHandle *, CGSize, void (^)(void)) -> void and
    // (id self, SEL _cmd, NSPrintOperation *, BOOL, void *) -> void — and the
    // encodings below spell those signatures. class_addMethod fails (NO)
    // rather than replace a method the class already defines.
    let (print_imp, did_run_imp) = unsafe {
        (
            std::mem::transmute::<
                extern "C-unwind" fn(
                    &AnyObject,
                    Sel,
                    *mut AnyObject,
                    *mut AnyObject,
                    CGSize,
                    *mut Block<dyn Fn()>,
                ),
                Imp,
            >(print_frame),
            std::mem::transmute::<
                extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject, Bool, *mut c_void),
                Imp,
            >(did_run),
        )
    };
    let class = (class as *const AnyClass).cast_mut();
    let did_run_types =
        CString::new(format!("v@:@{}^v", Bool::ENCODING)).expect("an encoding has no NUL");
    // The did-run callback first: a print hook without it would never
    // unblock the page.
    let did_run_added = unsafe {
        objc2::ffi::class_addMethod(class, did_run_sel(), did_run_imp, did_run_types.as_ptr())
    };
    if !did_run_added.as_bool() {
        tracing::warn!(
            "print hook: the UI delegate already has the did-run callback; page print stays off"
        );
        return false;
    }
    let print_added = unsafe {
        objc2::ffi::class_addMethod(
            class,
            sel!(_webView:printFrame:pdfFirstPageSize:completionHandler:),
            print_imp,
            c"v@:@@{CGSize=dd}@?".as_ptr(),
        )
    };
    if !print_added.as_bool() {
        tracing::warn!("print hook: the UI delegate already answers frame print requests");
    }
    print_added.as_bool()
}

fn did_run_sel() -> Sel {
    sel!(chimaeraPrintOperationDidRun:success:contextInfo:)
}

/// `- (void)_webView:(WKWebView *)webView printFrame:(_WKFrameHandle *)frame
///    pdfFirstPageSize:(CGSize)size completionHandler:(void (^)(void))completion`
extern "C-unwind" fn print_frame(
    this: &AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    frame: *mut AnyObject,
    _pdf_first_page_size: CGSize,
    completion: *mut Block<dyn Fn()>,
) {
    tracing::debug!("print hook: frame print requested");
    if completion.is_null() {
        return;
    }
    // SAFETY: WebKit passes a live block for the duration of this call.
    let completion = unsafe { &*completion };
    // A panic must not unwind into WebKit; failing closed unblocks the page.
    let started = catch_unwind(AssertUnwindSafe(|| unsafe {
        start(this, webview, frame, completion)
    }))
    .unwrap_or(false);
    if !started {
        completion.call(());
    }
}

/// Open the print panel for `frame` as a sheet on the webview's window.
/// False when it could not start (the caller then unblocks the page).
unsafe fn start(
    delegate: &AnyObject,
    webview: *mut AnyObject,
    frame: *mut AnyObject,
    completion: &Block<dyn Fn()>,
) -> bool {
    if webview.is_null() || frame.is_null() {
        return false;
    }
    let spi = sel!(_printOperationWithPrintInfo:forFrame:);
    let responds: bool = msg_send![webview, respondsToSelector: spi];
    if !responds {
        tracing::warn!("print hook: this WebKit cannot print a single frame");
        return false;
    }
    let window: *mut AnyObject = msg_send![webview, window];
    if window.is_null() {
        return false;
    }
    // A copy: the shared print info is process-global, and the panel's
    // choices belong to this job.
    let shared: Retained<AnyObject> = msg_send![class!(NSPrintInfo), sharedPrintInfo];
    let info: Retained<AnyObject> = msg_send![&shared, copy];
    let operation: Option<Retained<AnyObject>> =
        msg_send![webview, _printOperationWithPrintInfo: &*info, forFrame: frame];
    let Some(operation) = operation else {
        return false;
    };
    let pending = Box::into_raw(Box::new(Pending {
        completion: completion.copy(),
        _operation: operation.clone(),
    }));
    let _: () = msg_send![
        &operation,
        runOperationModalForWindow: window,
        delegate: delegate,
        didRunSelector: did_run_sel(),
        contextInfo: pending.cast::<c_void>()
    ];
    true
}

/// `- (void)printOperationDidRun:(NSPrintOperation *)operation
///    success:(BOOL)success contextInfo:(void *)contextInfo`
extern "C-unwind" fn did_run(
    _this: &AnyObject,
    _cmd: Sel,
    _operation: *mut AnyObject,
    _success: Bool,
    context: *mut c_void,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: `context` is the `Pending` that `start` leaked for this very
    // operation, and AppKit calls this exactly once per run.
    let pending = unsafe { Box::from_raw(context.cast::<Pending>()) };
    let _ = catch_unwind(AssertUnwindSafe(|| pending.completion.call(())));
}
