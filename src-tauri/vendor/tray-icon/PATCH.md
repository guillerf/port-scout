# macOS 27 tray click compatibility

This directory vendors the unmodified published `tray-icon` 0.21.3 crate except
for `src/platform_impl/macos/mod.rs`, which backports the applicable changes from
[upstream PR #365](https://github.com/tauri-apps/tray-icon/pull/365)
(merged commit `42eb44e`, 2026-09-16).

macOS 27 intercepts left clicks while an NSMenu is permanently attached to the
status item. Keep the menu detached until a menu click needs to present it, then
detach it again. Clone the retained menu before entering the nested AppKit event
loop so a re-entrant `set_menu` cannot panic on a borrowed RefCell.

The upstream change to `show_menu` is inapplicable: 0.21.3 has no such API.
The right-click policy remains the 0.21.3 policy. There are no changes to other
platforms, dependency versions, or public APIs. MIT and Apache licenses are
included from the original crate.

The fix shipped in 0.25.1, outside Tauri 2.10's `^0.21` requirement. As of
2026-09-20, the latest stable Tauri (2.11.6) still requires `^0.24`, so updating
Tauri alone does not include the fix. Remove this vendor directory and the Cargo
patch once the selected Tauri release uses a compatible fixed tray-icon version.
Do not update the registry copy locally.

Validation: build the app on macOS, then verify left-click opens/closes the
popover, right-click opens its menu, and Show Port Scout always opens the window.
Native event forwarding requires a GUI smoke test; Rust unit tests cannot
simulate AppKit delivery.
