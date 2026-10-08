// Установщику нужен готовый uskko.exe: он вшивается внутрь.
// Порядок сборки — build-installer.ps1 (сначала uskko, потом установщик).

use std::path::{Path, PathBuf};

fn main() {
    // Общие файлы программы узнают, что собираются для установщика.
    println!("cargo:rustc-check-cfg=cfg(uskko_installer)");
    println!("cargo:rustc-cfg=uskko_installer");

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    let root = manifest_dir.parent().unwrap().to_path_buf();

    let payload = std::env::var_os("USKKO_PAYLOAD")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target").join("release").join("uskko.exe"));

    println!("cargo:rerun-if-env-changed=USKKO_PAYLOAD");
    println!("cargo:rerun-if-changed={}", payload.display());

    if !payload.is_file() {
        panic!(
            "Нет {}: сначала соберите программу (cargo build --release -p uskko) или запустите build-installer.ps1",
            payload.display()
        );
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    std::fs::copy(&payload, out.join("payload.exe")).unwrap();

    embed_resources(&root, &out);
}

fn rc_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

fn embed_resources(root: &Path, out: &Path) {
    let icon = root.join("resources").join("uskko.ico");

    println!("cargo:rerun-if-changed={}", icon.display());

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();

    let mut numbers: Vec<u32> = version.split(['.', '-']).filter_map(|part| part.parse().ok()).take(3).collect();

    numbers.resize(4, 0);

    let comma = numbers.iter().map(u32::to_string).collect::<Vec<_>>().join(",");

    let dotted = numbers.iter().map(u32::to_string).collect::<Vec<_>>().join(".");

    let manifest = out.join("setup.manifest");

    std::fs::write(
        &manifest,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="oblako.uskko.setup" version="{dotted}"/>
  <description>uskko setup</description>
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
    </windowsSettings>
  </application>
</assembly>
"#
        ),
    )
    .unwrap();

    let rc = out.join("setup.rc");

    std::fs::write(
        &rc,
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
            VALUE "FileDescription", "Установка uskko"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "uskko-setup"
            VALUE "LegalCopyright", "oblako"
            VALUE "OriginalFilename", "uskko-setup.exe"
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
            icon = rc_path(&icon),
            manifest = rc_path(&manifest),
        ),
    )
    .unwrap();

    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
}
