// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! The VST3 editor view: embeds the baseview window into the host's X11 window.

use std::cell::{Cell, RefCell};
use std::ffi::{c_void, CStr};
use std::num::NonZeroU32;
use std::sync::Arc;

use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle, XlibWindowHandle};
use vst3::Class;
use vst3::Steinberg::*;

use crate::session::Shared;
use crate::ui::{self, theme, Editor, Options};

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

    fn size(&self) -> (i32, i32) {
        let s = self.scale.get();
        ((theme::WIDTH as f32 * s).round() as i32, (theme::HEIGHT as f32 * s).round() as i32)
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
        for settings in ui::window_settings(self.scale.get() as f64) {
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

    unsafe fn onSize(&self, _new_size: *mut ViewRect) -> tresult {
        kResultOk
    }

    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setFrame(&self, _frame: *mut IPlugFrame) -> tresult {
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        kResultFalse
    }

    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        let Some(rect) = rect.as_mut() else { return kInvalidArgument };
        let (w, h) = self.size();
        rect.right = rect.left + w;
        rect.bottom = rect.top + h;
        kResultOk
    }
}

impl IPlugViewContentScaleSupportTrait for View {
    unsafe fn setContentScaleFactor(&self, factor: IPlugViewContentScaleSupport_::ScaleFactor) -> tresult {
        // Applies from the next time the window opens; the window has a fixed size while open.
        if self.window.borrow().is_some() {
            return kResultFalse;
        }
        self.scale.set(factor.clamp(1.0, 4.0));
        kResultOk
    }
}
