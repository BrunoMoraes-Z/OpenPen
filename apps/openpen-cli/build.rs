//! Embeds the app icon and version info into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/icon/openpen.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest.join("../../assets/icon/openpen.ico");
    let icon = std::fs::canonicalize(&icon).expect("assets/icon/openpen.ico is missing");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut nums: Vec<u32> = version
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    nums.resize(4, 0);
    let commas = nums
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    // rc.exe wants forward slashes; strip the verbatim prefix canonicalize adds.
    let icon = icon.display().to_string();
    let icon = icon.trim_start_matches(r"\\?\").replace('\\', "/");
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "OpenPen"
      VALUE "FileDescription", "OpenPen CLI"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "openpen-cli"
      VALUE "LegalCopyright", "MIT License"
      VALUE "OriginalFilename", "openpen-cli.exe"
      VALUE "ProductName", "OpenPen"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("openpen-cli.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
