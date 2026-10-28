//! CarterCanvas for iPhone and iPad.
//!
//! The Swift host owns the app lifecycle, the `CAMetalLayer` view and the native sheets (document
//! picker, share sheet). This crate runs the unchanged PhotoCraft engine and interface on that
//! layer: [`host::Host`] drives `PhotocraftApp` through the same `eframe::App` hooks eframe calls,
//! paints egui's output with egui-wgpu on a wgpu surface made from the layer, and turns UIKit
//! touches, Apple Pencil samples and hardware keys into egui input ([`input`], [`keys`]).
//! Platform services (file dialogs, clipboard, preferences, autosave) are the host callbacks of
//! [`services`]. The C ABI the Swift host calls is in [`ffi`].

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod callbacks;
#[path = "../../photocraft/src/control_server.rs"]
mod control_server;
pub mod ffi;
pub mod host;
pub mod input;
pub mod keys;
pub mod logging;
pub mod services;
