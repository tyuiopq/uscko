fn main() {
    println!("cargo:rerun-if-changed=src/explorer_selection.cpp");

    cc::Build::new()
        .cpp(true)
        .file("src/explorer_selection.cpp")
        .flag_if_supported("/std:c++17")
        .compile("explorer_selection");

    println!("cargo:rustc-link-lib=ole32");
    println!("cargo:rustc-link-lib=oleaut32");
    println!("cargo:rustc-link-lib=shell32");
    println!("cargo:rustc-link-lib=uuid");
}
