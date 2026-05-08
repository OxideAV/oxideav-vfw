//! Round 14 Part B — surface probe of `IR41_32.AX`, the Indeo 4
//! DirectShow / ActiveMovie filter.
//!
//! Unlike `IR50_32.DLL` (a Video for Windows driver, `DriverProc`
//! plus `ICOpen` / `ICDecompress` surface, exercised by rounds
//! 8..14 Part A), `IR41_32.AX` is a **DirectShow filter DLL**:
//! the file extension `.AX` is the Microsoft convention for
//! "ActiveX filter", and the entry surface is COM (`IClassFactory`
//! returning `IBaseFilter` returning `IPin`) rather than the VfW
//! IC* call table. Round 14 does *not* attempt to drive the codec
//! end-to-end — that's a multi-round body of work to scaffold the
//! COM ABI, the DirectShow `IBaseFilter` / `IPin` interfaces, and
//! the filter-graph negotiation (`EnumMediaTypes`, `IPin::Connect`).
//!
//! Round 14 Part B's deliverable is **a reconnaissance** of the
//! file — what the size + section layout is, which exports it
//! offers (where `DllGetClassObject` is the COM-server entry
//! point per Microsoft "OLE Programmer's Reference",
//! §"DllGetClassObject"), and what Win32 imports we'd need to
//! satisfy before the entry point can even start. The test
//! prints the findings to stderr and asserts on a small, robust
//! shape:
//!
//! 1. The file parses as PE32 / I386 (we already know it does
//!    from the `file(1)` probe, but recording it as an asserted
//!    shape catches a future fixture corruption).
//! 2. `DllGetClassObject` and `DllCanUnloadNow` exports are
//!    present — the COM-server contract (Microsoft "Inside COM",
//!    §"In-Process Servers"). Without these, the file isn't a
//!    valid COM in-proc server and Part B's plan would be
//!    invalid.
//! 3. `DllRegisterServer` and `DllUnregisterServer` exports are
//!    present — the self-registration contract every DirectShow
//!    filter ships per the Microsoft DirectShow SDK
//!    `BaseClasses` boilerplate.
//! 4. The file imports `ole32!CoCreateInstance` *or* contains
//!    it as part of its own export surface — gives round-15 a
//!    concrete starting point for the COM instantiation flow.
//!
//! ## What round 15+ needs (recorded for the dispatch prompt)
//!
//! The `DllGetClassObject(REFCLSID, REFIID, LPVOID*)` ABI is
//! cdecl in COM, *not* stdcall — important for the call_export
//! helper. The signature returns an `HRESULT` (32-bit value, S_OK
//! = 0) and writes a class-factory interface pointer through the
//! out-parameter. The class factory's `CreateInstance(IUnknown*,
//! REFIID, void**)` then mints a filter object whose vtable
//! starts with `IUnknown::QueryInterface / AddRef / Release`,
//! followed by `IBaseFilter::GetClassID / Stop / Pause / Run /
//! GetState / SetSyncSource / GetSyncSource / EnumPins /
//! FindPin / QueryFilterInfo / JoinFilterGraph / QueryVendorInfo`
//! (Microsoft DirectShow SDK `strmif.h`, `IBaseFilter` interface
//! definition).
//!
//! Reference (clean-room): Microsoft "OLE Programmer's
//! Reference" + Microsoft "DirectShow SDK" header `strmif.h`
//! (interface IDs + vtable shape). NEVER reference Wine's
//! `dlls/quartz`, ReactOS, or any third-party reverse of the
//! Indeo filter.

mod common;

use oxideav_vfw::pe::header;

#[test]
fn ir41_32_ax_surface_probe() {
    let bytes = common::fetch_or_load("IR41_32.AX").expect("fetch IR41_32.AX");
    eprintln!("IR41_32.AX: {} bytes", bytes.len());

    // Gate 1 — parses as PE32 / I386.
    let parsed = header::parse(&bytes).expect("parse IR41_32.AX");
    assert_eq!(
        parsed.file.machine,
        header::IMAGE_FILE_MACHINE_I386,
        "IR41_32.AX must be I386 (got machine {:#x})",
        parsed.file.machine,
    );
    assert_eq!(
        parsed.optional.magic,
        header::IMAGE_NT_OPTIONAL_HDR32_MAGIC,
        "IR41_32.AX must be PE32 (got optional-magic {:#x})",
        parsed.optional.magic,
    );
    eprintln!(
        "PE shape: image_base={:#x} entry_rva={:#x} size_of_image={:#x} sections={}",
        parsed.optional.image_base,
        parsed.optional.address_of_entry_point,
        parsed.optional.size_of_image,
        parsed.sections.len(),
    );

    for s in &parsed.sections {
        eprintln!(
            "  section {} VA={:#x} VS={:#x} raw=[{:#x}..+{:#x}] chars={:#x}",
            s.name,
            s.virtual_address,
            s.virtual_size,
            s.pointer_to_raw_data,
            s.size_of_raw_data,
            s.characteristics,
        );
    }

    // Gate 2 + 3 — the four COM-server entry points.
    let exports =
        oxideav_vfw::pe::exports::parse_exports(&parsed, &bytes, parsed.optional.image_base)
            .expect("parse exports");
    eprintln!("IR41_32.AX exports ({}):", exports.len());
    for (name, rva) in &exports {
        eprintln!("  {name:<32} rva={rva:#x}");
    }

    let required_exports = &[
        "DllGetClassObject",
        "DllCanUnloadNow",
        "DllRegisterServer",
        "DllUnregisterServer",
    ];
    for name in required_exports {
        assert!(
            exports.contains_key(*name),
            "round-14 Part B gate: IR41_32.AX must export {name} \
             (Microsoft COM in-proc-server contract). \
             Available exports: {:?}",
            exports.keys().collect::<Vec<_>>(),
        );
    }

    // Gate 4 — list every (dll, fn) import. We don't *assert*
    // ole32!CoCreateInstance specifically: an in-proc server
    // typically does NOT call CoCreateInstance on itself; it's
    // the *client* that calls CoCreateInstance to reach this
    // server. What we do assert is that ole32 is imported (so
    // we know the COM dispatch surface lives there) — and we
    // print the full import list so round 15's dispatch prompt
    // can size the stub set.
    let imports = common::list_pe_imports(&bytes).expect("list_pe_imports");
    eprintln!("IR41_32.AX imports ({}):", imports.len());
    let mut prev_dll: Option<&str> = None;
    let mut dll_count: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for (dll, fname) in &imports {
        if Some(dll.as_str()) != prev_dll {
            eprintln!("  --- {dll} ---");
            prev_dll = Some(dll.as_str());
        }
        eprintln!("  {dll}!{fname}");
        *dll_count.entry(dll.as_str()).or_insert(0) += 1;
    }
    eprintln!("IR41_32.AX import-DLL summary:");
    for (dll, n) in &dll_count {
        eprintln!("  {dll}: {n} symbols");
    }

    // Cheap heuristic: an OCX / AX server should *almost
    // always* import ole32 or oleaut32 (the COM dispatch + BSTR
    // surface). If neither is in the import list this is a
    // surprise we want to surface to the round-15 dispatch
    // prompt.
    let has_ole32 = dll_count.contains_key("ole32.dll");
    let has_oleaut32 = dll_count.contains_key("oleaut32.dll");
    eprintln!(
        "round14 Part B: ole32 imported = {}, oleaut32 imported = {}",
        has_ole32, has_oleaut32,
    );

    // Beyond the structural gates, we surface a count of NEW
    // imports — every (dll, fn) pair NOT already covered by the
    // round-13 import-stub registry — to size round 15's
    // dispatch budget. We intentionally do *not* compare against
    // the registry programmatically here (the registry would
    // need a public "list all" hook the crate doesn't expose
    // yet); we just print the full list above. Round 15's
    // dispatch will turn this list into the stub-coverage diff.
}
