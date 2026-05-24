use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const LABEL: &str = "com.invisionnaire.dictaflow.login";

/// Mac App Store builds are sandboxed and cannot install LaunchAgents.
pub fn launch_at_login_supported() -> bool {
    #[cfg(target_os = "macos")]
    {
        !is_mac_app_store_build()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

#[cfg(target_os = "macos")]
fn is_mac_app_store_build() -> bool {
    app_bundle_path(&std::env::current_exe().unwrap_or_default())
        .is_some_and(|app| app.join("Contents/embedded.provisionprofile").exists())
}

pub fn is_enabled() -> bool {
    if !launch_at_login_supported() {
        return false;
    }
    launch_agent_path().is_some_and(|path| path.exists() && plist_points_to_current_app(&path))
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    if enabled && !launch_at_login_supported() {
        return Err(anyhow!(
            "Launch at login is not available in the Mac App Store build (App Sandbox)."
        ));
    }
    if enabled {
        enable()
    } else {
        disable()
    }
}

/// Align the LaunchAgent with persisted settings and the current app location.
pub fn reconcile(settings_enabled: bool) -> Result<()> {
    if !launch_at_login_supported() {
        if settings_enabled {
            tracing::info!("launch at login: disabled in Mac App Store build; clearing setting");
            let _ = disable();
            let _ = crate::settings::Settings::update(|saved| saved.launch_at_login = false);
        }
        return Ok(());
    }
    let path = launch_agent_path().ok_or_else(|| anyhow!("no LaunchAgents directory"))?;
    let plist_exists = path.exists();
    let plist_valid = plist_exists && plist_points_to_current_app(&path);

    match (settings_enabled, plist_valid) {
        (true, false) => {
            tracing::info!("launch at login: repairing LaunchAgent");
            enable()
        }
        (false, true) => {
            tracing::info!("launch at login: removing stale LaunchAgent");
            disable()
        }
        _ => Ok(()),
    }
}

fn enable() -> Result<()> {
    let path = launch_agent_path().ok_or_else(|| anyhow!("no LaunchAgents directory"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let args = program_arguments()?;
    fs::write(&path, plist(&args)).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn disable() -> Result<()> {
    if let Some(path) = launch_agent_path() {
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
    }
    Ok(())
}

fn launch_agent_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| {
        home.join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    })
}

fn program_arguments() -> Result<Vec<String>> {
    let exe = std::env::current_exe().context("current executable")?;
    if let Some(app) = app_bundle_path(&exe) {
        return Ok(vec!["/usr/bin/open".to_string(), app.display().to_string()]);
    }
    Ok(vec![exe.display().to_string()])
}

fn app_bundle_path(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|p| p.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
}

fn plist_points_to_current_app(path: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(expected) = program_arguments() else {
        return false;
    };
    expected.iter().all(|arg| raw.contains(arg))
}

fn plist(args: &[String]) -> String {
    let args_xml = args
        .iter()
        .map(|arg| format!("        <string>{}</string>", xml_escape(arg)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
{}
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
        LABEL, args_xml
    )
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escape_escapes_special_chars() {
        assert_eq!(xml_escape("a&b<c>"), "a&amp;b&lt;c&gt;");
    }

    #[test]
    fn launch_at_login_supported_in_dev_build() {
        // Dev/direct-install builds do not embed an App Store provisioning profile.
        assert!(launch_at_login_supported());
    }
}
