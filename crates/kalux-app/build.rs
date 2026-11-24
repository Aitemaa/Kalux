fn main() {
    // Embeds a Windows application manifest: per-monitor-v2 DPI, long paths, UTF-8 code page,
    // Common Controls v6. No-op on non-Windows targets.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest::embed_manifest(
            embed_manifest::new_manifest("Kalux")
                .dpi_awareness(embed_manifest::manifest::DpiAwareness::PerMonitorV2)
                .long_path_aware(embed_manifest::manifest::Setting::Enabled)
                .active_code_page(embed_manifest::manifest::ActiveCodePage::Utf8),
        )
        .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
