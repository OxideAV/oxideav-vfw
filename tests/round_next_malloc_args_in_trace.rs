//! Auditor P1 — surface cdecl size / pointer args on
//! `kind=win32_call` JSONL events for the msvcrt allocation
//! surface (`malloc`, `free`, `??2@YAPAXI@Z`, `??3@YAXPAX@Z`).
//!
//! Reference: `docs/video/msmpeg4/audit/06-sandbox-O3-quant-init.md`
//! §5.2.3 — before this change, every msvcrt-allocation
//! `win32_call` event arrived with `args:[]` even though the
//! cdecl call frame held the size at `[esp+4]`. The Auditor had
//! to differentiate against the call-site EIP of every allocation
//! to match a 2928-byte codec-context allocation back to its
//! source line; with this patch the size is in the event itself.
//!
//! Drives the real msvcrt stub registry (`Sandbox::new()`'s
//! `Registry::register_all`), then invokes `malloc` /
//! `operator new` / `operator delete` / `free` through the same
//! `dispatch_stub` path the loaded codec would take, and asserts
//! the captured JSONL line carries `args:[<size>]` (decimal,
//! matching the existing event format).
//!
//! Gated on `#[cfg(feature = "trace")]`; the file compiles to
//! nothing in compatibility-only builds.

#![cfg(feature = "trace")]

use std::sync::{Arc, Mutex};

use oxideav_vfw::emulator::regs::Reg32;
use oxideav_vfw::Sandbox;

/// `Box<dyn Write + Send>` funnel into a shared buffer the test
/// can read back after the dispatch returns.
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

/// Push the cdecl call frame for a 1-arg msvcrt entry, position
/// `eip` at the named thunk, and run `dispatch_stub` once.
/// Returns the captured JSONL bytes.
fn drive_one_arg_call(name: &str, arg0: u32, ret_eip: u32) -> String {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut sb = Sandbox::new();
    sb.set_trace_sink(Box::new(SharedSink(Arc::clone(&buf))));

    let thunk = sb
        .registry
        .resolve("msvcrt.dll", name)
        .unwrap_or_else(|| panic!("msvcrt!{name} should be registered by Sandbox::new"));

    // Lay out the cdecl call frame: [esp]=ret_eip, [esp+4]=arg0.
    sb.cpu.push32(&mut sb.mmu, arg0).unwrap();
    sb.cpu.push32(&mut sb.mmu, ret_eip).unwrap();
    sb.cpu.regs.eip = thunk;

    oxideav_vfw::win32::dispatch_stub(&mut sb.cpu, &mut sb.mmu, &sb.registry, &mut sb.host)
        .expect("dispatch_stub");

    // After dispatch the return address has been popped; eax
    // holds the stub's return value (we don't check it here —
    // the JSONL line carries it as `ret`).
    let _eax = sb.cpu.regs.get32(Reg32::Eax);

    let bytes = buf.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn malloc_emits_size_arg_in_trace_event() {
    // 2928 == 0xb70 — matches the auditor reference value for
    // the codec-context allocation on the MSMPEG4 v3 path.
    let s = drive_one_arg_call("malloc", 2928, 0x1c21_8058);
    assert!(s.contains(r#""kind":"win32_call""#), "line: {s}");
    assert!(s.contains(r#""dll":"msvcrt.dll""#), "line: {s}");
    assert!(s.contains(r#""name":"malloc""#), "line: {s}");
    assert!(
        s.contains(r#""args":[2928]"#),
        "expected args:[2928] (== 0xb70), got: {s}",
    );
    assert!(s.contains(r#""eip":"0x1c218058""#), "line: {s}");
}

#[test]
fn operator_new_emits_size_arg_in_trace_event() {
    // 704 == 0x2c0 — the matching value the Auditor saw on
    // sandbox-01 for the `??2@YAPAXI@Z` site at 0x1c237e58.
    let s = drive_one_arg_call("??2@YAPAXI@Z", 704, 0x1c23_7e58);
    assert!(s.contains(r#""name":"??2@YAPAXI@Z""#), "line: {s}");
    assert!(
        s.contains(r#""args":[704]"#),
        "expected args:[704] (== 0x2c0), got: {s}",
    );
    assert!(s.contains(r#""eip":"0x1c237e58""#), "line: {s}");
}

#[test]
fn operator_delete_emits_pointer_arg_in_trace_event() {
    // The pointer the codec is freeing — Auditor needs to match
    // it against the prior `operator new` `ret` to verify the
    // delete pairs with the right allocation.
    let ptr = 0x6000_02c0u32; // == 1610613440 decimal
    let s = drive_one_arg_call("??3@YAXPAX@Z", ptr, 0x1c23_8000);
    assert!(s.contains(r#""name":"??3@YAXPAX@Z""#), "line: {s}");
    assert!(
        s.contains(r#""args":[1610613440]"#),
        "expected args:[1610613440] (== 0x600002c0), got: {s}",
    );
}

#[test]
fn free_emits_pointer_arg_in_trace_event() {
    let ptr = 0x6000_0000u32; // == 1610612736 decimal
    let s = drive_one_arg_call("free", ptr, 0x1c23_9000);
    assert!(s.contains(r#""name":"free""#), "line: {s}");
    assert!(
        s.contains(r#""args":[1610612736]"#),
        "expected args:[1610612736] (== 0x60000000), got: {s}",
    );
}

#[test]
fn unrelated_cdecl_stub_still_emits_empty_args() {
    // Sanity check: cdecl stubs without a [`cdecl_trace_arg_count`]
    // override (e.g. `_initterm` — registered with `arg_dwords =
    // 0` for caller-cleanup) still emit `args:[]`. This keeps
    // the override surface narrowly scoped to the cases the
    // auditor needs.
    let s = drive_one_arg_call("_initterm", 0, 0x1c23_a000);
    assert!(s.contains(r#""name":"_initterm""#), "line: {s}");
    assert!(
        s.contains(r#""args":[]"#),
        "expected args:[] for unmapped cdecl entry, got: {s}",
    );
}
