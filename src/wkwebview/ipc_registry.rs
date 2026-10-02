// Copyright 2020-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! Bookkeeping for web views that share one `WKUserContentController`.
//!
//! A configuration passed to `with_webview_configuration` (Tauri passes the opener's
//! configuration to pop-out windows) shares its `WKUserContentController` with the opener.
//! WebKit accepts a script message handler name only once per controller
//! (<https://developer.apple.com/documentation/webkit/wkusercontentcontroller/add(_:name:)>),
//! so one "ipc" handler serves every web view on the controller. This registry maps each
//! web view to its own IPC handler, so a message can be routed by
//! `WKScriptMessage.webView` ("The web view that sent the message",
//! <https://developer.apple.com/documentation/webkit/wkscriptmessage/webview>), and counts
//! the live web views per controller, so the shared "ipc" handler is added by the first and
//! removed by the last.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use http::Request;
use objc2_foundation::MainThreadMarker;

/// The IPC handler of one web view.
pub type IpcHandler = Rc<dyn Fn(Request<String>)>;

/// Web view and controller identities are their object addresses. An entry is removed while
/// its web view and controller are still alive, so an address is never reused while registered.
#[derive(Debug)]
pub struct IpcRegistry<H> {
  handlers: HashMap<usize, H>,
  controller_users: HashMap<usize, usize>,
}

impl<H> Default for IpcRegistry<H> {
  fn default() -> Self {
    Self {
      handlers: HashMap::new(),
      controller_users: HashMap::new(),
    }
  }
}

impl<H: Clone> IpcRegistry<H> {
  /// Registers `webview`'s handler on `controller`. Returns `true` when `webview` is the first
  /// live web view on `controller`, i.e. the caller must add the "ipc" script message handler.
  pub fn insert(&mut self, controller: usize, webview: usize, handler: H) -> bool {
    if self.handlers.insert(webview, handler).is_some() {
      // Already registered: the handler is replaced and the count is unchanged.
      return false;
    }
    let users = self.controller_users.entry(controller).or_insert(0);
    *users += 1;
    *users == 1
  }

  /// Unregisters `webview` from `controller`. Returns `true` when it was the last live web view
  /// on `controller`, i.e. the caller must remove the "ipc" script message handler.
  pub fn remove(&mut self, controller: usize, webview: usize) -> bool {
    if self.handlers.remove(&webview).is_none() {
      return false;
    }
    match self.controller_users.get_mut(&controller) {
      Some(users) if *users > 1 => {
        *users -= 1;
        false
      }
      Some(_) => {
        self.controller_users.remove(&controller);
        true
      }
      None => false,
    }
  }

  /// The handler registered for `webview`, if any.
  pub fn handler(&self, webview: usize) -> Option<H> {
    self.handlers.get(&webview).cloned()
  }
}

thread_local! {
  static REGISTRY: RefCell<IpcRegistry<IpcHandler>> = RefCell::default();
}

/// Runs `f` with the main thread's registry. Taking a [`MainThreadMarker`] keeps the registry on
/// the main thread, where WebKit calls the script message handler. Do not call a handler inside
/// `f`: a handler may create or drop web views, which needs the registry again.
pub fn with_registry<R>(
  _mtm: MainThreadMarker,
  f: impl FnOnce(&mut IpcRegistry<IpcHandler>) -> R,
) -> R {
  REGISTRY.with(|registry| f(&mut registry.borrow_mut()))
}

#[cfg(test)]
mod tests {
  use super::IpcRegistry;

  const CONTROLLER: usize = 0x1000;
  const MAIN: usize = 0x2000;
  const POP_OUT: usize = 0x3000;

  #[test]
  fn first_web_view_on_a_controller_adds_the_handler() {
    let mut registry = IpcRegistry::default();
    assert!(registry.insert(CONTROLLER, MAIN, "main"));
    assert!(!registry.insert(CONTROLLER, POP_OUT, "pop-out"));
    assert!(registry.insert(0x1001, 0x4000, "other controller"));
  }

  #[test]
  fn routes_to_the_sender_handler() {
    let mut registry = IpcRegistry::default();
    registry.insert(CONTROLLER, MAIN, "main");
    registry.insert(CONTROLLER, POP_OUT, "pop-out");
    assert_eq!(registry.handler(MAIN), Some("main"));
    assert_eq!(registry.handler(POP_OUT), Some("pop-out"));
    assert_eq!(registry.handler(0x9999), None);
  }

  #[test]
  fn closing_a_pop_out_keeps_the_shared_handler() {
    let mut registry = IpcRegistry::default();
    registry.insert(CONTROLLER, MAIN, "main");
    registry.insert(CONTROLLER, POP_OUT, "pop-out");
    assert!(!registry.remove(CONTROLLER, POP_OUT));
    assert_eq!(registry.handler(POP_OUT), None);
    assert_eq!(registry.handler(MAIN), Some("main"));
    // The next pop-out still shares the handler that is already added.
    assert!(!registry.insert(CONTROLLER, 0x5000, "next pop-out"));
  }

  #[test]
  fn last_web_view_removes_the_handler_in_any_order() {
    let mut registry = IpcRegistry::default();
    registry.insert(CONTROLLER, MAIN, "main");
    registry.insert(CONTROLLER, POP_OUT, "pop-out");
    assert!(!registry.remove(CONTROLLER, MAIN));
    assert!(registry.remove(CONTROLLER, POP_OUT));
    // A later web view on the same controller adds the handler again.
    assert!(registry.insert(CONTROLLER, MAIN, "main"));
  }

  #[test]
  fn repeated_or_unknown_entries_do_not_change_the_count() {
    let mut registry = IpcRegistry::default();
    registry.insert(CONTROLLER, MAIN, "main");
    assert!(!registry.insert(CONTROLLER, MAIN, "main again"));
    assert_eq!(registry.handler(MAIN), Some("main again"));
    assert!(!registry.remove(CONTROLLER, POP_OUT));
    assert!(registry.remove(CONTROLLER, MAIN));
    assert!(!registry.remove(CONTROLLER, MAIN));
  }
}
