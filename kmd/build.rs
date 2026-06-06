//! Build script for the Helios KMD (WDDM Display-Only Driver).
//!
//! Two jobs:
//!   1. Generate Rust bindings for the WDDM display-miniport DDIs. `wdk-sys`
//!      only generates a fixed set of `ApiSubset`s (Base/Wdf/Gpio/Hid/...) and
//!      has NO display subset and no "add a header" API, so the entire
//!      `dispmprt.h` / `d3dkmddi.h` surface (KMDDOD_INITIALIZATION_DATA,
//!      DXGKRNL_INTERFACE, every DXGKARG_*, DxgkInitializeDisplayOnlyDriver, ...)
//!      is missing. We run our own bindgen over those headers, reusing the clang
//!      args / include paths that `wdk-build` computes via `Builder::wdk_default`.
//!   2. Configure downstream linking against the WDK (`configure_binary_build`)
//!      and link `displib.lib`, which exports `DxgkInitializeDisplayOnlyDriver`.
//!
//! The generated file lands at `$OUT_DIR/dxgk_bindings.rs` and is pulled in by
//! `src/dxgk.rs`.
//!
//! Phase-7 display pivot (DISPLAY.md §3.1): this is the SAME bindgen mechanism the
//! pre-pivot render miniport used (recovered from git `658168f:kmd/build.rs`),
//! but scoped to the **Display-Only** surface — `displib.lib` STAYS (it exports
//! the DOD entry point) while the render `DxgkInitialize` + AddAdapter cap DDIs
//! are NOT used. `KMDDOD_INITIALIZATION_DATA` is allowlisted explicitly (it does
//! not start with `DXGK`, so the `DXGK.*` type allowlist misses it); it pulls in
//! the DOD DDI fn typedefs + their `DXGKARG_*` arg structs transitively.

use wdk_build::{BuilderExt, Config};

/// Headers that declare the WDDM display-miniport DDIs we implement.
///
/// `DXGKDDI_INTERFACE_VERSION` is left at the header default (WDDM 3.2 on the
/// 26100 WDK) so every generated struct matches the buffers 24H2 dxgkrnl hands a
/// native driver; we declare the same version in `KMDDOD_INITIALIZATION_DATA.Version`
/// (consistent → no BUFFER_TOO_SMALL). Unlike the rejected render path, a DOD has
/// no AddAdapter capability/version contract, so the version only governs struct
/// layout here.
const DXGK_HEADER_CONTENTS: &str = r#"
#include <ntddk.h>
#include <dispmprt.h>
#include <d3dkmddi.h>
"#;

/// Base types that `wdk-sys` already defines. We blocklist them here and import
/// `wdk_sys::*` so DDI code uses one canonical set instead of two incompatible
/// copies. Extend this list as the first build reports conflicts.
const BLOCKLISTED_BASE_TYPES: &[&str] = &[
    "_?DEVICE_OBJECT",
    "_?DRIVER_OBJECT",
    "_?UNICODE_STRING",
    "_?LARGE_INTEGER",
    "_?ULARGE_INTEGER",
    "_?LIST_ENTRY",
    "_?SINGLE_LIST_ENTRY",
    "_?PHYSICAL_ADDRESS",
    "_?GUID",
    "_?KEVENT",
    "_?DISPATCHER_HEADER",
    "_?IRP",
    "_?IO_STACK_LOCATION",
    "_?KDPC",
    "_?KSPIN_LOCK",
    "NTSTATUS",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    generate_dxgk_bindings()?;

    // Emit the link configuration for a WDK binary (resolves ntoskrnl, etc.).
    Config::from_env_auto()?.configure_binary_build()?;

    // displib.lib provides DxgkInitializeDisplayOnlyDriver — the WDDM
    // display-only entry that registers our DOD DDI table. wdk-build links the
    // base kernel libs (ntoskrnl, hal, wmilib, ...) but not the display-miniport
    // import lib, so add it here. Its directory is already on the linker search
    // path (km\<ver>\x64).
    println!("cargo:rustc-link-lib=static=displib");
    Ok(())
}

fn generate_dxgk_bindings() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::var("OUT_DIR")?;
    let out_path = std::path::Path::new(&out_dir).join("dxgk_bindings.rs");

    // `wdk_default` seeds the builder with the correct target triple, kernel-mode
    // defines (_KERNEL_MODE, _AMD64_, ...) and WDK/SDK include paths.
    let mut builder = bindgen::Builder::wdk_default(Config::from_env_auto()?)?
        .header_contents("helios-dxgk-input.h", DXGK_HEADER_CONTENTS)
        // Generate only the display surface; base types come from wdk_sys.
        .allowlist_type("DXGK.*")
        // The DOD init-data struct (NOT matched by DXGK.*); pulls in the DOD DDI
        // fn typedefs + their DXGKARG_* arg structs transitively.
        .allowlist_type("_?KMDDOD_INITIALIZATION_DATA")
        .allowlist_type("_?DRIVER_INITIALIZATION_DATA")
        .allowlist_type("D3DKMT_.*")
        .allowlist_type("D3DDDI_.*")
        .allowlist_type("D3DKMDT_.*")
        .allowlist_type("DXGK_.*")
        .allowlist_function("Dxgk.*")
        .allowlist_var("DXGK.*")
        .allowlist_var("D3DKMDT_.*")
        .allowlist_var("KMT_.*")
        // Re-export the base types from wdk_sys so blocklisted references resolve.
        // (The allow(...) lints are applied as an outer attribute on the `bindings`
        // module in src/dxgk.rs — an inner attribute here is illegal under include!.)
        .raw_line("pub use wdk_sys::*;");

    for ty in BLOCKLISTED_BASE_TYPES {
        builder = builder.blocklist_type(ty);
    }

    builder.generate()?.write_to_file(&out_path)?;

    Ok(())
}
