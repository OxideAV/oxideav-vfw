//! Round 40 + 41 — bisect across `Transform`'s ten internal
//! `call dword ptr [...]` sites localised the stack-imbalance to
//! the FIRST one, RVA `0x4064d4 = call [ecx+0x1c]` —
//! `IMemAllocator::GetBuffer(this, IMediaSample **ppBuffer,
//! REFERENCE_TIME *pStartTime, REFERENCE_TIME *pStopTime,
//! DWORD dwFlags)`.  This is FIVE pushed dwords (this + four
//! arguments) but our host stub at
//! `crates/oxideav-vfw/src/com/host_iface.rs` was registered with
//! `arg_dwords=4`.  The stdcall callee-cleanup in
//! `win32::dispatch_stub` therefore popped only 16 bytes from the
//! guest stack, leaving esp 4 bytes too low — exactly the
//! 4-byte deficit round 40's snapshots measured at `pop ebx`
//! (RVA `0x4065c4`).
//!
//! Round 41 fixed the registration to `arg_dwords=5` and added
//! the missing `dwFlags` arg-read in the stub.  With the fix:
//!
//!   * The trap at MPG4DS32 RVA `0x7184` is GONE.
//!   * `IMemInputPin::Receive` returns S_OK and the codec emits
//!     a decoded frame through the downstream pin (24bpp BGR is
//!     surfaced via `surface_received_dshow_frame`).
//!
//! The original r40 trap-diagnostic assertions (`r40_snaps=`
//! presence, ebx==filter_base, etc.) no longer hold because we
//! never reach the trap branch.  This file's tests now assert
//! the FIXED behaviour: the MP43 keyframe round-trips through
//! the DShow path and produces a video frame.
//!
//! For the historical bisect derivation see the commit message
//! that landed round 41.

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
// Test 1 — `Transform`'s ten internal `call dword ptr [...]`
// sites have been bisected.  Each one is a `__stdcall` virtual
// dispatch; the call's pre-call ESP minus the post-call ESP
// must equal `args_pushed * 4` (callee cleanup).  Round 40's
// snapshots at `0x6479` (entry) and `0x65c4` (matched pop ebx)
// showed a 4-byte deficit; r41 walks each call site:
//
//   0x4064d4 [ecx+0x1c] ─ IMemAllocator::GetBuffer (5 args)
//   0x4064f3 [eax]      ─ IMediaSample::QueryInterface (3 args)
//   0x406505 [ecx+0x4c] ─ IMediaSample2::SetProperties (3 args)
//   0x406545 [edx+0x50] ─ IMediaSample2::SetProperties out
//                         (3 args, mirror of 0x6505)
//   0x40655b [ecx+0x8]  ─ Release (1 arg)
//   0x40656e [ecx+0x18] ─ IMediaSample::SetTime (3 args)
//   0x40657f [ecx+0x20] ─ IMediaSample::SetSyncPoint (2 args)
//   0x406590 [ecx+0x40] ─ IMediaSample::SetPreroll (2 args)
//   0x4065a8 [ecx+0x44] ─ IMediaSample::GetMediaType (2 args)
//   0x4065bd [ecx+0x48] ─ IMediaSample::SetMediaType (2 args)
//
// The first site (`0x4064d4` GetBuffer) was the culprit: our
// host stub registration in `com::host_iface::register` had
// `arg_dwords=4`; the fix bumped it to `5`.  With the fix the
// keyframe Receive returns a Video frame.
// ────────────────────────────────────────────────────────────────

#[test]
fn r41_mp43_keyframe_decodes_after_getbuffer_arg_count_fix() {
    let msg = match try_drive_one_keyframe() {
        Some(m) => m,
        None => {
            eprintln!("round41 mp43-keyframe: fixtures missing; skipping");
            return;
        }
    };
    eprintln!("round41 mp43-keyframe: {msg}");
    // The pre-r41 trap signature was the `0x7184` `repe cmpsd`
    // memory-fault inside `IsEqualGUID`; with the GetBuffer arg
    // count fixed we no longer hit it.
    assert!(
        !msg.contains("rva=0x00007184"),
        "r41 expected the 0x7184 trap to be GONE: {msg}"
    );
    // The MP43 path should now surface a Video frame.
    assert!(
        msg.starts_with("ok: Video(VideoFrame"),
        "r41 expected a Video frame from the MP43 keyframe: {msg}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — guard the registration: `IMemAllocator::GetBuffer` is
// a 5-dword stdcall (this + ppBuffer + pStartTime + pStopTime +
// dwFlags).  Counting only 4 truncates the dispatcher's
// callee-cleanup and shifts the stack 4 bytes too low.  This
// test goes through the public `RegistryHandle` to assert the
// `arg_dwords` field is now 5.
// ────────────────────────────────────────────────────────────────

#[test]
fn r41_imemallocator_getbuffer_registered_as_5_args() {
    use oxideav_vfw::win32::Registry;
    let mut registry = Registry::new();
    oxideav_vfw::com::host_iface::register(&mut registry);
    let thunk = registry
        .resolve("host-com.host", "IMemAllocator::GetBuffer")
        .expect("GetBuffer must be registered");
    let entry = registry
        .entry(thunk)
        .expect("GetBuffer thunk must round-trip via entry()");
    assert_eq!(
        entry.arg_dwords, 5,
        "IMemAllocator::GetBuffer is `(this, ppBuffer, \
         pStartTime, pStopTime, dwFlags)` = 5 dwords"
    );
}
