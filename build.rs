fn main() {
    println!("cargo:rerun-if-changed=src/explorer_selection.cpp");
    println!("cargo:rerun-if-changed=src/file_dialog.cpp");

    cc::Build::new()
        .cpp(true)
        .file("src/explorer_selection.cpp")
        .file("src/file_dialog.cpp")
        .flag_if_supported("/std:c++17")
        .compile("explorer_selection");

    println!("cargo:rustc-link-lib=ole32");
    println!("cargo:rustc-link-lib=oleaut32");
    println!("cargo:rustc-link-lib=shell32");
    println!("cargo:rustc-link-lib=uuid");
    println!("cargo:rustc-link-lib=shlwapi");

    // UnRAR (крейт unrar) использует реестр и криптографию Windows,
    // но сам advapi32 не подключает.
    println!("cargo:rustc-link-lib=advapi32");
}
