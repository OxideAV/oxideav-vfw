//! Round 40 — register-snapshot + memory-probe watchpoints
//! capture `ebx` and surrounding stack state across the entire
//! Transform call (entry `0x6473` → epilogue `0x65c4`) and the
//! enclosing `0x25a2` function's BB that follows.
//!
//! r39 left the trap at MPG4DS32 RVA `0x7184` unchanged; the
//! handoff hypothesis was that `ebx` SHOULD have been the
//! pInSample (`0x600007a0`, with vtable slot 13 wired to our host
//! thunk `0xfffe03a0`) but was being clobbered to filter_base
//! (`0x60000110`) by either:
//!
//!   (a) Transform's epilogue at `0x4065c4` `pop ebx` restoring
//!       the wrong saved value, or
//!   (b) a hidden write into the `[ebp+8]` argument slot via the
//!       `IMediaSample2::SetProperties` write-back that r39
//!       wired up.
//!
//! Round 40's snapshots answer:
//!
//!   * At Transform's entry (`0x6479` push ebx), the snapshot
//!     shows `ebx == 0x600007a0` (pInSample) and the value
//!     pushed (visible at the next snapshot's `[esp]`) IS
//!     `0x600007a0`.  Caller passed pInSample correctly.
//!   * Throughout Transform's body (snapshots at `0x64f3`,
//!     `0x6545`, `0x655e`, `0x65c0`, `0x65c4`),
//!     `[ebp-0x50]@0x900ffe60 == 0x600007a0` — the
//!     saved-ebx slot is INTACT.
//!   * Throughout Transform's body, `[ebp+8]@0x900ffeb8 ==
//!     0x600007a0` — the arg slot is INTACT.  Hypothesis (b)
//!     RULED OUT.
//!   * At `0x65c4` (the `pop ebx`), `esp == 0x900ffe5c` (FOUR
//!     BYTES LOWER than `ebp-0x50 == 0x900ffe60`).  `[esp]
//!     == 0x60000110` — what pop ebx WILL read.  `[esp+4] ==
//!     0x600007a0` — what pop ebx SHOULD have read.
//!
//! Root cause: stack imbalance inside Transform.  Some call
//! site between `0x6473` and `0x65c0` decremented esp by 4
//! more than it should, so the matched `pop ebx` reads from
//! the wrong slot.  Hypothesis (a) is CONFIRMED, but the
//! mechanism is NOT a faulty `pop ebx` opcode — it's that
//! some intermediate `push` was never paired with a `pop`
//! (or some `__stdcall` call's callee-cleanup `ret N` was
//! short by 4 bytes).
//!
//! What r41 must do: bisect inside Transform by arming
//! watchpoints across each `call dword ptr [...]` site
//! (`0x4064d4`, `0x4064f3`, `0x406505`, `0x406545`, `0x40655b`,
//! `0x40656e`, `0x40657f`, `0x406590`, `0x4065a8`, `0x4065bd`)
//! and tracking esp delta before/after.  The first site whose
//! delta != args_pushed is the culprit.  A likely target is the
//! `0x4064d4: call [ecx+0x1c]` — slot 7 of `[ecx]`'s vtable —
//! or `0x4064f3: call [eax]` (QueryInterface, 3 args + this).
//! Once the offending call is found, audit the host stub to
//! ensure it consumes the right number of args + returns the
//! correct callee-cleanup count.

#![cfg(feature = "auto-discovery")]

mod common;

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, Packet, TimeBase};
use oxideav_vfw::discovery::{make_decoder, register_factory_for_id, DiscoveryRecord, Kind};

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn dshow_dll_path() -> Option<PathBuf> {
    let p =
        workspace_root()?.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/MPG4DS32.AX");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn mp43_fixture_path(stem: &str) -> Option<PathBuf> {
    let p = workspace_root()?.join(format!("docs/video/msmpeg4-fixtures/{stem}/input.avi"));
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn extract_mp43_keyframe(stem: &str) -> Option<(u32, u32, Vec<u8>)> {
    let path = mp43_fixture_path(stem)?;
    let bytes = std::fs::read(&path).ok()?;
    let s = common::avi_extractor::extract_video_sample(&bytes, 0).ok()?;
    Some((s.width, s.height, s.bytes))
}

fn try_drive_one_keyframe() -> Option<String> {
    let dll_path = dshow_dll_path()?;
    let (width, height, keyframe) = extract_mp43_keyframe("fourcc-MP43")?;
    let id = format!(
        "vfw_round40_ebx_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    register_factory_for_id(
        &id,
        DiscoveryRecord {
            dll_path,
            fourcc: "MP43".to_string(),
            kind: Kind::DirectShow,
            clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".to_string()),
        },
    );
    let mut params = CodecParameters::video(CodecId::new(id.clone()));
    params.width = Some(width);
    params.height = Some(height);
    let mut decoder = make_decoder(&params).ok()?;
    let packet = Packet::new(0, TimeBase::new(1, 25), keyframe).with_keyframe(true);
    let _ = decoder.send_packet(&packet);
    let outcome = decoder.receive_frame();
    Some(match outcome {
        Ok(other) => format!("ok: {other:?}"),
        Err(e) => format!("{e}"),
    })
}

// ────────────────────────────────────────────────────────────────
// Test 1 — at the post-Transform return IP `0x002626`, ebx holds
// `filter_base` (`0x60000110`), NOT pInSample.
// ────────────────────────────────────────────────────────────────

#[test]
fn r40_ebx_at_post_transform_is_filter_base() {
    let msg = match try_drive_one_keyframe() {
        Some(m) => m,
        None => {
            eprintln!("round40 post-transform-ebx: fixtures missing; skipping");
            return;
        }
    };
    eprintln!("round40 post-transform-ebx: {msg}");
    // Trap reproducible.
    assert!(
        msg.contains("rva=0x00007184"),
        "r40 expected the r39 trap to still fire: {msg}"
    );
    // The diagnostic carries a `r40_snaps=[...]` slot.
    assert!(
        msg.contains("r40_snaps="),
        "r40 expected the snapshot block to be present: {msg}"
    );
    // The post-Transform return IP `0x2626` snapshot must show
    // `ebx == 0x60000110` (filter_base), confirming that the
    // slot-13 call dispatches off the FILTER's primary vtable,
    // not pInSample's.
    assert!(
        msg.contains("rva=0x2626 ") && msg.contains("ebx=0x60000110"),
        "r40 expected ebx=0x60000110 at rva=0x2626: {msg}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — at the slot-13 dispatch site `0x00263b`, ebx is still
// filter_base and eax holds the filter's primary vtable
// (`0x1c4269f4`), proving the call IS [filter_vtbl + 0x34].
// ────────────────────────────────────────────────────────────────

#[test]
fn r40_slot13_call_dispatches_off_filter_vtable() {
    let msg = match try_drive_one_keyframe() {
        Some(m) => m,
        None => {
            eprintln!("round40 slot13-dispatch: fixtures missing; skipping");
            return;
        }
    };
    assert!(
        msg.contains("rva=0x263b ")
            && msg.contains("eax=0x1c4269f4")
            && msg.contains("ebx=0x60000110"),
        "r40 expected slot-13 dispatch off filter primary vtable \
         (eax=0x1c4269f4 ebx=0x60000110): {msg}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 3 — `[ebp+8]` (the function's first arg = pInSample) is
// NEVER overwritten across the snapshot fan.
//
// IMPORTANT: the snapshot block reports `[ebp+8]@addr=value` so
// each report includes the exact addr.  Distinct frames have
// distinct ebps, so the same `[ebp+8]` syntactic slot resolves
// to different addresses; we assert the addr we care about
// (Transform's frame at `0x900ffeb8`) carries pInSample, and the
// outer caller's slot at `0x900ffee0` likewise.  This rules out
// hypothesis (b) (a stale SetProperties write clobbering an
// arg slot).
// ────────────────────────────────────────────────────────────────

#[test]
fn r40_arg1_pinsample_intact_across_snapshots() {
    let msg = match try_drive_one_keyframe() {
        Some(m) => m,
        None => {
            eprintln!("round40 arg1-intact: fixtures missing; skipping");
            return;
        }
    };
    // The diagnostic carries an `r40_arg1` block.
    assert!(
        msg.contains("r40_arg1="),
        "r40 expected arg1 block to be present: {msg}"
    );
    // Outer caller frame: `[ebp+8]@0x900ffee0=0x600007a0` (the
    // OUTER function's pInSample arg).
    assert!(
        msg.contains("[ebp+8]@0x900ffee0=0x600007a0"),
        "r40 expected outer caller's arg1 to remain pInSample: {msg}"
    );
    // Transform's frame: `[ebp+8]@0x900ffeb8=0x600007a0`.  This
    // is the killer: throughout Transform's lifetime the arg
    // slot is intact.  hypothesis (b) is RULED OUT.
    assert!(
        msg.contains("[ebp+8]@0x900ffeb8=0x600007a0"),
        "r40 expected Transform's arg1 slot to remain pInSample \
         (rules out hypothesis (b)): {msg}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 5 — root cause: the `pop ebx` in Transform's epilogue at
// RVA `0x4065c4` reads from `[esp]=0x60000110` even though the
// CORRECT saved-ebx slot at `[ebp-0x50]=0x900ffe60` holds
// `0x600007a0` (pInSample).  This is a stack-imbalance: between
// Transform's `push ebx` at `0x406479` and the matching `pop ebx`
// at `0x4065c4`, esp ends up 4 bytes lower than it should.
//
// Snapshot evidence at `0x4065c4`:
//   * `[ebp-0x50]@0x900ffe60=0x600007a0` (saved-ebx slot intact)
//   * `[esp]=0x60000110` (what pop ebx WILL read)
//   * `[esp+4]=0x600007a0` (what pop ebx SHOULD have read; 4
//                           bytes higher = the right slot)
//   * `esp=0x900ffe5c` (4 bytes lower than `ebp-0x50`)
//
// Diagnosis: some op inside Transform's body added a dword to
// the stack without a matching cleanup.  Candidates: a
// `__stdcall` virtual call where our emulator did NOT perform
// callee `ret N` cleanup, OR a `push` whose pair `pop` lives on
// a control-flow path we never took.  R41 should bisect by
// arming watchpoints across each `call dword ptr [...]` site
// inside Transform (`0x4064d4`, `0x4064f3`, `0x406505`, `0x406545`,
// `0x40655b`, `0x40656e`, `0x40657f`, `0x406590`, `0x4065a8`,
// `0x4065bd`) and tracking esp before/after each.
// ────────────────────────────────────────────────────────────────

#[test]
fn r40_stack_imbalance_at_pop_ebx_confirmed() {
    let msg = match try_drive_one_keyframe() {
        Some(m) => m,
        None => {
            eprintln!("round40 stack-imbalance: fixtures missing; skipping");
            return;
        }
    };
    // pop ebx site: snapshot at `0x65c4`.  We need:
    //   * `[esp]=0x60000110`  — what pop ebx WILL read
    //   * `[esp+4]=0x600007a0` — the slot 4 bytes higher
    //                            holds the correct value
    //   * `[ebp-0x50]@0x900ffe60=0x600007a0` — confirms the
    //                            saved-ebx slot itself was
    //                            never overwritten; the bug is
    //                            in WHERE pop ebx reads, not in
    //                            the slot's contents.
    assert!(
        msg.contains(
            "rva=0x65c4 [ebp+8]@0x900ffeb8=0x600007a0 \
                      [esp]=0x60000110 [esp+4]=0x600007a0 \
                      [ebp-0x50]@0x900ffe60=0x600007a0"
        ),
        "r40 expected stack-imbalance signature at pop ebx \
         (saved slot intact, esp 4 bytes too low): {msg}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 4 — function-entry `0x002a52` and end-of-prolog candidates
// `0x002a58` show `ebx = 0` (the value persists from the
// caller's saved-ebx push, and the prolog never copies `[ebp+8]`
// into `ebx`).  This is the proof that the disasm
// interpretation `mov ebx, [ebp+8]` (assumed by r39's handoff)
// is WRONG.
// ────────────────────────────────────────────────────────────────

#[test]
fn r40_function_entry_does_not_bind_ebx_to_arg1() {
    let msg = match try_drive_one_keyframe() {
        Some(m) => m,
        None => {
            eprintln!("round40 ebx-prolog: fixtures missing; skipping");
            return;
        }
    };
    // Entry: ebx is whatever the caller passed (unspecified in
    // SysV/cdecl callees).  Real codec built with MSVC
    // `__thiscall` puts `this` in ECX, then immediately moves it
    // into the saved-ebx slot.  Our snapshot at 0x25a2 fires
    // BEFORE the prolog even runs, so ebx == caller's choice
    // (here: 0).
    assert!(
        msg.contains("rva=0x25a2 ") && msg.contains("ebx=0x00000000"),
        "r40 expected function-entry snapshot at 0x25a2 \
         with caller-supplied ebx: {msg}"
    );
    // 0x25a8 is mid-prolog by our static-analysis guess.  The
    // r40 data shows ebx is STILL 0 here, which means whatever
    // the prolog does, it does not bind ebx to the first arg.
    // Confirms the original assumption "ebx is pInSample" was
    // false.
    assert!(
        msg.contains("rva=0x25a8 ") && msg.contains("ebx=0x00000000"),
        "r40 expected mid-prolog snapshot at 0x25a8 to show \
         ebx still 0 (no `mov ebx, [ebp+8]` in prolog): {msg}"
    );
}
