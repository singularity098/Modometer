// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! Acts as a VST3 host's GUI: opens a plain parent window, then asks the plugin's IPlugView to
//! embed itself into it, as Reaper does. For checking the editor under X11.
//! `cargo run --example embed_test -- <seconds> [--resize]`
//!
//! With `--resize`, halfway through it grows the host window to 1200 px wide, as a user dragging
//! its edge would: it asks the view for a valid size, resizes the parent, then calls `onSize`.

use std::sync::mpsc;

use baseview::{Event, EventStatus, HandlerError, WindowContext, WindowHandler, WindowSettings, WindowSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::ComPtr;

struct Host;
impl WindowHandler for Host {
    fn draw(&self) -> Result<(), HandlerError> {
        Ok(())
    }
    fn resized(&self, _: WindowSize) -> Result<(), HandlerError> {
        Ok(())
    }
    fn on_event(&self, _: Event) -> EventStatus {
        EventStatus::Ignored
    }
}

fn main() {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    let resize = std::env::args().any(|a| a == "--resize");
    let (tx, rx) = mpsc::channel();
    let settings = WindowSettings::new()
        .with_title("Fake host")
        .with_size(baseview::dpi::PhysicalSize::new(800u32, 450u32));
    let parent = baseview::Window::create(settings, move |ctx: WindowContext| {
        if let Ok(h) = ctx.window_handle() {
            if let RawWindowHandle::Xlib(x) = h.as_raw() {
                let _ = tx.send(x.window);
            } else if let RawWindowHandle::Xcb(x) = h.as_raw() {
                let _ = tx.send(x.window.get() as u64);
            }
        }
        Ok(Host)
    })
    .expect("parent window");
    parent.show().unwrap();
    let id = rx.recv().expect("parent window id");

    unsafe {
        let factory = ComPtr::from_raw(modometer::vst3::GetPluginFactory()).unwrap();
        let cid = modometer::vst3::Modometer::CID;
        let mut obj = std::ptr::null_mut();
        use vst3::Interface;
        factory.createInstance(cid.as_ptr(), IComponent::IID.as_ptr() as FIDString, &mut obj);
        let component = ComPtr::from_raw(obj as *mut IComponent).unwrap();
        component.initialize(std::ptr::null_mut());
        let controller = component.cast::<IEditController>().unwrap();
        let view = ComPtr::from_raw(controller.createView(c"editor".as_ptr())).expect("view");
        let mut rect: ViewRect = std::mem::zeroed();
        view.getSize(&mut rect);
        println!("view size {}x{}", rect.right - rect.left, rect.bottom - rect.top);
        let r = view.attached(id as usize as *mut std::ffi::c_void, c"X11EmbedWindowID".as_ptr());
        println!("attached: {}", if r == kResultOk { "ok" } else { "FAILED" });
        if resize {
            std::thread::sleep(std::time::Duration::from_millis(seconds * 500));
            let mut rect = ViewRect { left: 0, top: 0, right: 1200, bottom: 450 };
            view.checkSizeConstraint(&mut rect);
            println!("resizing to {}x{}", rect.right, rect.bottom);
            let _ = parent.resize(baseview::dpi::PhysicalSize::new(rect.right as u32, rect.bottom as u32));
            view.onSize(&mut rect);
            std::thread::sleep(std::time::Duration::from_millis(seconds * 500));
        } else {
            std::thread::sleep(std::time::Duration::from_secs(seconds));
        }
        view.removed();
        println!("removed");
        drop(view);
        component.terminate();
    }
    drop(parent);
}
