// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! Drives the plugin through its VST3 interfaces, as a host would.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper, Interface};

struct Events(Vec<Event>);

impl Class for Events {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for Events {
    unsafe fn getEventCount(&self) -> i32 {
        self.0.len() as i32
    }
    unsafe fn getEvent(&self, index: i32, e: *mut Event) -> tresult {
        match self.0.get(index as usize) {
            Some(ev) => {
                *e = *ev;
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
    unsafe fn addEvent(&self, _e: *mut Event) -> tresult {
        kResultFalse
    }
}

#[derive(Default)]
struct Stream {
    data: RefCell<Vec<u8>>,
    pos: RefCell<usize>,
}

impl Class for Stream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for Stream {
    unsafe fn read(&self, buffer: *mut c_void, n: int32, read: *mut int32) -> tresult {
        let data = self.data.borrow();
        let mut pos = self.pos.borrow_mut();
        let k = (n as usize).min(data.len() - *pos);
        ptr::copy_nonoverlapping(data[*pos..].as_ptr(), buffer as *mut u8, k);
        *pos += k;
        if !read.is_null() {
            *read = k as i32;
        }
        kResultOk
    }
    unsafe fn write(&self, buffer: *mut c_void, n: int32, written: *mut int32) -> tresult {
        self.data.borrow_mut().extend_from_slice(std::slice::from_raw_parts(buffer as *const u8, n as usize));
        if !written.is_null() {
            *written = n;
        }
        kResultOk
    }
    unsafe fn seek(&self, pos: int64, _mode: int32, result: *mut int64) -> tresult {
        *self.pos.borrow_mut() = pos as usize;
        if !result.is_null() {
            *result = pos;
        }
        kResultOk
    }
    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        *pos = *self.pos.borrow() as i64;
        kResultOk
    }
}

fn note_on(pitch: i16, velocity: f32) -> Event {
    let mut e: Event = unsafe { std::mem::zeroed() };
    e.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
    e.__field0.noteOn = NoteOnEvent { channel: 0, pitch, tuning: 0.0, velocity, length: 0, noteId: -1 };
    e
}

fn note_off(pitch: i16) -> Event {
    let mut e: Event = unsafe { std::mem::zeroed() };
    e.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
    e.__field0.noteOff = NoteOffEvent { channel: 0, pitch, velocity: 0.0, noteId: -1, tuning: 0.0 };
    e
}

unsafe fn create(factory: &ComPtr<IPluginFactory>) -> ComPtr<IComponent> {
    let cid = midi_counter::vst3::MidiCounter::CID;
    let mut obj = ptr::null_mut();
    let r = factory.createInstance(cid.as_ptr(), IComponent::IID.as_ptr() as FIDString, &mut obj);
    assert_eq!(r, kResultOk);
    ComPtr::from_raw(obj as *mut IComponent).unwrap()
}

#[test]
fn host_round_trip() {
    let dir = std::env::temp_dir().join(format!("midi-counter-host-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("MIDI_COUNTER_DATA_DIR", &dir);

    unsafe {
        let factory = ComPtr::from_raw(midi_counter::vst3::GetPluginFactory()).unwrap();
        let mut info: PFactoryInfo = std::mem::zeroed();
        assert_eq!(factory.getFactoryInfo(&mut info), kResultOk);
        let vendor = std::ffi::CStr::from_ptr(info.vendor.as_ptr()).to_str().unwrap().to_string();
        assert_eq!(vendor, "Kaia");
        assert_eq!(factory.countClasses(), 1);
        let f2 = factory.cast::<IPluginFactory2>().unwrap();
        let mut ci2: PClassInfo2 = std::mem::zeroed();
        assert_eq!(f2.getClassInfo2(0, &mut ci2), kResultOk);
        assert_eq!(std::ffi::CStr::from_ptr(ci2.name.as_ptr()).to_str().unwrap(), "MIDI Counter");

        let component = create(&factory);
        assert_eq!(component.initialize(ptr::null_mut()), kResultOk);
        let processor = component.cast::<IAudioProcessor>().unwrap();
        let controller = component.cast::<IEditController>().unwrap();
        assert_eq!(component.getBusCount(MediaTypes_::kEvent as i32, BusDirections_::kInput as i32), 1);
        assert_eq!(component.getBusCount(MediaTypes_::kEvent as i32, BusDirections_::kOutput as i32), 0);
        assert_eq!(controller.getParameterCount(), 0);

        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: 64,
            sampleRate: 48000.0,
        };
        assert_eq!(processor.setupProcessing(&mut setup), kResultOk);
        assert_eq!(component.setActive(1), kResultOk);
        assert_eq!(processor.setProcessing(1), kResultOk);

        // Three real hits, one velocity-0 note-on (a note-off), a note-off, and a double trigger
        // of 42 (1 ms later, inside the default 30 ms retrigger guard).
        let mut retrigger = note_on(42, 0.1);
        retrigger.sampleOffset = 48;
        let events = ComWrapper::new(Events(vec![
            note_on(36, 1.0),
            note_on(38, 0.5),
            note_on(38, 0.0),
            note_off(36),
            note_on(42, 0.25),
            retrigger,
        ]));
        let events = events.to_com_ptr::<IEventList>().unwrap();

        let mut in_l: Vec<f32> = (0..64).map(|i| i as f32 * 0.01).collect();
        let mut in_r: Vec<f32> = (0..64).map(|i| -(i as f32) * 0.02).collect();
        let (mut out_l, mut out_r) = (vec![9.0f32; 64], vec![9.0f32; 64]);
        let mut in_ptrs = [in_l.as_mut_ptr(), in_r.as_mut_ptr()];
        let mut out_ptrs = [out_l.as_mut_ptr(), out_r.as_mut_ptr()];
        let mut inputs = AudioBusBuffers { numChannels: 2, silenceFlags: 0, __field0: AudioBusBuffers__type0 { channelBuffers32: in_ptrs.as_mut_ptr() } };
        let mut outputs = AudioBusBuffers { numChannels: 2, silenceFlags: 0, __field0: AudioBusBuffers__type0 { channelBuffers32: out_ptrs.as_mut_ptr() } };
        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: 64,
            numInputs: 1,
            numOutputs: 1,
            inputs: &mut inputs,
            outputs: &mut outputs,
            inputParameterChanges: ptr::null_mut(),
            outputParameterChanges: ptr::null_mut(),
            inputEvents: events.as_ptr(),
            outputEvents: ptr::null_mut(),
            processContext: ptr::null_mut(),
        };
        assert_eq!(processor.process(&mut data), kResultOk);
        assert_eq!(out_l, in_l, "audio must pass through unchanged");
        assert_eq!(out_r, in_r);

        // State: profile name round trip.
        let stream = ComWrapper::new(Stream::default()).to_com_ptr::<IBStream>().unwrap();
        assert_eq!(component.getState(stream.as_ptr()), kResultOk);

        assert_eq!(processor.setProcessing(0), kResultOk);
        assert_eq!(component.setActive(0), kResultOk);
        assert_eq!(component.terminate(), kResultOk);
        drop((processor, controller, component));

        let store = midi_counter::store::Store::new(dir.clone());
        let mut total = 0;
        let mut notes = Vec::new();
        let mut vels = Vec::new();
        for m in store.months("Default") {
            midi_counter::store::scan_file(&store.month_path("Default", m), 0, true, |l| {
                total += l.total;
                notes.extend_from_slice(&l.notes);
                vels.extend_from_slice(&l.vels);
            })
            .unwrap();
        }
        assert_eq!(total, 3);
        assert_eq!(notes, vec![(36, 1), (38, 1), (42, 1)]);
        assert_eq!(vels, vec![(32, 1), (64, 1), (127, 1)]);

        // A second instance restores the profile from the saved state.
        store.create_profile("Drums").unwrap();
        let component = create(&factory);
        component.initialize(ptr::null_mut());
        let s2 = ComWrapper::new(Stream::default());
        *s2.data.borrow_mut() = b"MIDICOUNTER1\nDrums\n".to_vec();
        let s2 = s2.to_com_ptr::<IBStream>().unwrap();
        assert_eq!(component.setState(s2.as_ptr()), kResultOk);
        let out = ComWrapper::new(Stream::default());
        let out_ptr = out.to_com_ptr::<IBStream>().unwrap();
        component.getState(out_ptr.as_ptr());
        assert_eq!(out.data.borrow().as_slice(), b"MIDICOUNTER1\nDrums\n");
        component.terminate();
    }
    let _ = std::fs::remove_dir_all(&dir);
}
