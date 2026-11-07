// Compiles app.rc with the Windows SDK's rc.exe and links the .res into the exe.
// It's a build-time dependency only and adds zero runtime code.
fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=Inscriptio.ico");
    embed_resource::compile("app.rc", embed_resource::NONE).manifest_required().unwrap();
    // Our .rc already embeds a manifest. Stop link.exe from generating a second
    // one (duplicate RT_MANIFEST resources fail the link with CVT1100).
    println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
}
