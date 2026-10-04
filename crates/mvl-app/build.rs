//! Build script: compiles the Slint UI (embeds the bundled fonts and any
//! images referenced from .slint files) and generates the Rust bindings.

fn main() {
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/appwindow.slint", config)
        .expect("Slint UI compilation failed");
}
