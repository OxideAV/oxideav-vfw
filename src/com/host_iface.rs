//! Round 27 — host-side COM interface stubs.
//!
//! When a guest codec calls `IBaseFilter::JoinFilterGraph(pGraph,
//! pName)` we want the codec to believe it is hosted by a
//! real `IFilterGraph`.  But there is nothing on the host
//! side to back that pointer — the test process is not running a
//! real DirectShow filter graph manager.
//!
//! This module mints a *host-side* COM object: a vtable laid out
//! in arena memory whose function-pointer slots point at
//! synthetic thunk addresses registered with [`crate::win32::Registry`].
//! When the codec calls one of those slots, the existing
//! `dispatch_stub` machinery routes control into a Rust handler
//! that returns a sensible HRESULT (typically `E_NOTIMPL` or
//! `S_OK`).
//!
//! Refcounting is intentionally minimal — the host object lives
//! for the duration of the sandbox, so `AddRef` / `Release` just
//! increment / decrement an internal counter without ever
//! deallocating.  The codec sees the contract:
//!
//! * `QueryInterface(IID_IUnknown | IID_IFilterGraph)` → `S_OK`
//! * `QueryInterface(other)` → `E_NOINTERFACE`
//! * `AddRef` → previous_refcount + 1
//! * `Release` → previous_refcount - 1 (never reaching 0 in
//!   practice; we floor at 1 so a buggy codec that double-releases
//!   doesn't kill the host stub mid-test).
//! * Every IFilterGraph method (`AddFilter` / `RemoveFilter` /
//!   `EnumFilters` / `FindFilterByName` / `ConnectDirect` /
//!   `Reconnect` / `Disconnect` / `SetDefaultSyncSource`) →
//!   `E_NOTIMPL`.  None of these are called during the codec's
//!   `JoinFilterGraph` → `ReceiveConnection` path; they exist so
//!   if the codec *does* probe one of them later, it gets a
//!   well-formed reply rather than crashing on an uninitialised
//!   slot.
//!
//! Reference: MSDN
//! "[IFilterGraph (DirectShow)](https://learn.microsoft.com/en-us/windows/win32/api/strmif/nn-strmif-ifiltergraph)"
//! interface — slots 0..2 are IUnknown's; slots 3..10 are
//! IFilterGraph's eight methods in declaration order.

use super::{Guid, IID_IFILTERGRAPH, IID_IPIN, IID_IUNKNOWN};
use crate::emulator::{Cpu, Mmu};
use crate::win32::{HostState, Registry, StubFn, Win32Error};

/// `S_OK = 0` — locally redefined to keep this module
/// self-contained.
const S_OK: u32 = 0x0000_0000;
/// `E_NOINTERFACE = 0x80004002`.
const E_NOINTERFACE: u32 = 0x8000_4002;
/// `E_NOTIMPL = 0x80004001`.
const E_NOTIMPL: u32 = 0x8000_4001;

/// Pseudo-DLL name used when registering host-COM thunks with the
/// stub `Registry`.  The guest never imports from this name;
/// `dispatch_stub` only reads it for tracing.
const HOST_DLL: &str = "host-com.host";

/// Register every host-COM thunk with the stub registry.  Idempotent.
///
/// Each method is a separate registration so the trace-feature
/// `kind=win32_call` events name the IFilterGraph slot directly.
pub fn register(registry: &mut Registry) {
    // IUnknown trio — present on every host vtable.
    registry.register(HOST_DLL, "IFilterGraph::QueryInterface", qi as StubFn, 3);
    registry.register(HOST_DLL, "IFilterGraph::AddRef", addref as StubFn, 1);
    registry.register(HOST_DLL, "IFilterGraph::Release", release as StubFn, 1);
    // IFilterGraph methods — see MSDN strmif.h.
    registry.register(HOST_DLL, "IFilterGraph::AddFilter", notimpl_3 as StubFn, 3);
    registry.register(
        HOST_DLL,
        "IFilterGraph::RemoveFilter",
        notimpl_2 as StubFn,
        2,
    );
    registry.register(
        HOST_DLL,
        "IFilterGraph::EnumFilters",
        notimpl_2 as StubFn,
        2,
    );
    registry.register(
        HOST_DLL,
        "IFilterGraph::FindFilterByName",
        notimpl_3 as StubFn,
        3,
    );
    registry.register(
        HOST_DLL,
        "IFilterGraph::ConnectDirect",
        notimpl_4 as StubFn,
        4,
    );
    registry.register(HOST_DLL, "IFilterGraph::Reconnect", notimpl_2 as StubFn, 2);
    registry.register(HOST_DLL, "IFilterGraph::Disconnect", notimpl_2 as StubFn, 2);
    registry.register(
        HOST_DLL,
        "IFilterGraph::SetDefaultSyncSource",
        notimpl_1 as StubFn,
        1,
    );

    // ---- HostIPin (output-pin role) — round-27 A.2/B reach -----
    //
    // The codec calls `pConnector->QueryDirection()` etc. from
    // inside its own `ReceiveConnection`.  If `pConnector` is the
    // codec's own input pin (round-26's self-loop), QueryDirection
    // reports INPUT and the codec falls into a no-acceptable-type
    // branch.  HostIPin lets a test pass a non-self IPin pointer
    // that QueryDirection's as OUTPUT.
    registry.register(HOST_DLL, "IPin::QueryInterface", pin_qi as StubFn, 3);
    registry.register(HOST_DLL, "IPin::AddRef", addref as StubFn, 1);
    registry.register(HOST_DLL, "IPin::Release", release as StubFn, 1);
    registry.register(HOST_DLL, "IPin::Connect", notimpl_3 as StubFn, 3);
    registry.register(HOST_DLL, "IPin::ReceiveConnection", notimpl_3 as StubFn, 3);
    registry.register(HOST_DLL, "IPin::Disconnect", pin_s_ok_1 as StubFn, 1);
    registry.register(HOST_DLL, "IPin::ConnectedTo", notimpl_2 as StubFn, 2);
    registry.register(
        HOST_DLL,
        "IPin::ConnectionMediaType",
        pin_connection_media_type as StubFn,
        2,
    );
    registry.register(HOST_DLL, "IPin::QueryPinInfo", notimpl_2 as StubFn, 2);
    registry.register(
        HOST_DLL,
        "IPin::QueryDirection",
        pin_query_direction as StubFn,
        2,
    );
    registry.register(HOST_DLL, "IPin::QueryId", notimpl_2 as StubFn, 2);
    registry.register(HOST_DLL, "IPin::QueryAccept", pin_s_ok_2 as StubFn, 2);
    registry.register(
        HOST_DLL,
        "IPin::EnumMediaTypes",
        pin_enum_media_types as StubFn,
        2,
    );
    registry.register(
        HOST_DLL,
        "IPin::QueryInternalConnections",
        notimpl_3 as StubFn,
        3,
    );
    registry.register(HOST_DLL, "IPin::EndOfStream", pin_s_ok_1 as StubFn, 1);
    registry.register(HOST_DLL, "IPin::BeginFlush", pin_s_ok_1 as StubFn, 1);
    registry.register(HOST_DLL, "IPin::EndFlush", pin_s_ok_1 as StubFn, 1);
    registry.register(HOST_DLL, "IPin::NewSegment", pin_s_ok_5 as StubFn, 5);

    // HostIEnumMediaTypes — vended by HostIPin::EnumMediaTypes.
    registry.register(
        HOST_DLL,
        "IEnumMediaTypes::QueryInterface",
        enum_qi as StubFn,
        3,
    );
    registry.register(HOST_DLL, "IEnumMediaTypes::AddRef", addref as StubFn, 1);
    registry.register(HOST_DLL, "IEnumMediaTypes::Release", release as StubFn, 1);
    registry.register(HOST_DLL, "IEnumMediaTypes::Next", enum_next as StubFn, 4);
    registry.register(HOST_DLL, "IEnumMediaTypes::Skip", enum_skip as StubFn, 2);
    registry.register(HOST_DLL, "IEnumMediaTypes::Reset", enum_reset as StubFn, 1);
    registry.register(HOST_DLL, "IEnumMediaTypes::Clone", notimpl_2 as StubFn, 2);
}

/// Lay out a host IFilterGraph in arena memory.  Returns the
/// guest VA of the object pointer — suitable as the `pGraph`
/// argument of `IBaseFilter::JoinFilterGraph`.
///
/// Layout:
///
/// | offset | content                          |
/// |--------|-----------------------------------|
/// | obj    | vtbl_ptr (= obj + 8)             |
/// | obj+4  | refcount (initialised to 1)      |
/// | obj+8  | vtbl[0] = QueryInterface thunk   |
/// | obj+12 | vtbl[1] = AddRef thunk           |
/// | obj+16 | vtbl[2] = Release thunk          |
/// | obj+20 | vtbl[3] = AddFilter thunk        |
/// | …      | …                                |
/// | obj+48 | vtbl[10] = SetDefaultSyncSource  |
///
/// Total footprint is `8 + 11*4 = 52 bytes`; the arena allocator
/// rounds to 16 so we consume 64 bytes per host filter graph.
pub fn mint_host_filter_graph(
    state: &mut HostState,
    mmu: &mut Mmu,
    registry: &Registry,
) -> Result<u32, crate::Error> {
    let obj = state.arena_alloc(64).map_err(crate::Error::Win32)?;
    let vtbl = obj.wrapping_add(8);
    // [obj] = vtbl
    mmu.write_initializer(obj, &vtbl.to_le_bytes())
        .map_err(crate::Error::Trap)?;
    // [obj+4] = refcount = 1
    mmu.write_initializer(obj + 4, &1u32.to_le_bytes())
        .map_err(crate::Error::Trap)?;
    // Build the vtable.
    let methods: [&str; 11] = [
        "IFilterGraph::QueryInterface",
        "IFilterGraph::AddRef",
        "IFilterGraph::Release",
        "IFilterGraph::AddFilter",
        "IFilterGraph::RemoveFilter",
        "IFilterGraph::EnumFilters",
        "IFilterGraph::FindFilterByName",
        "IFilterGraph::ConnectDirect",
        "IFilterGraph::Reconnect",
        "IFilterGraph::Disconnect",
        "IFilterGraph::SetDefaultSyncSource",
    ];
    for (i, name) in methods.iter().enumerate() {
        let thunk = registry.resolve(HOST_DLL, name).ok_or_else(|| {
            crate::Error::Win32(Win32Error::InvalidArgument {
                stub: "mint_host_filter_graph",
                reason: format!("host-com thunk {name:?} not registered"),
            })
        })?;
        mmu.write_initializer(vtbl + (i as u32) * 4, &thunk.to_le_bytes())
            .map_err(crate::Error::Trap)?;
    }
    Ok(obj)
}

/// Round 27 A.2/B — mint a host-side `IPin` that pretends to be
/// an OUTPUT pin advertising `amt_addr` (a guest pointer to a
/// staged `AM_MEDIA_TYPE`).  Returned guest pointer is suitable
/// as the `pConnector` argument of `IPin::ReceiveConnection`.
///
/// Object layout (16-byte aligned):
///
/// | offset | content                    |
/// |--------|-----------------------------|
/// | obj    | vtbl_ptr (= obj + 16)      |
/// | obj+4  | refcount = 1               |
/// | obj+8  | advertised_amt = amt_addr  |
/// | obj+12 | reserved (0)               |
/// | obj+16 | vtbl[0..18] (72 bytes)     |
///
/// Total = 16 + 72 = 88 bytes; arena allocator rounds to 96.
pub fn mint_host_output_pin(
    state: &mut HostState,
    mmu: &mut Mmu,
    registry: &Registry,
    amt_addr: u32,
) -> Result<u32, crate::Error> {
    let obj = state.arena_alloc(96).map_err(crate::Error::Win32)?;
    let vtbl = obj.wrapping_add(16);
    mmu.write_initializer(obj, &vtbl.to_le_bytes())
        .map_err(crate::Error::Trap)?;
    mmu.write_initializer(obj + 4, &1u32.to_le_bytes())
        .map_err(crate::Error::Trap)?;
    mmu.write_initializer(obj + 8, &amt_addr.to_le_bytes())
        .map_err(crate::Error::Trap)?;
    mmu.write_initializer(obj + 12, &0u32.to_le_bytes())
        .map_err(crate::Error::Trap)?;
    let methods: [&str; 18] = [
        "IPin::QueryInterface",
        "IPin::AddRef",
        "IPin::Release",
        "IPin::Connect",
        "IPin::ReceiveConnection",
        "IPin::Disconnect",
        "IPin::ConnectedTo",
        "IPin::ConnectionMediaType",
        "IPin::QueryPinInfo",
        "IPin::QueryDirection",
        "IPin::QueryId",
        "IPin::QueryAccept",
        "IPin::EnumMediaTypes",
        "IPin::QueryInternalConnections",
        "IPin::EndOfStream",
        "IPin::BeginFlush",
        "IPin::EndFlush",
        "IPin::NewSegment",
    ];
    for (i, name) in methods.iter().enumerate() {
        let thunk = registry.resolve(HOST_DLL, name).ok_or_else(|| {
            crate::Error::Win32(Win32Error::InvalidArgument {
                stub: "mint_host_output_pin",
                reason: format!("host-com thunk {name:?} not registered"),
            })
        })?;
        mmu.write_initializer(vtbl + (i as u32) * 4, &thunk.to_le_bytes())
            .map_err(crate::Error::Trap)?;
    }
    Ok(obj)
}

/// Mint a fresh HostIEnumMediaTypes that yields `amt_addr` once
/// (and `S_FALSE` thereafter).  Used by HostIPin::EnumMediaTypes.
fn mint_host_enum_media_types(
    state: &mut HostState,
    mmu: &mut Mmu,
    registry: &Registry,
    amt_addr: u32,
) -> Result<u32, Win32Error> {
    let obj = state.arena_alloc(48)?;
    let vtbl = obj.wrapping_add(16);
    let _ = mmu.write_initializer(obj, &vtbl.to_le_bytes());
    let _ = mmu.write_initializer(obj + 4, &1u32.to_le_bytes());
    let _ = mmu.write_initializer(obj + 8, &amt_addr.to_le_bytes());
    let _ = mmu.write_initializer(obj + 12, &0u32.to_le_bytes()); // cursor
    let methods: [&str; 7] = [
        "IEnumMediaTypes::QueryInterface",
        "IEnumMediaTypes::AddRef",
        "IEnumMediaTypes::Release",
        "IEnumMediaTypes::Next",
        "IEnumMediaTypes::Skip",
        "IEnumMediaTypes::Reset",
        "IEnumMediaTypes::Clone",
    ];
    for (i, name) in methods.iter().enumerate() {
        let thunk =
            registry
                .resolve(HOST_DLL, name)
                .ok_or_else(|| Win32Error::InvalidArgument {
                    stub: "mint_host_enum_media_types",
                    reason: format!("thunk {name:?} not registered"),
                })?;
        let _ = mmu.write_initializer(vtbl + (i as u32) * 4, &thunk.to_le_bytes());
    }
    Ok(obj)
}

// ---- Stub implementations ---------------------------------------------

/// `QueryInterface(this, REFIID, void**)`.
///
/// `[esp+4]` = `this`, `[esp+8]` = `REFIID*`, `[esp+12]` = `ppv*`.
///
/// We resolve `IID_IUnknown` and `IID_IFilterGraph` to `this`;
/// every other IID returns `E_NOINTERFACE` and zeros `*ppv`.
fn qi(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let this = arg(cpu, mmu, 0)?;
    let piid = arg(cpu, mmu, 1)?;
    let ppv = arg(cpu, mmu, 2)?;
    if ppv == 0 {
        return Ok(crate::com::E_POINTER);
    }
    // Default *ppv = NULL.
    let _ = mmu.write_initializer(ppv, &0u32.to_le_bytes());
    if piid == 0 {
        return Ok(crate::com::E_POINTER);
    }
    let iid = Guid::load(mmu, piid).map_err(|t| trap("HostIFilterGraph::QI", t))?;
    if iid == IID_IUNKNOWN || iid == IID_IFILTERGRAPH {
        // Bump refcount.
        if let Ok(rc) = mmu.load32(this + 4) {
            let _ = mmu.write_initializer(this + 4, &rc.saturating_add(1).to_le_bytes());
        }
        // *ppv = this.
        let _ = mmu.write_initializer(ppv, &this.to_le_bytes());
        // Bookkeep on the host side too — same rules as a guest-
        // returned object.
        state.com.intern(this, Some(iid));
        return Ok(S_OK);
    }
    Ok(E_NOINTERFACE)
}

/// `AddRef(this)`. Returns the new refcount.
fn addref(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let this = arg(cpu, mmu, 0)?;
    let rc = mmu
        .load32(this + 4)
        .map_err(|t| trap("HostIFilterGraph::AddRef", t))?;
    let nrc = rc.saturating_add(1);
    mmu.write_initializer(this + 4, &nrc.to_le_bytes())
        .map_err(|t| trap("HostIFilterGraph::AddRef", t))?;
    Ok(nrc)
}

/// `Release(this)`. Returns the new refcount; floors at 1 (the
/// host object lives forever).
fn release(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let this = arg(cpu, mmu, 0)?;
    let rc = mmu
        .load32(this + 4)
        .map_err(|t| trap("HostIFilterGraph::Release", t))?;
    let nrc = if rc > 1 { rc - 1 } else { 1 };
    mmu.write_initializer(this + 4, &nrc.to_le_bytes())
        .map_err(|t| trap("HostIFilterGraph::Release", t))?;
    Ok(nrc)
}

/// Generic `E_NOTIMPL` stub for an N-arg IFilterGraph method.
/// Each method has a distinct registration so the trace event
/// names the slot.
fn notimpl_1(_: &mut Cpu, _: &mut Mmu, _: &mut HostState, _: &Registry) -> Result<u32, Win32Error> {
    Ok(E_NOTIMPL)
}
fn notimpl_2(_: &mut Cpu, _: &mut Mmu, _: &mut HostState, _: &Registry) -> Result<u32, Win32Error> {
    Ok(E_NOTIMPL)
}
fn notimpl_3(_: &mut Cpu, _: &mut Mmu, _: &mut HostState, _: &Registry) -> Result<u32, Win32Error> {
    Ok(E_NOTIMPL)
}
fn notimpl_4(_: &mut Cpu, _: &mut Mmu, _: &mut HostState, _: &Registry) -> Result<u32, Win32Error> {
    Ok(E_NOTIMPL)
}

// ---- HostIPin stubs --------------------------------------------------

/// `IPin::QueryInterface(this, REFIID, void**)`.
/// Resolves IUnknown / IPin to `this`; everything else fails.
fn pin_qi(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let this = arg(cpu, mmu, 0)?;
    let piid = arg(cpu, mmu, 1)?;
    let ppv = arg(cpu, mmu, 2)?;
    if ppv == 0 || piid == 0 {
        return Ok(crate::com::E_POINTER);
    }
    let _ = mmu.write_initializer(ppv, &0u32.to_le_bytes());
    let iid = Guid::load(mmu, piid).map_err(|t| trap("HostIPin::QI", t))?;
    if iid == IID_IUNKNOWN || iid == IID_IPIN {
        if let Ok(rc) = mmu.load32(this + 4) {
            let _ = mmu.write_initializer(this + 4, &rc.saturating_add(1).to_le_bytes());
        }
        let _ = mmu.write_initializer(ppv, &this.to_le_bytes());
        state.com.intern(this, Some(iid));
        return Ok(S_OK);
    }
    Ok(E_NOINTERFACE)
}

/// `IPin::QueryDirection(this, PIN_DIRECTION* pPinDir)`.  Always
/// reports `PIN_OUTPUT (1)` — that is the role the host pin plays
/// for the codec's downstream-input-pin handshake.
fn pin_query_direction(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let _this = arg(cpu, mmu, 0)?;
    let p_pin_dir = arg(cpu, mmu, 1)?;
    if p_pin_dir == 0 {
        return Ok(crate::com::E_POINTER);
    }
    // PIN_OUTPUT = 1.
    mmu.write_initializer(p_pin_dir, &1u32.to_le_bytes())
        .map_err(|t| trap("HostIPin::QueryDirection", t))?;
    Ok(S_OK)
}

/// `IPin::QueryAccept(this, AM_MEDIA_TYPE* pmt)` → `S_OK`.  We
/// pretend to accept any type the codec offers — this method is
/// only called when the codec is renegotiating, which the round-27
/// scope doesn't exercise.
fn pin_s_ok_2(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(S_OK)
}

/// 1-arg `S_OK` stub for `EndOfStream`/`BeginFlush`/`EndFlush`/
/// `Disconnect` — fire-and-forget control messages.
fn pin_s_ok_1(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(S_OK)
}

/// 5-arg `S_OK` stub for `IPin::NewSegment(this, tStart_lo,
/// tStart_hi, tStop_lo, tStop_hi, double rate-as-2-dwords)`.
/// Stdcall passes the LONGLONG `tStart` / `tStop` as adjacent
/// dword pairs; `double` rate is also two dwords on stdcall.
fn pin_s_ok_5(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(S_OK)
}

/// `IPin::ConnectionMediaType(this, AM_MEDIA_TYPE* pmt)` — copy
/// the host-pin's advertised AMT (72 bytes) into `pmt`.  Used by
/// the codec when it wants to inspect the upstream's connected
/// type.  Per MSDN, the caller is responsible for freeing the
/// `pbFormat` allocation; we leave `pbFormat` pointing at the
/// host arena (read-only as far as the codec is concerned).
fn pin_connection_media_type(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let this = arg(cpu, mmu, 0)?;
    let pmt = arg(cpu, mmu, 1)?;
    if pmt == 0 {
        return Ok(crate::com::E_POINTER);
    }
    // Read the advertised AMT pointer from this+8.
    let amt_src = mmu
        .load32(this + 8)
        .map_err(|t| trap("HostIPin::ConnectionMediaType", t))?;
    if amt_src == 0 {
        return Ok(0x8004_0211 /* VFW_E_NOT_CONNECTED */);
    }
    // Bulk copy 72 bytes.
    for i in 0..72u32 {
        let b = mmu
            .load8(amt_src + i)
            .map_err(|t| trap("HostIPin::ConnectionMediaType", t))?;
        mmu.store8(pmt + i, b)
            .map_err(|t| trap("HostIPin::ConnectionMediaType", t))?;
    }
    Ok(S_OK)
}

/// `IPin::EnumMediaTypes(this, IEnumMediaTypes** ppEnum)`.  Mints
/// a fresh HostIEnumMediaTypes that yields the advertised AMT
/// once.
fn pin_enum_media_types(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
    registry: &Registry,
) -> Result<u32, Win32Error> {
    let this = arg(cpu, mmu, 0)?;
    let pp = arg(cpu, mmu, 1)?;
    if pp == 0 {
        return Ok(crate::com::E_POINTER);
    }
    let amt_src = mmu
        .load32(this + 8)
        .map_err(|t| trap("HostIPin::EnumMediaTypes", t))?;
    let new_enum = mint_host_enum_media_types(state, mmu, registry, amt_src)?;
    mmu.write_initializer(pp, &new_enum.to_le_bytes())
        .map_err(|t| trap("HostIPin::EnumMediaTypes", t))?;
    Ok(S_OK)
}

// ---- HostIEnumMediaTypes stubs ---------------------------------------

/// `QueryInterface` for the enumerator.  Same shape as the pin's
/// QI but no IPin acceptance — only IUnknown.
fn enum_qi(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let this = arg(cpu, mmu, 0)?;
    let piid = arg(cpu, mmu, 1)?;
    let ppv = arg(cpu, mmu, 2)?;
    if ppv == 0 || piid == 0 {
        return Ok(crate::com::E_POINTER);
    }
    let _ = mmu.write_initializer(ppv, &0u32.to_le_bytes());
    let iid = Guid::load(mmu, piid).map_err(|t| trap("HostIEnum::QI", t))?;
    if iid == IID_IUNKNOWN {
        if let Ok(rc) = mmu.load32(this + 4) {
            let _ = mmu.write_initializer(this + 4, &rc.saturating_add(1).to_le_bytes());
        }
        let _ = mmu.write_initializer(ppv, &this.to_le_bytes());
        state.com.intern(this, Some(iid));
        return Ok(S_OK);
    }
    Ok(E_NOINTERFACE)
}

/// `IEnumMediaTypes::Next(this, ULONG cMediaTypes, AM_MEDIA_TYPE**
/// ppMediaTypes, ULONG* pcFetched)`.
///
/// Returns `S_OK` with `*pcFetched = 1` on the first call (yields
/// the AMT pointer the host pin was minted with), `S_FALSE` on
/// subsequent calls.  `cMediaTypes > 1` is treated as "give me up
/// to N" — we only ever yield 1.
fn enum_next(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let this = arg(cpu, mmu, 0)?;
    let c = arg(cpu, mmu, 1)?;
    let pp = arg(cpu, mmu, 2)?;
    let p_fetched = arg(cpu, mmu, 3)?;
    if pp == 0 {
        return Ok(crate::com::E_POINTER);
    }
    let cursor = mmu.load32(this + 12).unwrap_or(0);
    if c == 0 {
        if p_fetched != 0 {
            let _ = mmu.write_initializer(p_fetched, &0u32.to_le_bytes());
        }
        return Ok(S_OK);
    }
    if cursor == 0 {
        let amt = mmu
            .load32(this + 8)
            .map_err(|t| trap("HostIEnum::Next", t))?;
        mmu.write_initializer(pp, &amt.to_le_bytes())
            .map_err(|t| trap("HostIEnum::Next", t))?;
        if p_fetched != 0 {
            let _ = mmu.write_initializer(p_fetched, &1u32.to_le_bytes());
        }
        let _ = mmu.write_initializer(this + 12, &1u32.to_le_bytes());
        // S_OK only when we returned exactly the requested count;
        // when caller asks for >1 we return S_FALSE per the spec.
        if c == 1 {
            return Ok(S_OK);
        }
        return Ok(crate::com::S_FALSE);
    }
    // Exhausted.
    let _ = mmu.write_initializer(pp, &0u32.to_le_bytes());
    if p_fetched != 0 {
        let _ = mmu.write_initializer(p_fetched, &0u32.to_le_bytes());
    }
    Ok(crate::com::S_FALSE)
}

/// `IEnumMediaTypes::Skip(this, ULONG cMediaTypes)`.  Advances
/// the cursor.  We only have one item so any non-zero `cMediaTypes`
/// exhausts.
fn enum_skip(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let this = arg(cpu, mmu, 0)?;
    let _ = mmu.write_initializer(this + 12, &1u32.to_le_bytes());
    Ok(S_OK)
}

/// `IEnumMediaTypes::Reset(this)`.  Cursor → 0.
fn enum_reset(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let this = arg(cpu, mmu, 0)?;
    let _ = mmu.write_initializer(this + 12, &0u32.to_le_bytes());
    Ok(S_OK)
}

fn arg(cpu: &Cpu, mmu: &Mmu, n: u32) -> Result<u32, Win32Error> {
    crate::win32::arg_dword(cpu, mmu, n).map_err(|t| trap("HostIFilterGraph::arg", t))
}

fn trap(stub: &'static str, t: crate::emulator::Trap) -> Win32Error {
    Win32Error::InvalidArgument {
        stub,
        reason: format!("{t}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::call::call_method;
    use crate::Sandbox;

    #[test]
    fn host_filter_graph_layout_has_eleven_method_slots() {
        let mut sb = Sandbox::new();
        let g = mint_host_filter_graph(&mut sb.host, &mut sb.mmu, &sb.registry).unwrap();
        // [g] = vtbl_ptr; vtbl_ptr = g + 8 by construction.
        let vtbl = sb.mmu.load32(g).unwrap();
        assert_eq!(vtbl, g + 8);
        // 11 slots populated with non-zero thunk addresses (the
        // IUnknown trio + 8 IFilterGraph methods).
        for i in 0..11u32 {
            let m = sb.mmu.load32(vtbl + i * 4).unwrap();
            assert!(m != 0, "vtbl slot {i} unmapped");
            assert!(
                sb.registry.is_thunk(m),
                "vtbl slot {i} = {m:#010x} not a registered thunk"
            );
        }
    }

    #[test]
    fn host_filter_graph_addref_release_round_trip() {
        let mut sb = Sandbox::new();
        let g = mint_host_filter_graph(&mut sb.host, &mut sb.mmu, &sb.registry).unwrap();
        // Initial refcount = 1.
        assert_eq!(sb.mmu.load32(g + 4).unwrap(), 1);
        // AddRef → 2.
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            g,
            crate::com::SLOT_ADD_REF,
            &[],
        )
        .unwrap();
        assert_eq!(r, 2);
        assert_eq!(sb.mmu.load32(g + 4).unwrap(), 2);
        // Release → 1.
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            g,
            crate::com::SLOT_RELEASE,
            &[],
        )
        .unwrap();
        assert_eq!(r, 1);
        // Release floors at 1.
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            g,
            crate::com::SLOT_RELEASE,
            &[],
        )
        .unwrap();
        assert_eq!(r, 1);
    }

    #[test]
    fn host_filter_graph_query_interface_for_iunknown_returns_self() {
        let mut sb = Sandbox::new();
        let g = mint_host_filter_graph(&mut sb.host, &mut sb.mmu, &sb.registry).unwrap();
        let scratch = sb.host.arena_alloc(20).unwrap();
        IID_IUNKNOWN.stage(&mut sb.mmu, scratch).unwrap();
        sb.mmu
            .write_initializer(scratch + 16, &0u32.to_le_bytes())
            .unwrap();
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            g,
            crate::com::SLOT_QUERY_INTERFACE,
            &[scratch, scratch + 16],
        )
        .unwrap();
        assert_eq!(r, S_OK);
        assert_eq!(sb.mmu.load32(scratch + 16).unwrap(), g);
    }

    #[test]
    fn host_filter_graph_query_interface_unknown_iid_rejected() {
        let mut sb = Sandbox::new();
        let g = mint_host_filter_graph(&mut sb.host, &mut sb.mmu, &sb.registry).unwrap();
        let scratch = sb.host.arena_alloc(20).unwrap();
        // IID_IBaseFilter — not satisfied by the host filter graph.
        crate::com::IID_IBASEFILTER
            .stage(&mut sb.mmu, scratch)
            .unwrap();
        sb.mmu
            .write_initializer(scratch + 16, &0u32.to_le_bytes())
            .unwrap();
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            g,
            crate::com::SLOT_QUERY_INTERFACE,
            &[scratch, scratch + 16],
        )
        .unwrap();
        assert_eq!(r, E_NOINTERFACE);
        assert_eq!(sb.mmu.load32(scratch + 16).unwrap(), 0);
    }

    #[test]
    fn host_filter_graph_addfilter_returns_e_notimpl() {
        let mut sb = Sandbox::new();
        let g = mint_host_filter_graph(&mut sb.host, &mut sb.mmu, &sb.registry).unwrap();
        // AddFilter(this, IBaseFilter*, LPCWSTR) — slot 3.
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            g,
            3,
            &[0xDEAD_BEEF, 0xCAFE_F00D],
        )
        .unwrap();
        assert_eq!(r, E_NOTIMPL);
    }
}
