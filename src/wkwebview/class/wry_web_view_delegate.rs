// Copyright 2020-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::{ffi::CStr, panic::AssertUnwindSafe, rc::Rc};

use http::Request;
use objc2::{
  define_class, msg_send,
  rc::Retained,
  runtime::{NSObject, ProtocolObject},
  DeclaredClass, MainThreadOnly,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSObjectProtocol, NSString};
use objc2_web_kit::{WKScriptMessage, WKScriptMessageHandler, WKUserContentController};

use crate::wkwebview::ipc_registry::{with_registry, IpcHandler};

pub const IPC_MESSAGE_HANDLER_NAME: &str = "ipc";

pub struct WryWebViewDelegateIvars {
  pub controller: Retained<WKUserContentController>,
  /// Used when the sending web view is not in the IPC registry (today's behaviour).
  pub ipc_handler: IpcHandler,
}

define_class!(
  #[unsafe(super(NSObject))]
  #[thread_kind = MainThreadOnly]
  #[ivars = WryWebViewDelegateIvars]
  pub struct WryWebViewDelegate;

  unsafe impl NSObjectProtocol for WryWebViewDelegate {}

  unsafe impl WKScriptMessageHandler for WryWebViewDelegate {
    // Function for ipc handler
    #[unsafe(method(userContentController:didReceiveScriptMessage:))]
    fn did_receive(
      this: &WryWebViewDelegate,
      _controller: &WKUserContentController,
      msg: &WKScriptMessage,
    ) {
      // Safety: objc runtime calls are unsafe
      unsafe {
        #[cfg(feature = "tracing")]
        let _span = tracing::info_span!(parent: None, "wry::ipc::handle").entered();

        // One delegate serves every web view that shares this controller (see `ipc_registry`),
        // so route by `WKScriptMessage.webView`, "The web view that sent the message"
        // <https://developer.apple.com/documentation/webkit/wkscriptmessage/webview>.
        let ipc_handler = sender_address(msg)
          .and_then(|sender| with_registry(this.mtm(), |registry| registry.handler(sender)))
          .unwrap_or_else(|| this.ivars().ipc_handler.clone());
        let body = msg.body();
        if let Ok(body) = body.downcast::<NSString>() {
          let js_utf8 = body.UTF8String();

          let frame_info = msg.frameInfo();
          let request = frame_info.request();
          let url = request.URL().unwrap();
          let absolute_url = url.absoluteString().unwrap();
          let url_utf8 = absolute_url.UTF8String();

          if let (Ok(url), Ok(js)) = (
            CStr::from_ptr(url_utf8).to_str(),
            CStr::from_ptr(js_utf8).to_str(),
          ) {
            if let Ok(r) = Request::builder().uri(url).body(js.to_string()) {
              ipc_handler(r);
            } else {
              #[cfg(feature = "tracing")]
              tracing::warn!("WebView received invalid IPC request: {}", js);
            }
            return;
          }
        }

        #[cfg(feature = "tracing")]
        tracing::warn!("WebView received invalid IPC call.");
      }
    }
  }
);

/// Address of the web view that sent `msg`, or `None` if it is gone.
fn sender_address(msg: &WKScriptMessage) -> Option<usize> {
  // SAFETY: `webView` is a documented `WKScriptMessage` property returning a (weak, so possibly
  // nil) `WKWebView`; it is read through `msg_send!` because the typed accessor is macOS-only.
  let sender: Option<Retained<NSObject>> = unsafe { msg_send![msg, webView] };
  sender.map(|sender| Retained::as_ptr(&sender) as usize)
}

impl WryWebViewDelegate {
  /// Creates the IPC delegate of the web view at address `webview` and registers its handler.
  /// The "ipc" script message handler is added to `controller` only by the first live web view
  /// on it: a configuration from `with_webview_configuration` shares its controller, and WebKit
  /// raises an exception for a name that is already registered
  /// <https://developer.apple.com/documentation/webkit/wkusercontentcontroller/add(_:name:)>.
  pub fn new(
    controller: Retained<WKUserContentController>,
    webview: usize,
    ipc_handler: Box<dyn Fn(Request<String>)>,
    mtm: MainThreadMarker,
  ) -> Retained<Self> {
    let ipc_handler: IpcHandler = Rc::from(ipc_handler);
    let controller_address = Retained::as_ptr(&controller) as usize;
    let delegate = mtm
      .alloc::<WryWebViewDelegate>()
      .set_ivars(WryWebViewDelegateIvars {
        ipc_handler: ipc_handler.clone(),
        controller,
      });

    let delegate: Retained<Self> = unsafe { msg_send![super(delegate), init] };

    let is_first = with_registry(mtm, |registry| {
      registry.insert(controller_address, webview, ipc_handler)
    });
    if is_first {
      let proto_delegate = ProtocolObject::from_ref(&*delegate);
      // SAFETY: a documented `WKUserContentController` call on the main thread (`mtm`); an
      // Objective-C exception is caught rather than unwinding into Rust.
      unsafe {
        // this will increase the retain count of the delegate, which then stays alive until the
        // last web view on the controller removes it in `unregister`
        let _res = objc2::exception::catch(AssertUnwindSafe(|| {
          delegate
            .ivars()
            .controller
            .addScriptMessageHandler_name(proto_delegate, ns_string!(IPC_MESSAGE_HANDLER_NAME));
        }));
      }
    }

    delegate
  }

  /// Unregisters the web view at address `webview`. The last live web view on the controller
  /// removes the "ipc" script message handler, which releases the delegate added for it.
  pub fn unregister(&self, webview: usize, mtm: MainThreadMarker) {
    let controller = &self.ivars().controller;
    let controller_address = Retained::as_ptr(controller) as usize;
    let is_last = with_registry(mtm, |registry| registry.remove(controller_address, webview));
    if is_last {
      // SAFETY: a documented `WKUserContentController` call on the main thread (`mtm`).
      unsafe {
        controller.removeScriptMessageHandlerForName(ns_string!(IPC_MESSAGE_HANDLER_NAME));
      }
    }
  }
}
