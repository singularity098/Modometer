// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! The VST3 editor view: embeds the baseview window into the host's X11 window.
//!
//! The view is resizable to any shape: the host asks
//! [`checkSizeConstraint`](IPlugViewTrait::checkSizeConstraint) while the user drags its window's
//! edge (we only keep each side between 0.75× and 3× of 800 × 450), then calls `onSize`, and the
//! editor scales and stretches its layout to fill it. The size is kept in the instance's state.

use std::cell::{Cell, RefCell};
use std::ffi::{c_void, CStr};
use std::num::NonZeroU32;
use std::sync::Arc;

use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle, XlibWindowHandle};
use vst3::Class;
use vst3::Steinberg::*;

use crate::session::{self, Shared};
use crate::ui::{self, Editor, Options};

pub struct View {
    shared: Arc<Shared>,
    window: RefCell<Option<baseview::Window>>,
    scale: Cell<f32>,
}

// VST3 calls the view only from the host's UI thread.
unsafe impl Send for View {}
unsafe impl Sync for View {}

impl Class for View {
    type Interfaces = (IPlugView, IPlugViewContentScaleSupport);
}

impl View {
    pub fn new(shared: Arc<Shared>) -> View {
        View { shared, window: RefCell::new(None), scale: Cell::new(1.0) }
    }

    /// A size at a content scale of 1 in physical pixels.
    fn physical(&self, (w, h): (f32, f32)) -> (i32, i32) {
        let s = self.scale.get();
        ((w * s).round() as i32, (h * s).round() as i32)
    }

    fn size(&self) -> (i32, i32) {
        self.physical(self.shared.editor_size())
    }

    /// A size in physical pixels at a content scale of 1.
    fn unscaled(&self, w: i32, h: i32) -> (f32, f32) {
        let s = self.scale.get();
        (w as f32 / s, h as f32 / s)
    }
}

struct Parent(u64);

impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = RawWindowHandle::Xlib(XlibWindowHandle::new(self.0 as _));
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

fn is_x11(t: FIDString) -> bool {
    !t.is_null() && unsafe { CStr::from_ptr(t) }.to_bytes() == b"X11EmbedWindowID"
}

impl IPlugViewTrait for View {
    unsafe fn isPlatformTypeSupported(&self, t: FIDString) -> tresult {
        if is_x11(t) {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn attached(&self, parent: *mut c_void, t: FIDString) -> tresult {
        if !is_x11(t) || NonZeroU32::new(parent as usize as u32).is_none() {
            return kResultFalse;
        }
        let parent = Parent(parent as usize as u64);
        let (w, h) = self.size();
        for settings in ui::window_settings(w.max(1) as u32, h.max(1) as u32) {
            let settings = settings.with_parent(&parent);
            let shared = Arc::clone(&self.shared);
            if let Ok(window) = baseview::Window::create(settings, move |ctx| Editor::new(ctx, shared, Options::default())) {
                let _ = window.show();
                *self.window.borrow_mut() = Some(window);
                return kResultOk;
            }
        }
        kResultFalse
    }

    unsafe fn removed(&self) -> tresult {
        drop(self.window.borrow_mut().take());
        kResultOk
    }

    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyDown(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyUp(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }

    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        let Some(size) = size.as_mut() else { return kInvalidArgument };
        let (w, h) = self.size();
        *size = ViewRect { left: 0, top: 0, right: w, bottom: h };
        kResultOk
    }

    unsafe fn onSize(&self, new_size: *mut ViewRect) -> tresult {
        let Some(r) = new_size.as_ref() else { return kInvalidArgument };
        let (w, h) = ((r.right - r.left).max(1), (r.bottom - r.top).max(1));
        // Take whatever the host gives (the editor fills any shape), and remember it.
        let (uw, uh) = self.unscaled(w, h);
        self.shared.set_editor_size(uw, uh);
        if let Some(window) = self.window.borrow().as_ref() {
            let _ = window.resize(baseview::dpi::PhysicalSize::new(w as u32, h as u32));
        }
        kResultOk
    }

    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setFrame(&self, _frame: *mut IPlugFrame) -> tresult {
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        kResultTrue
    }

    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        let Some(rect) = rect.as_mut() else { return kInvalidArgument };
        let (uw, uh) = self.unscaled(rect.right - rect.left, rect.bottom - rect.top);
        let (w, h) = self.physical(session::clamp_size(uw, uh));
        rect.right = rect.left + w;
        rect.bottom = rect.top + h;
        kResultOk
    }
}

impl IPlugViewContentScaleSupportTrait for View {
    unsafe fn setContentScaleFactor(&self, factor: IPlugViewContentScaleSupport_::ScaleFactor) -> tresult {
        // Applies from the next time the window opens.
        if self.window.borrow().is_some() {
            return kResultFalse;
        }
        self.scale.set(factor.clamp(1.0, 4.0));
        kResultOk
    }
}
