use std::path::{Path, PathBuf};

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
    println!("cargo:rustc-link-lib=comctl32");

    // UnRAR использует реестр и криптографию Windows, но сам advapi32 не подключает.
    println!("cargo:rustc-link-lib=advapi32");

    embed_resources();
}

// Иконка, сведения о файле («Свойства → Подробно») и манифест — в exe.
fn embed_resources() {
    println!("cargo:rerun-if-changed=resources/uskko.ico");

    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();

    let mut numbers: Vec<u32> = version.split(['.', '-']).filter_map(|part| part.parse().ok()).take(3).collect();

    numbers.resize(4, 0);

    let comma = numbers.iter().map(u32::to_string).collect::<Vec<_>>().join(",");

    let dotted = numbers.iter().map(u32::to_string).collect::<Vec<_>>().join(".");

    let manifest = out.join("uskko.manifest");

    std::fs::write(&manifest, manifest_text(&dotted)).unwrap();

    let rc = out.join("uskko.rc");

    std::fs::write(&rc, rc_text(&root.join("resources").join("uskko.ico"), &manifest, &comma, &version)).unwrap();

    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
}

fn rc_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

fn rc_text(icon: &Path, manifest: &Path, comma: &str, version: &str) -> String {
    format!(
        r#"#pragma code_page(65001)

1 ICON "{icon}"

1 24 "{manifest}"

1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEFLAGSMASK 0x3f
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "041904b0"
        BEGIN
            VALUE "CompanyName", "oblako"
            VALUE "FileDescription", "uskko"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "uskko"
            VALUE "LegalCopyright", "oblako"
            VALUE "OriginalFilename", "uskko.exe"
            VALUE "ProductName", "uskko"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x419, 1200
    END
END
"#,
        icon = rc_path(icon),
        manifest = rc_path(manifest),
    )
}

// Обычные права (без запроса администратора), свой масштаб на каждом
// мониторе, длинные пути, современный вид системных окон и меню.
fn manifest_text(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="oblako.uskko" version="{version}"/>
  <description>uskko</description>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
"#
    )
}
