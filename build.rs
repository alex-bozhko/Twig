fn main() {
    embed_resource::compile("res/twig.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
    println!("cargo:rerun-if-changed=res/twig.rc");
    println!("cargo:rerun-if-changed=res/twig.manifest");
    println!("cargo:rerun-if-changed=res/AppIcon.ico");
}
