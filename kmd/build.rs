//! Build script for the Helios KMD (System-class KMDF function driver).
//!
//! All WDF / NTDDK bindings come from `wdk-sys`; WDF functions are invoked
//! through `wdk_sys::call_unsafe_wdf_function_binding!`, so there is no custom
//! bindgen here (the old `dispmprt.h`/`d3dkmddi.h` display bindgen and the
//! `displib.lib` link were deleted in the WDDM→System-class pivot — see
//! ARCH.md §10). This script only emits the WDK link configuration for the
//! driver binary.

use wdk_build::Config;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Config::from_env_auto()?.configure_binary_build()?;
    Ok(())
}
