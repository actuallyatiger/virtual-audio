//! Embeds a Windows application manifest. Without one, Windows treats the exe
//! as an old program: the Program Compatibility Assistant can offer to run it
//! "using compatibility settings" after it exits (e.g. a copy handing over
//! to another).
use embed_manifest::manifest::{ExecutionLevel, SupportedOS};
use embed_manifest::{embed_manifest, empty_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // Only what's needed: no DPI, code page or common controls changes.
        let manifest = empty_manifest()
            .name("VirtualAudioMixer")
            .supported_os(SupportedOS::Windows7..=SupportedOS::Windows10)
            .requested_execution_level(ExecutionLevel::AsInvoker);
        embed_manifest(manifest).expect("embedding the Windows manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
