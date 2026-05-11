//! Round 46 — `user32!{SetTimer, KillTimer}` stubs + `msadds32.ax`
//! PE-load surface advance.
//!
//! ## Background
//!
//! Round 45 added `user32!MapDialogRect` and pushed
//! `Sandbox::load("msadds32.ax")` past that import; the next
//! unresolved `user32` symbol the splitter pulls is `KillTimer`,
//! immediately followed by `SetTimer`.  Round 46 wires both as
//! fail-soft stubs in one commit so the splitter's PE-load
//! advances cleanly past the entire timer-API surface.
//!
//! Stub semantics — both are `__stdcall`, both fail-soft per the
//! round-24 / round-45 user32 playbook (codec sandbox never enters
//! the message-loop branch that would let a TIMERPROC actually
//! fire):
//!
//!   * `UINT_PTR SetTimer(HWND hWnd, UINT_PTR nIDEvent, UINT
//!     uElapse, TIMERPROC lpTimerFunc)` — return the caller's
//!     `nIDEvent` if non-zero, else a synthetic `1`.  Both
//!     satisfy the documented "non-zero == success" probe.
//!   * `BOOL KillTimer(HWND hWnd, UINT_PTR uIDEvent)` —
//!     return `TRUE` (1) per MSDN's "found and destroyed"
//!     contract.
//!
//! ## References (clean-room, on-disk)
//!
//! * `docs/winmf/winmf-emulator.md` §"`msadds32.ax` — 22 imports"
//!   — lists `KillTimer` / `SetTimer` among the user32 symbols
//!   pulled by the splitter.
//! * MSDN `SetTimer`:
//!   <https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-settimer>
//! * MSDN `KillTimer`:
//!   <https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-killtimer>
//!
//! ## What we deliberately do NOT do
//!
//! Drive `msadds32.ax` through `DLL_PROCESS_ATTACH` /
//! `DriverProc`.  Per the round-24 / round-45 follow-up scope we
//! just "wire the stub, don't drive msadds32".  Future rounds
//! that decide to exercise the splitter's window-pump path will
//! need to extend more `user32` stubs as the next blocker
//! surfaces.

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
// Test 1 — both timer stubs are wired into the user32 registry.
// ────────────────────────────────────────────────────────────────

#[test]
fn set_timer_and_kill_timer_are_registered_in_user32() {
    let mut r = Registry::new();
    oxideav_vfw::win32::user32::register(&mut r);
    assert!(
        r.resolve("user32.dll", "SetTimer").is_some(),
        "user32!SetTimer stub missing — round-46 follow-up"
    );
    assert!(
        r.resolve("user32.dll", "KillTimer").is_some(),
        "user32!KillTimer stub missing — round-46 follow-up"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — `SetTimer` with a non-zero `nIDEvent` echoes that id
// back as the return value (MSDN: when `hWnd` is non-NULL and
// `nIDEvent` is non-zero, that id is the timer id).
// ────────────────────────────────────────────────────────────────

#[test]
fn set_timer_returns_caller_supplied_id_when_nonzero() {
    let mut sb = Sandbox::new();
    let thunk = sb
        .registry
        .resolve("user32.dll", "SetTimer")
        .expect("SetTimer registered");

    // 4-arg stdcall.  Push args in reverse so they sit at
    // [esp+4] (hWnd) … [esp+16] (lpTimerFunc) post-CALL.
    let nid_event: u32 = 0xDEAD_BEEF;
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // lpTimerFunc = NULL
    sb.cpu.push32(&mut sb.mmu, 1000).unwrap(); // uElapse = 1 s
    sb.cpu.push32(&mut sb.mmu, nid_event).unwrap(); // nIDEvent
    sb.cpu.push32(&mut sb.mmu, 0xCAFE_0000).unwrap(); // hWnd
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();

    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        nid_event,
        "SetTimer should echo the caller's non-zero nIDEvent back"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 3 — `SetTimer` with `nIDEvent == 0` returns a synthetic
// non-zero id (MSDN: when the caller passes 0 the system
// allocates one; we hand back `1`).
// ────────────────────────────────────────────────────────────────

#[test]
fn set_timer_returns_synthetic_id_when_nid_event_is_zero() {
    let mut sb = Sandbox::new();
    let thunk = sb
        .registry
        .resolve("user32.dll", "SetTimer")
        .expect("SetTimer registered");
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // lpTimerFunc = NULL
    sb.cpu.push32(&mut sb.mmu, 500).unwrap(); // uElapse
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // nIDEvent = 0
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // hWnd = NULL
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    let ret = sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax);
    assert_ne!(ret, 0, "SetTimer must return non-zero id on success");
    assert_eq!(ret, 1, "current synthetic id is the constant 1");
}

// ────────────────────────────────────────────────────────────────
// Test 4 — `KillTimer` returns TRUE (1) regardless of argument
// values: stub never registered any actual timer, but reports
// success per MSDN's "destroyed" contract.
// ────────────────────────────────────────────────────────────────

#[test]
fn kill_timer_returns_true() {
    let mut sb = Sandbox::new();
    let thunk = sb
        .registry
        .resolve("user32.dll", "KillTimer")
        .expect("KillTimer registered");
    sb.cpu.push32(&mut sb.mmu, 0xDEAD_BEEF).unwrap(); // uIDEvent
    sb.cpu.push32(&mut sb.mmu, 0xCAFE_0000).unwrap(); // hWnd
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        1,
        "KillTimer should report success (BOOL = 1)"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 5 — the headline: `Sandbox::load("msadds32.ax")` advances
// past `KillTimer` and `SetTimer`.  Either the load completes
// (all user32 imports resolved by r46) or it stops at the next
// unresolved import; we report both outcomes informationally and
// pin the failure case so any silent forward progress in a
// sibling round shows up here.
//
// Skipped gracefully if the DLL is not present in the docs tree.
// ────────────────────────────────────────────────────────────────

#[test]
fn msadds32_ax_pe_load_advances_past_timer_pair() {
    let Some(p) = msadds32_path() else {
        eprintln!("round46: msadds32.ax missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    match sb.load("msadds32.ax", &bytes) {
        Ok(img) => {
            // The desired terminal state: full splitter PE-load.
            eprintln!(
                "round46: msadds32.ax FULLY PE-loaded — image_base={:#010x}, \
                 entry_point={:#010x}, DllMain={:?}, DllGetClassObject={:?}",
                img.image_base,
                img.entry_point,
                img.export("DllMain"),
                img.export("DllGetClassObject"),
            );
        }
        Err(e) => {
            // Pin: must not be KillTimer or SetTimer any more.
            let msg = format!("{e}");
            assert!(
                !msg.contains("\"KillTimer\""),
                "round 46 expected msadds32.ax PE-load to advance PAST KillTimer; \
                 got: {msg}"
            );
            assert!(
                !msg.contains("\"SetTimer\""),
                "round 46 expected msadds32.ax PE-load to advance PAST SetTimer; \
                 got: {msg}"
            );
            eprintln!(
                "round46: msadds32.ax PE-load advanced past KillTimer + SetTimer; \
                 next blocker (if any) is reported in the error: {msg}"
            );
        }
    }
}
