use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=FASTDISCORD_ZIP");
    println!("cargo:rerun-if-env-changed=FASTDISCORD_BUILD");
    // The exe's icon. Absolute path: the resource compiler runs on a
    // preprocessed copy in OUT_DIR, where relative paths break.
    let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/icon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let rc = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("app.rc");
    let icon = icon.display().to_string().replace('\\', "/");
    std::fs::write(&rc, format!("1 ICON \"{icon}\"\n")).unwrap();
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
