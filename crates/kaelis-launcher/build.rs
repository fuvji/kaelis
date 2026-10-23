fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=kaelis.rc");
    println!("cargo:rerun-if-changed=../../assets/kaelis.ico");
    embed_resource::compile("kaelis.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("embed the Windows icon resource");
}
