//! Round 47 — `gdi32!StretchDIBits` stub + `msadds32.ax` PE-load
//! surface advance.
//!
//! ## Background
//!
//! Round 46 added `user32!{SetTimer, KillTimer}` and pushed
//! `Sandbox::load("msadds32.ax")` past the entire user32 timer-API
//! surface.  The next unresolved import the splitter pulls is
//! `gdi32!StretchDIBits` — the splitter's headless render-out
//! surface.  Round 47 wires it as a fail-soft stub so the
//! splitter's PE-load advances cleanly past the render-out edge.
//!
//! ## Stub semantics
//!
//! `int StretchDIBits(HDC hdc, int xDest, int yDest, int DestWidth,
//! int DestHeight, int xSrc, int ySrc, int SrcWidth, int SrcHeight,
//! const VOID *lpBits, const BITMAPINFO *lpbmi, UINT iUsage,
//! DWORD rop)` — `__stdcall`, 13 dwords on the stack.
//!
//! Returns the caller's `DestHeight` — i.e. the number of
//! scanlines "copied" per MSDN's success contract.  The codec
//! sandbox never actually paints; `msadds32.ax` is the
//! MS-MPEG-4-v3 audio splitter and only pulls this import as part
//! of its statically-linked render-out surface, never invokes it
//! on the decode path we drive.  Reporting `DestHeight` satisfies
//! any "scanlines > 0 == success" probe at the call site without
//! ever surfacing `GDI_ERROR` (the explicit failure marker).
//!
//! ## References (clean-room, on-disk)
//!
//! * `docs/winmf/winmf-emulator.md` — splitter import-walk
//!   inventory; `gdi32!StretchDIBits` is the post-r46 edge symbol.
//! * MSDN `StretchDIBits`:
//!   <https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-stretchdibits>
//!
//! ## What we deliberately do NOT do
//!
//! Drive `msadds32.ax` through `DLL_PROCESS_ATTACH` /
//! `DriverProc`.  Per the round-24 / round-45 / round-46 follow-up
//! scope we just "wire the stub, don't drive msadds32".  Future
//! rounds that decide to exercise the splitter's window-pump path
//! will need to extend more stubs as the next blocker surfaces.

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
// Test 1 — the stub is wired into the gdi32 registry.
// ────────────────────────────────────────────────────────────────

#[test]
fn stretch_dibits_is_registered_in_gdi32() {
    let mut r = Registry::new();
    oxideav_vfw::win32::gdi32::register(&mut r);
    assert!(
        r.resolve("gdi32.dll", "StretchDIBits").is_some(),
        "gdi32!StretchDIBits stub missing — round-47 follow-up"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — `StretchDIBits` reports the caller's `DestHeight` as
// the scanline count (MSDN: "the return value is the number of
// scanlines copied").  Probed end-to-end through the dispatcher
// with 13 dwords on the stack.
// ────────────────────────────────────────────────────────────────

#[test]
fn stretch_dibits_returns_dest_height_through_sandbox() {
    let mut sb = Sandbox::new();
    let thunk = sb
        .registry
        .resolve("gdi32.dll", "StretchDIBits")
        .expect("StretchDIBits registered");

    // 13-arg stdcall.  Push args in reverse so they sit at
    // [esp+4] (hdc) … [esp+52] (rop) post-CALL.
    let dest_height: u32 = 288;
    sb.cpu.push32(&mut sb.mmu, 0x00CC_0020).unwrap(); // rop (SRCCOPY)
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // iUsage = DIB_RGB_COLORS
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // lpbmi = NULL
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // lpBits = NULL
    sb.cpu.push32(&mut sb.mmu, 288).unwrap(); // SrcHeight
    sb.cpu.push32(&mut sb.mmu, 352).unwrap(); // SrcWidth
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // ySrc
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // xSrc
    sb.cpu.push32(&mut sb.mmu, dest_height).unwrap(); // DestHeight
    sb.cpu.push32(&mut sb.mmu, 352).unwrap(); // DestWidth
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // yDest
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // xDest
    sb.cpu.push32(&mut sb.mmu, 0xDEAD_C011).unwrap(); // hdc (SENTINEL)
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();

    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        dest_height,
        "StretchDIBits should echo DestHeight as the scanline count"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 3 — degenerate `DestHeight == 0` echoes 0 (still a
// non-error outcome — `GDI_ERROR` is the explicit failure
// marker and we never want to surface it from a fail-soft stub).
// ────────────────────────────────────────────────────────────────

#[test]
fn stretch_dibits_zero_dest_height_echoes_zero() {
    let mut sb = Sandbox::new();
    let thunk = sb
        .registry
        .resolve("gdi32.dll", "StretchDIBits")
        .expect("StretchDIBits registered");
    sb.cpu.push32(&mut sb.mmu, 0x00CC_0020).unwrap(); // rop
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // iUsage
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // lpbmi
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // lpBits
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // SrcHeight
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // SrcWidth
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // ySrc
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // xSrc
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // DestHeight = 0
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // DestWidth
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // yDest
    sb.cpu.push32(&mut sb.mmu, 0).unwrap(); // xDest
    sb.cpu.push32(&mut sb.mmu, 0xDEAD_C011).unwrap(); // hdc
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        0,
    );
}

// ────────────────────────────────────────────────────────────────
// Test 4 — the round-47 headline: `Sandbox::load("msadds32.ax")`
// advances past `StretchDIBits`.  Either the load completes (all
// imports resolved by r47) or it stops at the next unresolved
// import; we report both outcomes informationally and pin the
// failure case so any silent forward progress in a sibling round
// shows up here.
//
// Skipped gracefully if the DLL is not present in the docs tree.
// ────────────────────────────────────────────────────────────────

#[test]
fn msadds32_ax_pe_load_advances_past_stretch_dibits() {
    let Some(p) = msadds32_path() else {
        eprintln!("round47: msadds32.ax missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    match sb.load("msadds32.ax", &bytes) {
        Ok(img) => {
            // The desired terminal state: full splitter PE-load.
            eprintln!(
                "round47: msadds32.ax FULLY PE-loaded — image_base={:#010x}, \
                 entry_point={:#010x}, DllMain={:?}, DllGetClassObject={:?}",
                img.image_base,
                img.entry_point,
                img.export("DllMain"),
                img.export("DllGetClassObject"),
            );
        }
        Err(e) => {
            // Pin: must not be StretchDIBits any more.
            let msg = format!("{e}");
            assert!(
                !msg.contains("\"StretchDIBits\""),
                "round 47 expected msadds32.ax PE-load to advance PAST StretchDIBits; \
                 got: {msg}"
            );
            eprintln!(
                "round47: msadds32.ax PE-load advanced past StretchDIBits; \
                 next blocker (if any) is reported in the error: {msg}"
            );
        }
    }
}
