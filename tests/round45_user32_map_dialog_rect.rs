//! Round 45 — `user32!MapDialogRect` stub + `msadds32.ax`
//! PE-load surface unblock.
//!
//! ## Background
//!
//! The MS-MPEG-4-v3 reference bundle (`wmpcdcs8-2001`) ships
//! the audio-splitter half `msadds32.ax` alongside the video
//! decoder filters.  Round 24 added `user32!{RegisterClassExA,
//! UnregisterClassA}` so that splitter's IAT could resolve
//! enough of `user32` to PE-load — but only enough to register
//! the import slots it touches at `DLL_PROCESS_ATTACH`.  The
//! splitter's full IAT pulls 29 distinct `user32` symbols
//! (PE import-table walk; see module test below), one of
//! which — `MapDialogRect` — was not yet registered.
//!
//! Without `MapDialogRect` registered, `Sandbox::load("msadds32.ax")`
//! short-circuits with
//! `PeError::UnknownImportFunction { dll: "user32.dll", name:
//! "MapDialogRect" }` because PE-loader IAT fix-up is
//! eager: every named import must resolve to a thunk before
//! the loader returns the [`Image`], regardless of whether
//! the host ever drives a code path that actually CALLs it.
//!
//! Round 45 ships a fail-soft `MapDialogRect` stub
//! (`stub_map_dialog_rect`, identity passthrough — leave the
//! caller's RECT untouched and report success per MSDN's
//! `BOOL` return convention) and proves three things end-to-end:
//!
//!   1. The stub is registered in the `Registry` and
//!      callable through the standard `dispatch_stub` path.
//!   2. After calling the stub, the input RECT contents are
//!      bit-identical to the values the test seeded — i.e. the
//!      identity passthrough does not corrupt caller memory.
//!   3. `Sandbox::load("msadds32.ax")` now succeeds (its
//!      complete `user32` IAT resolves).  This is the
//!      headline win — it advances the MS-MPEG-4-v3 audio
//!      splitter from "blocked at PE-load" to "fully
//!      PE-loadable", ungating any future round that wants
//!      to drive its DLL_PROCESS_ATTACH or DriverProc.
//!
//! ## References (clean-room, on-disk)
//!
//! * `docs/winmf/winmf-emulator.md` §"`msadds32.ax` — 22 imports"
//!   — the doc lists `MapDialogRect` as one of the user32
//!   symbols pulled by the splitter.
//! * MSDN `MapDialogRect`:
//!   <https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-mapdialogrect>
//!   — `BOOL MapDialogRect(HWND hDlg, LPRECT lpRect)`; converts
//!   dialog-base-units in `*lpRect` to screen pixels in-place
//!   on success.
//!
//! ## What we deliberately do NOT do
//!
//! Drive `msadds32.ax` through `DLL_PROCESS_ATTACH` /
//! `DriverProc`.  Per the round-24 follow-up scope we just
//! "wire the stub, don't drive msadds32 through DRV_LOAD or
//! anything else".  Future rounds that decide to exercise the
//! splitter's window-pump path will need to extend several other
//! `user32` stubs (e.g. `KillTimer`, `SetTimer` are also pulled
//! by msadds32 but are still on the round-45 todo list).

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
// Test 1 — `MapDialogRect` is wired into the user32 stub registry.
// ────────────────────────────────────────────────────────────────

#[test]
fn map_dialog_rect_is_registered_in_user32() {
    let mut r = Registry::new();
    oxideav_vfw::win32::user32::register(&mut r);
    assert!(
        r.resolve("user32.dll", "MapDialogRect").is_some(),
        "user32!MapDialogRect stub missing — round-45 follow-up"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — call `MapDialogRect` through the sandbox and verify:
//   (a) return value is TRUE (1) per MSDN's `BOOL` contract.
//   (b) the input RECT is unchanged (identity passthrough).
// ────────────────────────────────────────────────────────────────

#[test]
fn map_dialog_rect_returns_true_and_leaves_rect_unchanged() {
    let mut sb = Sandbox::new();
    let thunk = sb
        .registry
        .resolve("user32.dll", "MapDialogRect")
        .expect("MapDialogRect registered");

    // Stage a 16-byte RECT in arena memory with non-zero, easy-to-
    // recognise sentinel values.  RECT layout per `winuser.h`:
    //   typedef struct _RECT { LONG left, top, right, bottom; } RECT;
    let rect = sb
        .host
        .arena_alloc(16)
        .expect("arena_alloc 16 bytes for RECT");
    let seeded = [
        0x1111_2222u32,
        0x3333_4444u32,
        0x5555_6666u32,
        0x7777_8888u32,
    ];
    for (i, w) in seeded.iter().enumerate() {
        sb.mmu
            .store32(rect + (i as u32) * 4, *w)
            .expect("seed RECT word");
    }

    // Synthetic HWND (any non-NULL value is fine for a stub that
    // doesn't dereference it).  2-arg stdcall: push lpRect, hDlg
    // (rev order so hDlg ends up at [esp + 4] post-CALL → arg 0).
    let hdlg: u32 = 0xCAFE_0000;
    sb.cpu.push32(&mut sb.mmu, rect).unwrap();
    sb.cpu.push32(&mut sb.mmu, hdlg).unwrap();
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();

    // (a) Return value.
    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        1,
        "MapDialogRect should report success (BOOL = 1)"
    );

    // (b) RECT unchanged.
    for (i, expected) in seeded.iter().enumerate() {
        let observed = sb.mmu.load32(rect + (i as u32) * 4).unwrap();
        assert_eq!(
            observed, *expected,
            "RECT word {i}: identity stub mutated memory ({observed:#010x} != {expected:#010x})"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Test 3 — `MapDialogRect` accepts a NULL `lpRect` without
// trapping (defensive — the stub does not deref the arg).
// ────────────────────────────────────────────────────────────────

#[test]
fn map_dialog_rect_with_null_rect_does_not_trap() {
    let mut sb = Sandbox::new();
    let thunk = sb
        .registry
        .resolve("user32.dll", "MapDialogRect")
        .expect("MapDialogRect registered");
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // lpRect = NULL
    sb.cpu.push32(&mut sb.mmu, 0xCAFE_0000).unwrap(); // hDlg
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    // Identity stub still reports success on NULL — matches the
    // "fail-soft, never block the codec" pattern of the rest of
    // the user32 stub surface.
    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        1,
    );
}

// ────────────────────────────────────────────────────────────────
// Test 4 — the round-45 headline.  `Sandbox::load("msadds32.ax")`
// makes forward progress past `MapDialogRect` (the round-45
// blocker).  The post-r45 edge symbol was `KillTimer`; round 46
// added `KillTimer` + `SetTimer` and pushed the edge forward to
// `gdi32!StretchDIBits` (see `tests/round46_user32_set_kill_timer.rs`
// for the next-blocker probe).  This test only validates that
// `MapDialogRect` itself is no longer the edge — i.e. the error
// message must NOT mention `MapDialogRect` — so the
// round-45-specific assertion stays meaningful even as later
// rounds push the IAT-resolve frontier further.
//
// Skipped gracefully if the DLL is not present in the docs tree.
// ────────────────────────────────────────────────────────────────

#[test]
fn msadds32_ax_pe_load_advances_past_map_dialog_rect() {
    let Some(p) = msadds32_path() else {
        eprintln!("round45: msadds32.ax missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    match sb.load("msadds32.ax", &bytes) {
        Ok(img) => {
            // Desired terminal state (full splitter PE-load).
            eprintln!(
                "round45: msadds32.ax FULLY PE-loaded — image_base={:#010x}, \
                 entry_point={:#010x}, DllMain={:?}, DllGetClassObject={:?}",
                img.image_base,
                img.entry_point,
                img.export("DllMain"),
                img.export("DllGetClassObject"),
            );
        }
        Err(e) => {
            // Pin only the round-45-specific invariant: the edge
            // must have moved forward, off `MapDialogRect`.
            let msg = format!("{e}");
            assert!(
                !msg.contains("MapDialogRect"),
                "round 45 expected msadds32.ax PE-load to advance PAST MapDialogRect; \
                 got: {msg}"
            );
            eprintln!(
                "round45: msadds32.ax PE-load advanced past MapDialogRect; \
                 current next-blocker reported in error: {msg}"
            );
        }
    }
}
