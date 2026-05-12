//! Round 50 — `msvcrt!_beginthreadex` stub + `msadds32.ax` PE-load
//! surface advance.
//!
//! ## Background
//!
//! Round 49 added `msvcrt!_strnicmp` as a real ASCII-tolower
//! bounded compare and pushed `Sandbox::load("msadds32.ax")` past
//! the splitter's case-insensitive bounded-compare edge.  The next
//! unresolved import the splitter pulls is `msvcrt!_beginthreadex`
//! — the CRT entry that creates an `__stdcall` worker thread.
//! Round 50 wires it as a fail-soft no-op stub returning 0 so the
//! splitter's PE-load advances cleanly past the entire CRT
//! thread-lifecycle surface (`_beginthreadex` + the r48
//! `_endthreadex`).
//!
//! ## Stub semantics
//!
//! `uintptr_t __cdecl _beginthreadex(void *security, unsigned
//! stack_size, unsigned (__stdcall *start_address)(void *), void
//! *arglist, unsigned initflag, unsigned *thrdaddr)` — cdecl
//! (caller-cleanup), 6 dwords on the stack.
//!
//! Returns 0 (NULL handle) — the MSDN "thread creation failed"
//! sentinel.  The codec sandbox NEVER actually spawns the
//! splitter's worker thread on the decode path we drive (we only
//! exercise `DLL_PROCESS_ATTACH` / `DriverProc` /
//! `IPin::ReceiveConnection`); real call sites in the splitter's
//! init layer check the return for non-zero and either fall back
//! or skip the worker-thread codepath cleanly.  Optionally
//! clears `*thrdaddr` to 0 when the pointer is non-NULL and
//! in-bounds; OOB pointers are silently swallowed (the MSDN
//! contract has no way to surface a fault back to the caller, and
//! panicking would tear down the host process).
//!
//! ## References (clean-room, on-disk)
//!
//! * `docs/winmf/winmf-emulator.md` — splitter import-walk
//!   inventory; `msvcrt!_beginthreadex` is the post-r49 edge
//!   symbol.
//! * MSDN `_beginthread, _beginthreadex`:
//!   <https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/beginthread-beginthreadex>
//!
//! ## What we deliberately do NOT do
//!
//! Drive `msadds32.ax` through `DLL_PROCESS_ATTACH` /
//! `DriverProc`.  Per the round-24 / r45 / r46 / r47 / r48 / r49
//! follow-up scope we just "wire the stub, don't drive
//! msadds32".  Future rounds that decide to exercise the
//! splitter's window-pump path will need to extend more stubs as
//! the next blocker surfaces.

use oxideav_vfw::emulator::isa_int::RET_SENTINEL;
use oxideav_vfw::emulator::regs::Reg32;
use oxideav_vfw::win32::Registry;
use oxideav_vfw::Sandbox;
use std::path::PathBuf;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn msadds32_path() -> Option<PathBuf> {
    let p =
        workspace_root()?.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/msadds32.ax");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// Push 6 cdecl args + the synthetic return-address sentinel, run
/// the stub to completion, and return the dword left in `eax`.
fn call_begin_thread_ex(
    sb: &mut Sandbox,
    security: u32,
    stack_size: u32,
    start_address: u32,
    arglist: u32,
    initflag: u32,
    thrdaddr: u32,
) -> u32 {
    let thunk = sb
        .registry
        .resolve("msvcrt.dll", "_beginthreadex")
        .expect("_beginthreadex registered");
    // cdecl: push args right-to-left so they sit at [esp+4..]
    // post-CALL.
    sb.cpu.push32(&mut sb.mmu, thrdaddr).unwrap();
    sb.cpu.push32(&mut sb.mmu, initflag).unwrap();
    sb.cpu.push32(&mut sb.mmu, arglist).unwrap();
    sb.cpu.push32(&mut sb.mmu, start_address).unwrap();
    sb.cpu.push32(&mut sb.mmu, stack_size).unwrap();
    sb.cpu.push32(&mut sb.mmu, security).unwrap();
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    sb.cpu.regs.get32(Reg32::Eax)
}

// ────────────────────────────────────────────────────────────────
// Test 1 — the stub is wired into the msvcrt registry.
// ────────────────────────────────────────────────────────────────

#[test]
fn begin_thread_ex_is_registered_in_msvcrt() {
    let mut r = Registry::new();
    oxideav_vfw::win32::msvcrt::register(&mut r);
    assert!(
        r.resolve("msvcrt.dll", "_beginthreadex").is_some(),
        "msvcrt!_beginthreadex stub missing — round-50 follow-up"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — canonical call: 6 dwords on the stack, NULL `thrdaddr`.
// Returns 0 (MSDN "thread creation failed" sentinel) without
// dereferencing the NULL out-pointer.
// ────────────────────────────────────────────────────────────────

#[test]
fn begin_thread_ex_returns_zero_for_canonical_call() {
    let mut sb = Sandbox::new();
    let eax = call_begin_thread_ex(
        &mut sb,
        /*security=*/ 0,
        /*stack_size=*/ 0,
        /*start_address=*/ 0xDEAD_BEEF, // never invoked
        /*arglist=*/ 0,
        /*initflag=*/ 0,
        /*thrdaddr=*/ 0, // NULL — no write-back
    );
    assert_eq!(
        eax, 0,
        "_beginthreadex should return 0 (NULL handle) per MSDN's failure contract"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 3 — non-NULL `thrdaddr` is cleared to 0.  Stage a dword in
// the host's const arena, point `thrdaddr` at it, confirm the stub
// writes 0 through it.
// ────────────────────────────────────────────────────────────────

#[test]
fn begin_thread_ex_clears_thrdaddr_when_non_null() {
    let mut sb = Sandbox::new();
    let tid_slot = sb.host.arena_const_alloc(4).unwrap();
    // Pre-seed the slot to a non-zero sentinel so we can prove the
    // stub actually wrote 0 (not just left it untouched).
    sb.mmu
        .write_initializer(tid_slot, &0xCAFE_BABEu32.to_le_bytes())
        .unwrap();

    let eax = call_begin_thread_ex(
        &mut sb,
        /*security=*/ 0,
        /*stack_size=*/ 0x10000,
        /*start_address=*/ 0xDEAD_BEEF,
        /*arglist=*/ 0,
        /*initflag=*/ 0,
        /*thrdaddr=*/ tid_slot,
    );
    assert_eq!(eax, 0, "_beginthreadex should return 0");
    let after = sb.mmu.load32(tid_slot).unwrap();
    assert_eq!(
        after, 0,
        "*thrdaddr should be cleared to 0; got {after:#010x}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 4 — fail-soft on OOB `thrdaddr` pointer.  Pass an unmapped
// guest address; the stub must silently swallow the trap and
// still return 0 rather than propagating an error to the
// dispatcher.
// ────────────────────────────────────────────────────────────────

#[test]
fn begin_thread_ex_fail_soft_on_oob_thrdaddr() {
    let mut sb = Sandbox::new();
    // 0x0000_0010 is unmapped (the const arena lives much higher).
    let oob = 0x0000_0010u32;
    assert!(
        !sb.mmu.is_mapped(oob),
        "test precondition: {oob:#010x} must be unmapped"
    );
    let eax = call_begin_thread_ex(
        &mut sb,
        /*security=*/ 0,
        /*stack_size=*/ 0,
        /*start_address=*/ 0xDEAD_BEEF,
        /*arglist=*/ 0,
        /*initflag=*/ 0,
        /*thrdaddr=*/ oob,
    );
    assert_eq!(
        eax, 0,
        "fail-soft envelope should return 0 even on OOB thrdaddr; got {eax:#010x}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 5 — the round-50 headline: `Sandbox::load("msadds32.ax")`
// advances past `_beginthreadex`.  Either the load completes (all
// imports resolved by r50) or it stops at the next unresolved
// import; we report both outcomes informationally and pin the
// failure case so any silent forward progress in a sibling round
// shows up here.
//
// Skipped gracefully if the DLL is not present in the docs tree.
// ────────────────────────────────────────────────────────────────

#[test]
fn msadds32_ax_pe_load_advances_past_begin_thread_ex() {
    let Some(p) = msadds32_path() else {
        eprintln!("round50: msadds32.ax missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    match sb.load("msadds32.ax", &bytes) {
        Ok(img) => {
            // Desired terminal state: full splitter PE-load.
            eprintln!(
                "round50: msadds32.ax FULLY PE-loaded — image_base={:#010x}, \
                 entry_point={:#010x}, DllMain={:?}, DllGetClassObject={:?}",
                img.image_base,
                img.entry_point,
                img.export("DllMain"),
                img.export("DllGetClassObject"),
            );
        }
        Err(e) => {
            // Pin: the unresolved-import name in the error must
            // not contain `_beginthreadex` any more.
            let msg = format!("{e}");
            assert!(
                !msg.contains("\"_beginthreadex\"") && !msg.contains("!_beginthreadex"),
                "round 50 expected msadds32.ax PE-load to advance PAST _beginthreadex; \
                 got: {msg}"
            );
            eprintln!(
                "round50: msadds32.ax PE-load advanced past _beginthreadex; \
                 next blocker (if any) is reported in the error: {msg}"
            );
        }
    }
}
