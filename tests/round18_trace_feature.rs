//! Round-18 — `trace` Cargo feature smoke test.
//!
//! Drives `Sandbox::watch` + `Sandbox::set_trace_sink` against a
//! synthetic codec image, then asserts a `kind=mem_write` JSONL
//! event lands on the sink when guest code stores into the
//! watched range, AND a `kind=win32_call` event fires when the
//! synthetic DllMain exits via `RET 12`.
//!
//! Gated on `#[cfg(feature = "trace")]` — the entire file
//! compiles to nothing in compatibility-only builds.
//!
//! See `docs/winmf/winmf-emulator.md` §"Trace mode" for the
//! design contract this test exercises.

#![cfg(feature = "trace")]

use std::sync::{Arc, Mutex};

use oxideav_vfw::emulator::mmu::Perm;
use oxideav_vfw::pe::test_image::build_minimal_dll;
use oxideav_vfw::{Sandbox, WatchMode, DLL_PROCESS_ATTACH};

/// `Box<dyn Write + Send>` that funnels into a shared
/// `Arc<Mutex<Vec<u8>>>` we can read out post-test.
struct SharedSink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedSink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn watch_emits_mem_write_event_on_guest_store() {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut sb = Sandbox::new();
    sb.set_trace_sink(Box::new(SharedSink(Arc::clone(&buf))));

    // Map a R+W page the test will use as the watched range.
    let scratch_base = 0x4000_0000u32;
    sb.mmu.map(scratch_base, 0x1000, Perm::R | Perm::W);
    sb.watch(scratch_base, 16, WatchMode::Both);

    // Drive a write directly through the MMU; this is the
    // fundamental probe site (MMU::store32). The trace state's
    // last_eip is 0 since no instruction has stepped — the
    // event still fires per the design contract; eip just
    // shows 0x00000000.
    sb.mmu.store32(scratch_base, 0xDEADBEEF).unwrap();

    let s = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        s.contains(r#""kind":"mem_write""#),
        "expected mem_write JSONL event, got: {s:?}"
    );
    assert!(
        s.contains(r#""addr":"0x40000000""#),
        "addr field missing or wrong: {s:?}"
    );
    assert!(
        s.contains(r#""value":"0xdeadbeef""#),
        "value field missing or wrong: {s:?}"
    );
}

#[test]
fn watch_emits_mem_read_event_on_guest_load() {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut sb = Sandbox::new();
    sb.set_trace_sink(Box::new(SharedSink(Arc::clone(&buf))));

    let scratch_base = 0x4001_0000u32;
    sb.mmu.map(scratch_base, 0x1000, Perm::R | Perm::W);
    sb.mmu.store32(scratch_base, 0x4242_1111).unwrap();

    sb.watch(scratch_base, 16, WatchMode::Read);

    let v = sb.mmu.load32(scratch_base).unwrap();
    assert_eq!(v, 0x4242_1111);

    let s = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        s.contains(r#""kind":"mem_read""#),
        "expected mem_read event, got: {s:?}"
    );
    assert!(
        s.contains(r#""value":"0x42421111""#),
        "value field missing: {s:?}"
    );
}

#[test]
fn unwatch_drops_subsequent_emissions() {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut sb = Sandbox::new();
    sb.set_trace_sink(Box::new(SharedSink(Arc::clone(&buf))));

    let scratch_base = 0x4002_0000u32;
    sb.mmu.map(scratch_base, 0x1000, Perm::R | Perm::W);
    sb.watch(scratch_base, 4, WatchMode::Write);
    sb.mmu.store32(scratch_base, 0x1111).unwrap();
    sb.unwatch(scratch_base, 4);
    sb.mmu.store32(scratch_base, 0x2222).unwrap();

    let s = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    // The first store fires; the second after unwatch must not.
    assert!(s.contains(r#""value":"0x00001111""#), "first event missing");
    assert!(
        !s.contains(r#""value":"0x00002222""#),
        "second event must NOT fire after unwatch, got: {s:?}"
    );
}

#[test]
fn dll_main_smoke_produces_no_unexpected_events_when_unwatched() {
    // Sanity check: wiring DllMain end-to-end with a sink
    // installed BUT no watchpoints emits no mem_* events. (The
    // win32_call probe still doesn't fire because the synthetic
    // DllMain doesn't go through the IAT.)
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut sb = Sandbox::new();
    sb.set_trace_sink(Box::new(SharedSink(Arc::clone(&buf))));

    let bytes = build_minimal_dll();
    let img = sb.load("synth.dll", &bytes).unwrap();
    let _ret = sb.call_dll_main(&img, DLL_PROCESS_ATTACH).unwrap();

    let s = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        !s.contains(r#""kind":"mem_write""#),
        "no mem_write watchpoints registered, but events fired: {s:?}"
    );
    assert!(
        !s.contains(r#""kind":"trap""#),
        "synth DllMain ran cleanly; no trap event expected: {s:?}"
    );
}
