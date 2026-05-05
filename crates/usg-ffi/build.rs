// Set the dylib's install_name to a relocatable @rpath form so the artifact
// doesn't leak the build machine's absolute target path. Unity's [DllImport]
// loads via dlopen so install_name isn't strictly required for resolution,
// but adhoc-signed dylibs with absolute build paths look broken in `otool -D`
// and some downstream linkers complain.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libUnitySolutionGenerator.dylib"
        );
    }
}
