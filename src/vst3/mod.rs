// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! The VST3 plugin, on the MIT-licensed `vst3` bindings.
//!
//! One class is both the processor and the controller (a "single component effect"). It has a
//! stereo audio bus, copied through untouched, and one MIDI input bus. It declares no MIDI output,
//! so hosts pass the incoming MIDI on to the next plugin unchanged.

mod view;

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void};
use std::ptr;
use std::sync::Arc;

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{uid, Class, ComRef, ComWrapper};

use crate::session::{Guard, Shared};
use crate::store::{self, Store};

pub const PLUGIN_NAME: &str = "MIDI Counter";
pub const VENDOR: &str = "Kaia";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const URL: &str = "https://github.com/singularity098/midi-counter";
const STATE_MAGIC: &[u8] = b"MIDICOUNTER1\n";

pub fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let n = src.len().min(dst.len().saturating_sub(1));
    for (d, s) in dst.iter_mut().zip(src.bytes().take(n)) {
        *d = s as c_char;
    }
    if let Some(end) = dst.get_mut(n) {
        *end = 0;
    }
}

pub fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (d, s) in dst.iter_mut().zip(src.encode_utf16()) {
        *d = s as TChar;
        len += 1;
    }
    let end = len.min(dst.len().saturating_sub(1));
    if let Some(e) = dst.get_mut(end) {
        *e = 0;
    }
}

pub struct MidiCounter {
    shared: Arc<Shared>,
    sample64: Cell<bool>,
    // Audio thread only: the retrigger guard, a sample clock and the sample rate.
    guard: RefCell<Guard>,
    clock: Cell<u64>,
    rate: Cell<f64>,
}

// The host calls the controller and processor parts from its own threads as the VST3 threading
// rules allow; the only state shared between threads lives in `Shared`, which is Sync.
unsafe impl Send for MidiCounter {}
unsafe impl Sync for MidiCounter {}

impl Class for MidiCounter {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements, IEditController);
}

impl MidiCounter {
    pub const CID: TUID = uid(0x4D494449, 0x4B616961, 0x436F756E, 0x74657231);

    pub fn new() -> MidiCounter {
        MidiCounter {
            shared: Shared::new(Store::new(store::default_root())),
            sample64: Cell::new(false),
            guard: RefCell::new(Guard::default()),
            clock: Cell::new(0),
            rate: Cell::new(48000.0),
        }
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }
}

impl Default for MidiCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl IPluginBaseTrait for MidiCounter {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        self.shared.start();
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        self.shared.shutdown();
        kResultOk
    }
}

unsafe fn read_stream(stream: *mut IBStream, max: usize) -> Option<Vec<u8>> {
    let stream = ComRef::from_raw(stream)?;
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    while out.len() < max {
        let mut read = 0;
        if stream.read(buf.as_mut_ptr() as *mut c_void, buf.len() as i32, &mut read) != kResultOk || read <= 0 {
            break;
        }
        out.extend_from_slice(&buf[..read as usize]);
    }
    Some(out)
}

impl IComponentTrait for MidiCounter {
    unsafe fn getControllerClassId(&self, _class_id: *mut TUID) -> tresult {
        // Single component: the controller is this object.
        kNotImplemented
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media_type: MediaType, dir: BusDirection) -> i32 {
        match (media_type as MediaTypes, dir as BusDirections) {
            (MediaTypes_::kAudio, _) => 1,
            (MediaTypes_::kEvent, BusDirections_::kInput) => 1,
            _ => 0,
        }
    }

    unsafe fn getBusInfo(&self, media_type: MediaType, dir: BusDirection, index: i32, bus: *mut BusInfo) -> tresult {
        if index != 0 || bus.is_null() {
            return kInvalidArgument;
        }
        let bus = &mut *bus;
        bus.mediaType = media_type;
        bus.direction = dir;
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive;
        match (media_type as MediaTypes, dir as BusDirections) {
            (MediaTypes_::kAudio, d) => {
                bus.channelCount = 2;
                copy_wstring(if d == BusDirections_::kInput { "Input" } else { "Output" }, &mut bus.name);
                kResultOk
            }
            (MediaTypes_::kEvent, BusDirections_::kInput) => {
                bus.channelCount = 16;
                copy_wstring("MIDI In", &mut bus.name);
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn getRoutingInfo(&self, _in_info: *mut RoutingInfo, _out_info: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(&self, _media_type: MediaType, _dir: BusDirection, _index: i32, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        if state == 0 {
            // Deactivated (e.g. the track was disabled): a good moment to save.
            self.shared.save();
        }
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let Some(bytes) = read_stream(state, 4096) else { return kInvalidArgument };
        if let Some(name) = bytes.strip_prefix(STATE_MAGIC) {
            // The profile name, then optional "key value" lines (older versions wrote only the name).
            let text = String::from_utf8_lossy(name);
            let mut lines = text.lines();
            let name = lines.next().unwrap_or("").trim();
            if !name.is_empty() {
                self.shared.select_profile(name);
            }
            for line in lines {
                if let Some(z) = line.strip_prefix("zoom ").and_then(|z| z.trim().parse::<f32>().ok()) {
                    self.shared.set_zoom(z);
                }
            }
        }
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = ComRef::from_raw(state) else { return kInvalidArgument };
        let mut bytes = STATE_MAGIC.to_vec();
        bytes.extend_from_slice(self.shared.profile_name().as_bytes());
        bytes.push(b'\n');
        let zoom = self.shared.zoom();
        if (zoom - 1.0).abs() > 0.001 {
            bytes.extend_from_slice(format!("zoom {zoom:.3}\n").as_bytes());
        }
        let mut written = 0;
        if stream.write(bytes.as_ptr() as *mut c_void, bytes.len() as i32, &mut written) == kResultOk {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IAudioProcessorTrait for MidiCounter {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if num_ins == 1 && num_outs == 1 && *inputs == SpeakerArr::kStereo && *outputs == SpeakerArr::kStereo {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn getBusArrangement(&self, _dir: BusDirection, index: i32, arr: *mut SpeakerArrangement) -> tresult {
        if index != 0 || arr.is_null() {
            return kInvalidArgument;
        }
        *arr = SpeakerArr::kStereo;
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        match size as SymbolicSampleSizes {
            SymbolicSampleSizes_::kSample32 | SymbolicSampleSizes_::kSample64 => kResultOk,
            _ => kInvalidArgument,
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if let Some(setup) = setup.as_ref() {
            self.sample64.set(setup.symbolicSampleSize == SymbolicSampleSizes_::kSample64 as i32);
            if setup.sampleRate > 0.0 {
                self.rate.set(setup.sampleRate);
            }
        }
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        // The clock stops while not processing; forget old hits so a gap can't look short.
        if let Ok(mut g) = self.guard.try_borrow_mut() {
            g.reset();
        }
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let Some(data) = data.as_ref() else { return kResultOk };

        // Count note-ons (note-on with velocity 0 is a note-off), through the retrigger guard.
        let clock = self.clock.get();
        if let (Some(events), Ok(mut guard)) = (ComRef::from_raw(data.inputEvents), self.guard.try_borrow_mut()) {
            let mut minute = None;
            for i in 0..events.getEventCount() {
                let mut e: Event = std::mem::zeroed();
                if events.getEvent(i, &mut e) != kResultOk || e.r#type != Event_::EventTypes_::kNoteOnEvent as u16 {
                    continue;
                }
                let on = e.__field0.noteOn;
                let vel = (on.velocity * 127.0).round().clamp(0.0, 127.0) as u8;
                if vel == 0 || !(0..128).contains(&on.pitch) {
                    continue;
                }
                let m = *minute.get_or_insert_with(|| store::now_minute() as u32);
                let at = clock + e.sampleOffset.max(0) as u64;
                self.shared.note_on(&mut guard, at, self.rate.get(), m, on.pitch as u8, vel);
            }
        }
        self.clock.set(clock + data.numSamples.max(0) as u64);

        // Audio through, untouched.
        let n = data.numSamples.max(0) as usize;
        if data.numInputs >= 1 && data.numOutputs >= 1 && n > 0 && !data.inputs.is_null() && !data.outputs.is_null() {
            let (inp, out) = (&*data.inputs, &mut *data.outputs);
            let channels = inp.numChannels.min(out.numChannels).max(0) as usize;
            out.silenceFlags = inp.silenceFlags;
            for ch in 0..channels {
                if data.symbolicSampleSize == SymbolicSampleSizes_::kSample64 as i32 {
                    let (i, o) = (*inp.__field0.channelBuffers64.add(ch), *out.__field0.channelBuffers64.add(ch));
                    if !i.is_null() && !o.is_null() && i != o {
                        ptr::copy_nonoverlapping(i, o, n);
                    }
                } else {
                    let (i, o) = (*inp.__field0.channelBuffers32.add(ch), *out.__field0.channelBuffers32.add(ch));
                    if !i.is_null() && !o.is_null() && i != o {
                        ptr::copy_nonoverlapping(i, o, n);
                    }
                }
            }
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for MidiCounter {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

impl IEditControllerTrait for MidiCounter {
    unsafe fn setComponentState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> i32 {
        0
    }

    unsafe fn getParameterInfo(&self, _index: i32, _info: *mut ParameterInfo) -> tresult {
        kInvalidArgument
    }

    unsafe fn getParamStringByValue(&self, _id: u32, _value: f64, _string: *mut String128) -> tresult {
        kInvalidArgument
    }

    unsafe fn getParamValueByString(&self, _id: u32, _string: *mut TChar, _value: *mut f64) -> tresult {
        kInvalidArgument
    }

    unsafe fn normalizedParamToPlain(&self, _id: u32, value: f64) -> f64 {
        value
    }

    unsafe fn plainParamToNormalized(&self, _id: u32, value: f64) -> f64 {
        value
    }

    unsafe fn getParamNormalized(&self, _id: u32) -> f64 {
        0.0
    }

    unsafe fn setParamNormalized(&self, _id: u32, _value: f64) -> tresult {
        kInvalidArgument
    }

    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
        kResultOk
    }

    unsafe fn createView(&self, name: FIDString) -> *mut IPlugView {
        if !name.is_null() && std::ffi::CStr::from_ptr(name).to_bytes() != b"editor" {
            return ptr::null_mut();
        }
        ComWrapper::new(view::View::new(Arc::clone(&self.shared)))
            .to_com_ptr::<IPlugView>()
            .map(|p| p.into_raw())
            .unwrap_or(ptr::null_mut())
    }
}

pub struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let Some(info) = info.as_mut() else { return kInvalidArgument };
        copy_cstring(VENDOR, &mut info.vendor);
        copy_cstring(URL, &mut info.url);
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        1
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let Some(info) = info.as_mut() else { return kInvalidArgument };
        if index != 0 {
            return kInvalidArgument;
        }
        info.cid = MidiCounter::CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(PLUGIN_NAME, &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
        if cid.is_null() || obj.is_null() || *(cid as *const TUID) != MidiCounter::CID {
            return kInvalidArgument;
        }
        let Some(instance) = ComWrapper::new(MidiCounter::new()).to_com_ptr::<FUnknown>() else {
            return kInvalidArgument;
        };
        let ptr = instance.as_ptr();
        ((*(*ptr).vtbl).queryInterface)(ptr, iid as *const TUID, obj)
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        let Some(info) = info.as_mut() else { return kInvalidArgument };
        if index != 0 {
            return kInvalidArgument;
        }
        info.cid = MidiCounter::CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(PLUGIN_NAME, &mut info.name);
        info.classFlags = 0;
        copy_cstring("Fx|Analyzer", &mut info.subCategories);
        copy_cstring(VENDOR, &mut info.vendor);
        copy_cstring(VERSION, &mut info.version);
        copy_cstring("VST 3.7.12", &mut info.sdkVersion);
        kResultOk
    }
}

#[no_mangle]
pub extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory).to_com_ptr::<IPluginFactory>().map(|p| p.into_raw()).unwrap_or(ptr::null_mut())
}

#[cfg(target_os = "linux")]
#[no_mangle]
pub extern "system" fn ModuleEntry(_library_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[no_mangle]
pub extern "system" fn ModuleExit() -> bool {
    true
}
