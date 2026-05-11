//! Round 48 — `msvcrt!_endthreadex` stub + `msadds32.ax` PE-load
//! surface advance.
//!
//! ## Background
//!
//! Round 47 added `gdi32!StretchDIBits` and pushed
//! `Sandbox::load("msadds32.ax")` past the splitter's render-out
//! edge.  The next unresolved import the splitter pulls is
//! `msvcrt!_endthreadex` — the CRT thread-teardown terminator.
//! Round 48 wires it as a fail-soft stub so the splitter's
//! PE-load advances cleanly past the entire thread-lifecycle
//! surface.
//!
//! ## Stub semantics
//!
//! `void __cdecl _endthreadex(unsigned retval)` — cdecl
//! (caller-cleanup), 1 dword on the stack.  MSDN documents it as
//! `__declspec(noreturn)`; in the real CRT control never returns
//! to the caller after `_endthreadex` runs.
//!
//! Returns 0.  The codec sandbox NEVER actually spawns the
//! splitter's worker thread on the decode path we drive (we only
//! exercise `DLL_PROCESS_ATTACH` / `DriverProc` /
//! `IPin::ReceiveConnection`); the IAT slot just needs to
//! resolve at PE-load time.  If the codec ever did call the stub
//! we'd want to fall back to the caller's return-address rather
//! than terminate the host process — which is exactly what a
//! cdecl `Ok(0)` stub does (the dispatcher pops nothing for
//! cdecl, the codec's RET picks up the saved return-address
//! from the stack).
//!
//! ## References (clean-room, on-disk)
//!
//! * `docs/winmf/winmf-emulator.md` — splitter import-walk
//!   inventory; `msvcrt!_endthreadex` is the post-r47 edge symbol.
//! * MSDN `_endthread, _endthreadex`:
//!   <https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/endthread-endthreadex>
//!
//! ## What we deliberately do NOT do
//!
//! Drive `msadds32.ax` through `DLL_PROCESS_ATTACH` /
//! `DriverProc`.  Per the round-24 / round-45 / round-46 / r47
//! follow-up scope we just "wire the stub, don't drive
//! msadds32".  Future rounds that decide to exercise the
//! splitter's window-pump path will need to extend more stubs as
//! the next blocker surfaces.

mod common;

use oxideav_vfw::emulator::isa_int::RET_SENTINEL;
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

// ────────────────────────────────────────────────────────────────
// Test 1 — the stub is wired into the msvcrt registry.
// ────────────────────────────────────────────────────────────────

#[test]
fn end_thread_ex_is_registered_in_msvcrt() {
    let mut r = Registry::new();
    oxideav_vfw::win32::msvcrt::register(&mut r);
    assert!(
        r.resolve("msvcrt.dll", "_endthreadex").is_some(),
        "msvcrt!_endthreadex stub missing — round-48 follow-up"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — `_endthreadex(retval)` returns 0 in eax (the
// noreturn-on-MSDN contract means the codec never inspects the
// return value, but a fail-soft stub should still leave `eax` in
// a deterministic state — zero is the natural choice).  Probed
// end-to-end through the dispatcher with 1 dword on the stack.
// ────────────────────────────────────────────────────────────────

#[test]
fn end_thread_ex_returns_zero_through_sandbox() {
    let mut sb = Sandbox::new();
    let thunk = sb
        .registry
        .resolve("msvcrt.dll", "_endthreadex")
        .expect("_endthreadex registered");

    // cdecl: 1-arg.  Push `retval` so it sits at [esp+4] post-CALL,
    // then push the synthetic return-address sentinel.
    sb.cpu.push32(&mut sb.mmu, 0x0000_002A).unwrap(); // retval
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();

    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        0,
        "_endthreadex should return 0 from the fail-soft stub"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 3 — degenerate `retval == 0` echoes 0 too (the stub is
// supposed to be insensitive to the caller's `retval` — it never
// surfaces it back).
// ────────────────────────────────────────────────────────────────

#[test]
fn end_thread_ex_zero_retval_returns_zero() {
    let mut sb = Sandbox::new();
    let thunk = sb
        .registry
        .resolve("msvcrt.dll", "_endthreadex")
        .expect("_endthreadex registered");
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // retval = 0
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        0,
    );
}

// ────────────────────────────────────────────────────────────────
// Test 4 — the round-48 headline: `Sandbox::load("msadds32.ax")`
// advances past `_endthreadex`.  Either the load completes (all
// imports resolved by r48) or it stops at the next unresolved
// import; we report both outcomes informationally and pin the
// failure case so any silent forward progress in a sibling round
// shows up here.
//
// Skipped gracefully if the DLL is not present in the docs tree.
// ────────────────────────────────────────────────────────────────

#[test]
fn msadds32_ax_pe_load_advances_past_end_thread_ex() {
    let Some(p) = msadds32_path() else {
        eprintln!("round48: msadds32.ax missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    match sb.load("msadds32.ax", &bytes) {
        Ok(img) => {
            // The desired terminal state: full splitter PE-load.
            eprintln!(
                "round48: msadds32.ax FULLY PE-loaded — image_base={:#010x}, \
                 entry_point={:#010x}, DllMain={:?}, DllGetClassObject={:?}",
                img.image_base,
                img.entry_point,
                img.export("DllMain"),
                img.export("DllGetClassObject"),
            );
        }
        Err(e) => {
            // Pin: must not be _endthreadex any more.
            let msg = format!("{e}");
            assert!(
                !msg.contains("\"_endthreadex\"") && !msg.contains("!_endthreadex"),
                "round 48 expected msadds32.ax PE-load to advance PAST _endthreadex; \
                 got: {msg}"
            );
            eprintln!(
                "round48: msadds32.ax PE-load advanced past _endthreadex; \
                 next blocker (if any) is reported in the error: {msg}"
            );
        }
    }
}
