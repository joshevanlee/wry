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

use std::{cell::RefCell, collections::HashMap, mem, rc::Rc};

use http::Request;
use objc2_foundation::{MainThreadMarker, NSObject};
use objc2_web_kit::WKUserContentController;

/// The IPC handler of one web view.
pub type IpcHandler = Rc<dyn Fn(Request<String>)>;

/// Identity of a web view: its object address. An entry is removed while its web view is still
/// alive, so an address is never reused while registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WebViewAddress(usize);

impl WebViewAddress {
  pub fn of(web_view: &NSObject) -> Self {
    Self(web_view as *const NSObject as usize)
  }
}

/// Identity of a `WKUserContentController`: its object address. Its web views retain it, so
/// the address is never reused while one of them is registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ControllerAddress(usize);

impl ControllerAddress {
  pub fn of(controller: &WKUserContentController) -> Self {
    Self(controller as *const WKUserContentController as usize)
  }
}

#[derive(Debug)]
struct Entry<H> {
  controller: ControllerAddress,
  handler: H,
}

/// Result of [`IpcRegistry::insert`].
#[derive(Debug)]
pub struct Insertion<H> {
  /// The web view is the first live one on its controller: the caller adds the "ipc" handler.
  pub first_on_controller: bool,
  /// The handler this one replaced, for the caller to drop outside the registry borrow.
  pub replaced: Option<H>,
}

/// Result of [`IpcRegistry::remove`].
#[derive(Debug)]
pub struct Removal<H> {
  /// The web view was the last live one on its controller: the caller removes the "ipc" handler.
  pub last_on_controller: bool,
  /// The removed handler, for the caller to drop outside the registry borrow.
  pub handler: H,
}

#[derive(Debug)]
pub struct IpcRegistry<H> {
  entries: HashMap<WebViewAddress, Entry<H>>,
  controller_users: HashMap<ControllerAddress, usize>,
}

impl<H> Default for IpcRegistry<H> {
  fn default() -> Self {
    Self {
      entries: HashMap::new(),
      controller_users: HashMap::new(),
    }
  }
}

impl<H> IpcRegistry<H> {
  /// Registers `webview`'s handler on `controller`. Registering a web view again replaces its
  /// handler; its controller cannot change, as WebKit fixes it when the web view is created.
  pub fn insert(
    &mut self,
    controller: ControllerAddress,
    webview: WebViewAddress,
    handler: H,
  ) -> Insertion<H> {
    if let Some(entry) = self.entries.get_mut(&webview) {
      debug_assert_eq!(entry.controller, controller);
      return Insertion {
        first_on_controller: false,
        replaced: Some(mem::replace(&mut entry.handler, handler)),
      };
    }
    self.entries.insert(
      webview,
      Entry {
        controller,
        handler,
      },
    );
    let users = self.controller_users.entry(controller).or_insert(0);
    *users += 1;
    Insertion {
      first_on_controller: *users == 1,
      replaced: None,
    }
  }

  /// Unregisters `webview` from the controller it was registered on, or returns `None` if it
  /// is not registered.
  pub fn remove(&mut self, webview: WebViewAddress) -> Option<Removal<H>> {
    let Entry {
      controller,
      handler,
    } = self.entries.remove(&webview)?;
    let users = self.controller_users.get_mut(&controller)?;
    *users -= 1;
    let last_on_controller = *users == 0;
    if last_on_controller {
      self.controller_users.remove(&controller);
    }
    Some(Removal {
      last_on_controller,
      handler,
    })
  }

  /// Whether a live web view is registered on `controller`, i.e. wry has already added the
  /// "ipc" handler and its user scripts to it.
  pub fn is_shared(&self, controller: ControllerAddress) -> bool {
    self.controller_users.contains_key(&controller)
  }
}

impl<H: Clone> IpcRegistry<H> {
  /// The handler registered for `webview`, if any.
  pub fn handler(&self, webview: WebViewAddress) -> Option<H> {
    self
      .entries
      .get(&webview)
      .map(|entry| entry.handler.clone())
  }
}

thread_local! {
  static REGISTRY: RefCell<IpcRegistry<IpcHandler>> = RefCell::default();
}

/// Runs `f` with the main thread's registry. Taking a [`MainThreadMarker`] keeps the registry on
/// the main thread, where WebKit calls the script message handler. Do not call or drop a handler
/// inside `f`: a handler may create or drop web views, which needs the registry again.
pub fn with_registry<R>(
  _mtm: MainThreadMarker,
  f: impl FnOnce(&mut IpcRegistry<IpcHandler>) -> R,
) -> R {
  REGISTRY.with(|registry| f(&mut registry.borrow_mut()))
}

#[cfg(test)]
mod tests {
  use super::{ControllerAddress, IpcRegistry, WebViewAddress};

  const CONTROLLER: ControllerAddress = ControllerAddress(0x1000);
  const OTHER_CONTROLLER: ControllerAddress = ControllerAddress(0x1100);
  const MAIN: WebViewAddress = WebViewAddress(0x2000);
  const POP_OUT: WebViewAddress = WebViewAddress(0x3000);
  const NEXT_POP_OUT: WebViewAddress = WebViewAddress(0x4000);
  const OTHER_WEB_VIEW: WebViewAddress = WebViewAddress(0x5000);
  const UNREGISTERED: WebViewAddress = WebViewAddress(0x9000);

  fn main_and_pop_out() -> IpcRegistry<&'static str> {
    let mut registry = IpcRegistry::default();
    registry.insert(CONTROLLER, MAIN, "main");
    registry.insert(CONTROLLER, POP_OUT, "pop-out");
    registry
  }

  #[test]
  fn only_the_first_web_view_on_a_controller_adds_the_handler() {
    let mut registry = IpcRegistry::default();
    assert!(
      registry
        .insert(CONTROLLER, MAIN, "main")
        .first_on_controller
    );
    assert!(
      !registry
        .insert(CONTROLLER, POP_OUT, "pop-out")
        .first_on_controller
    );
  }

  #[test]
  fn controllers_are_counted_separately() {
    let mut registry = main_and_pop_out();
    assert!(
      registry
        .insert(OTHER_CONTROLLER, OTHER_WEB_VIEW, "other")
        .first_on_controller
    );
    assert!(registry.remove(OTHER_WEB_VIEW).unwrap().last_on_controller);
    assert!(registry.is_shared(CONTROLLER));
  }

  #[test]
  fn routes_to_the_sender_handler() {
    let registry = main_and_pop_out();
    assert_eq!(registry.handler(MAIN), Some("main"));
    assert_eq!(registry.handler(POP_OUT), Some("pop-out"));
    assert_eq!(registry.handler(UNREGISTERED), None);
  }

  #[test]
  fn closing_a_pop_out_keeps_the_shared_handler() {
    let mut registry = main_and_pop_out();
    let removal = registry.remove(POP_OUT).unwrap();
    assert!(!removal.last_on_controller);
    assert_eq!(removal.handler, "pop-out");
    assert_eq!(registry.handler(POP_OUT), None);
    assert_eq!(registry.handler(MAIN), Some("main"));
    assert!(
      !registry
        .insert(CONTROLLER, NEXT_POP_OUT, "next")
        .first_on_controller
    );
  }

  #[test]
  fn the_last_web_view_removes_the_handler_in_any_order() {
    let mut registry = main_and_pop_out();
    assert!(!registry.remove(MAIN).unwrap().last_on_controller);
    assert!(registry.remove(POP_OUT).unwrap().last_on_controller);
    assert!(!registry.is_shared(CONTROLLER));
    assert!(
      registry
        .insert(CONTROLLER, MAIN, "main")
        .first_on_controller
    );
  }

  #[test]
  fn remove_uses_the_controller_the_web_view_was_registered_on() {
    let mut registry = main_and_pop_out();
    registry.insert(OTHER_CONTROLLER, OTHER_WEB_VIEW, "other");
    registry.remove(MAIN);
    registry.remove(POP_OUT);
    assert!(!registry.is_shared(CONTROLLER));
    assert!(registry.is_shared(OTHER_CONTROLLER));
  }

  #[test]
  fn is_shared_while_a_web_view_is_registered() {
    let mut registry = IpcRegistry::default();
    assert!(!registry.is_shared(CONTROLLER));
    registry.insert(CONTROLLER, MAIN, "main");
    assert!(registry.is_shared(CONTROLLER));
    registry.remove(MAIN);
    assert!(!registry.is_shared(CONTROLLER));
  }

  #[test]
  fn registering_again_returns_the_replaced_handler() {
    let mut registry = IpcRegistry::default();
    registry.insert(CONTROLLER, MAIN, "main");
    let insertion = registry.insert(CONTROLLER, MAIN, "main again");
    assert!(!insertion.first_on_controller);
    assert_eq!(insertion.replaced, Some("main"));
    assert_eq!(registry.handler(MAIN), Some("main again"));
    assert!(registry.remove(MAIN).unwrap().last_on_controller);
  }

  #[test]
  fn removing_an_unregistered_web_view_changes_nothing() {
    let mut registry = main_and_pop_out();
    assert!(registry.remove(UNREGISTERED).is_none());
    registry.remove(MAIN);
    assert!(registry.remove(MAIN).is_none());
    assert!(registry.remove(POP_OUT).unwrap().last_on_controller);
  }
}
