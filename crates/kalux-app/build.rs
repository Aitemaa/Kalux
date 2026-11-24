//! Windows resources for `kalux.exe`: app icon (resource ID 1), version info (`FileDescription` /
//! `ProductName`), and application manifest (PerMonitorV2 DPI awareness, long paths, UTF-8 code
//! page, Common-Controls v6, `asInvoker`, Windows 10/11).
//!
//! * Windows host (CI, releases): single resource script via `winresource`.
//! * Non-Windows host targeting Windows: pure-Rust manifest via `embed-manifest`.

const ICON: &str = "../../assets/icon.ico";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ICON}");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed();
    }
}

#[cfg(windows)]
fn embed() {
    const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Kalux" version="1.0.0.0"/>
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
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2,PerMonitor</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
    </windowsSettings>
  </application>
</assembly>
"#;
    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id(ICON, "1")
        .set_manifest(MANIFEST)
        // Task Manager, "Open with", and Start show FileDescription/ProductName rather than the crate description.
        .set("FileDescription", "Kalux")
        .set("ProductName", "Kalux");
    res.compile().expect("unable to compile Windows resources");
}

#[cfg(not(windows))]
fn embed() {
    embed_manifest::embed_manifest(
        embed_manifest::new_manifest("Kalux")
            .dpi_awareness(embed_manifest::manifest::DpiAwareness::PerMonitorV2)
            .long_path_aware(embed_manifest::manifest::Setting::Enabled)
            .active_code_page(embed_manifest::manifest::ActiveCodePage::Utf8),
    )
    .expect("unable to embed manifest");
}
