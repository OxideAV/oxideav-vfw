//! Round 66 — generate per-fixture LUT-read trace artifacts
//! for the MS-MPEG-4 v3 docs collaborator.
//!
//! This binary loads `mpg4c32.dll` (the wmpcdcs8-2001 build),
//! installs `Sandbox::watch` on every candidate VLC LUT region
//! listed in `docs/codec/msmpeg4-mpg4c32-rdata-map.md`, decodes
//! each of the 10 MS-MPEG-4 v3 fixtures from
//! `docs/video/msmpeg4-fixtures/`, and writes one JSONL artifact
//! per fixture under `docs/codec/msmpeg4-traces/`.
//!
//! Run with `--features trace`:
//!
//! ```text
//! CARGO_TARGET_DIR=/tmp/oxideav-vfw-r66-target \
//!   cargo run --release -p oxideav-vfw \
//!   --features trace --example gen_msmpeg4_traces
//! ```
//!
//! Missing-DLL is fatal (we always commit the binaries alongside
//! the docs, so absence here means a corrupted checkout);
//! missing-fixture is logged but not considered fatal so the
//! round can still produce partial artifacts.

#[cfg(feature = "trace")]
#[allow(dead_code)]
#[path = "../tests/common/avi_extractor.rs"]
mod avi_extractor;

#[cfg(feature = "trace")]
mod inner {
    use super::avi_extractor;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use oxideav_vfw::win32::vfw32::ICDECOMPRESS_NOTKEYFRAME;
    use oxideav_vfw::{Bih, Sandbox, WatchMode, DLL_PROCESS_ATTACH};

    // LUT candidates per `docs/codec/msmpeg4-mpg4c32-rdata-map.md`.
    // (RVA, byte_size, label) tuples. RVAs are inside `.data`; the
    // runtime adds `image_base` to derive the absolute guest VA.
    const LUT_CANDIDATES: &[(u32, u32, &str)] = &[
        (0x0003_a4c8, 64, "lut_bootstrap"),
        (0x0003_a708, 128, "lut_dc_size"),
        (0x0004_f938, 16376, "lut_ac_coef_g0"),
        (0x0005_3940, 1024, "lut_fanout_routing"),
        (0x0005_3d42, 1660, "lut_mv_vlc"),
        (0x0005_43c0, 510, "lut_mb_type"),
        (0x0005_45c0, 12288, "lut_ac_coef_g1_alt"),
        (0x0005_7860, 168, "lut_scan_a"),
        (0x0005_7bf0, 186, "lut_scan_b"),
        (0x0005_7f00, 148, "lut_scan_c"),
        (0x0005_81a8, 132, "lut_scan_d"),
        (0x0005_8230, 102, "lut_scan_e"),
        (0x0005_844c, 74, "lut_scan_f"),
    ];

    const FIXTURES: &[(&str, u32)] = &[
        // Multi-frame fixtures (exercise P-frame paths + alt-MV-VLC + skip-MB)
        ("gop-30-352x288", 6),
        ("with-skip-mbs-352x288", 5),
        ("motion-pan-352x288", 4),
        ("intra-pred-active-352x288", 1),
        ("qscale-high-352x288", 1),
        // I-only / smaller fixtures (exercise intra paths exclusively)
        ("qscale-low-352x288", 1),
        ("i-only-352x288-cif", 1),
        ("tiny-i-only-176x144", 1),
        ("fourcc-MP43", 1),
        ("i-frame-then-p-frame-176x144", 2),
    ];

    struct SharedSink(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedSink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn workspace_root() -> PathBuf {
        let manifest = std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        PathBuf::from(manifest)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn dll_path() -> PathBuf {
        workspace_root().join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/mpg4c32.dll")
    }

    fn fixture_path(name: &str) -> PathBuf {
        workspace_root().join(format!("docs/video/msmpeg4-fixtures/{name}/input.avi"))
    }

    fn out_path(name: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("docs/codec/msmpeg4-traces")
            .join(format!("{name}.jsonl"))
    }

    fn drive_one_fixture(
        dll_bytes: &[u8],
        avi_bytes: &[u8],
        n: u32,
    ) -> Result<(Vec<u8>, usize, u64), String> {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut sb = Sandbox::new();
        sb.set_trace_sink(Box::new(SharedSink(Arc::clone(&buf))));
        sb.cpu.set_instr_limit(8_000_000_000);

        let img = sb
            .load("mpg4c32.dll", dll_bytes)
            .map_err(|e| format!("load: {e}"))?;
        let image_base = img.image_base;
        sb.call_dll_main(&img, DLL_PROCESS_ATTACH)
            .map_err(|e| format!("dll_main: {e}"))?;
        sb.install_codec(&img)
            .map_err(|e| format!("install_codec: {e}"))?;

        // Install watchpoints for every candidate LUT region.
        for (rva, size, _label) in LUT_CANDIDATES {
            sb.watch(image_base.wrapping_add(*rva), *size, WatchMode::Read);
        }

        let s0 = avi_extractor::extract_video_sample(avi_bytes, 0)
            .map_err(|e| format!("avi sample 0: {e}"))?;
        let (width, height) = (s0.width, s0.height);

        let fcc_video = u32::from_le_bytes(*b"VIDC");
        let fcc_handler = u32::from_le_bytes(*b"MP43");
        let hic = sb
            .ic_open(fcc_video, fcc_handler, 2)
            .map_err(|e| format!("ic_open: {e}"))?;
        if hic == 0 {
            return Err("ic_open returned 0 (codec rejected MP43)".into());
        }

        let bih_in_template = Bih {
            bi_size: 40,
            width: width as i32,
            height: height as i32,
            planes: 1,
            bit_count: 24,
            compression: *b"MP43",
            size_image: s0.bytes.len() as u32,
            ..Bih::default()
        };
        let output = Bih {
            bi_size: 40,
            width: width as i32,
            height: height as i32,
            planes: 1,
            bit_count: 24,
            compression: [0; 4],
            size_image: width * height * 3,
            ..Bih::default()
        };

        let q = sb
            .ic_decompress_query(hic, &bih_in_template, Some(&output))
            .map_err(|e| format!("ic_decompress_query: {e}"))?;
        if q != 0 {
            return Err(format!("ic_decompress_query → {q:#010x} (want 0)"));
        }
        let begin = sb
            .ic_decompress_begin(hic, &bih_in_template, &output)
            .map_err(|e| format!("ic_decompress_begin: {e}"))?;
        if begin != 0 {
            return Err(format!("ic_decompress_begin → {begin:#010x} (want 0)"));
        }

        let cap = output.size_image;
        let mut ok_frames = 0usize;
        let pre_instr = sb.cpu.instr_count;
        for i in 0..n {
            let s = match avi_extractor::extract_video_sample(avi_bytes, i) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("    sample {i} unavailable: {e}");
                    break;
                }
            };
            let bih_in = Bih {
                size_image: s.bytes.len() as u32,
                ..bih_in_template.clone()
            };
            let flags = if i == 0 { 0 } else { ICDECOMPRESS_NOTKEYFRAME };
            let (rc, _out) = sb
                .ic_decompress(hic, flags, &bih_in, &s.bytes, &output, cap)
                .map_err(|e| format!("ic_decompress(sample {i}): {e}"))?;
            if rc == 0 {
                ok_frames += 1;
            } else {
                eprintln!("    frame {i} returned lr={rc:#010x}");
                break;
            }
        }
        let instrs = sb.cpu.instr_count.saturating_sub(pre_instr);
        let _ = sb.ic_decompress_end(hic);
        let _ = sb.ic_close(hic);

        let bytes = buf.lock().unwrap().clone();
        Ok((bytes, ok_frames, instrs))
    }

    pub fn run() {
        let dll_path = dll_path();
        let dll_bytes =
            std::fs::read(&dll_path).unwrap_or_else(|e| panic!("read {}: {e}", dll_path.display()));
        eprintln!(
            "[gen] dll: {} ({} bytes)",
            dll_path.display(),
            dll_bytes.len()
        );

        let out_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("docs/codec/msmpeg4-traces");
        std::fs::create_dir_all(&out_dir).unwrap();

        let mut total_bytes = 0u64;
        let mut total_fixtures = 0usize;
        for (name, n) in FIXTURES {
            let fpath = fixture_path(name);
            if !fpath.is_file() {
                eprintln!(
                    "[gen] {name}: fixture missing at {}; skipping",
                    fpath.display()
                );
                continue;
            }
            let avi_bytes = std::fs::read(&fpath).expect("read fixture");
            eprintln!(
                "[gen] {name}: avi {} bytes, target {n} frames",
                avi_bytes.len()
            );
            match drive_one_fixture(&dll_bytes, &avi_bytes, *n) {
                Ok((jsonl, ok_frames, instrs)) => {
                    let out = out_path(name);
                    std::fs::write(&out, &jsonl).expect("write trace");
                    eprintln!(
                        "[gen]   → {} ({} bytes, {ok_frames} frames OK, {instrs} instrs)",
                        out.display(),
                        jsonl.len(),
                    );
                    total_bytes += jsonl.len() as u64;
                    total_fixtures += 1;
                }
                Err(e) => {
                    eprintln!("[gen] {name}: ERROR {e}");
                }
            }
        }
        eprintln!(
            "[gen] DONE — {} fixtures, {} total bytes of trace data",
            total_fixtures, total_bytes
        );
    }
}

#[cfg(feature = "trace")]
fn main() {
    inner::run();
}

#[cfg(not(feature = "trace"))]
fn main() {
    eprintln!("This example requires `--features trace`. Rerun:");
    eprintln!("  cargo run --release -p oxideav-vfw --features trace --example gen_msmpeg4_traces");
    std::process::exit(2);
}
