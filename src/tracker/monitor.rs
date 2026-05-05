use active_win_pos_rs::get_active_window;
use anyhow::Result;
use std::env;
#[cfg(target_os = "linux")]
use super::process_inspection;

#[derive(serde::Deserialize, Debug)]
struct WindowInfo {
    #[serde(default)]
    wm_class: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    focus: bool,
}

#[derive(serde::Deserialize, Debug)]
struct HyprlandWindow {
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
}

pub struct AppMonitor {
    use_wayland: bool,
    use_hyprland: bool,
}

impl Default for AppMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl AppMonitor {
    pub fn new() -> Self {
        // Log detected platform
        #[cfg(target_os = "macos")]
        log::info!("=== PLATFORM: macOS ===");

        #[cfg(target_os = "windows")]
        log::info!("=== PLATFORM: Windows ===");

        #[cfg(target_os = "linux")]
        log::info!("=== PLATFORM: Linux ===");

        let use_wayland = Self::is_wayland();
        let use_hyprland = Self::is_hyprland();

        // Platform-specific window tracking method
        #[cfg(target_os = "linux")]
        {
            if use_wayland {
                if use_hyprland {
                    log::info!("Session type: Wayland (Hyprland) - using hyprctl for window tracking");
                } else {
                    log::info!("Session type: Wayland - using D-Bus for window tracking");
                }
            } else {
                log::info!("Session type: X11 - using X11 APIs for window tracking");
            }
        }

        #[cfg(target_os = "macos")]
        log::info!("Using Cocoa/AppKit APIs for window tracking");

        #[cfg(target_os = "windows")]
        log::info!("Using Win32 APIs for window tracking");

        Self { use_wayland, use_hyprland }
    }

    pub fn uses_wayland(&self) -> bool {
        self.use_wayland
    }

    fn is_wayland() -> bool {
        #[cfg(target_os = "linux")]
        {
            if env::var("WAYLAND_DISPLAY").is_ok() {
                return true;
            }
            if env::var("XDG_SESSION_TYPE").map(|s| s == "wayland").unwrap_or(false) {
                return true;
            }
            // Fallback: check for Wayland socket when env vars aren't propagated
            if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
                let wayland_path = std::path::Path::new(&runtime_dir).join("wayland-0");
                if wayland_path.exists() {
                    log::info!("Wayland detected via socket (env vars not set)");
                    return true;
                }
            }
            false
        }

        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    fn is_hyprland() -> bool {
        env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok()
    }

    async fn get_active_window_hyprland() -> Result<(String, String)> {
        use std::process::Command;

        let output = Command::new("hyprctl")
            .args(["activewindow", "-j"])
            .output()?;

        if !output.status.success() {
            return Err(anyhow::anyhow!("hyprctl activewindow failed"));
        }

        let json_str = String::from_utf8_lossy(&output.stdout);
        let window: HyprlandWindow = serde_json::from_str(&json_str)?;

        if window.class.is_empty() {
            return Err(anyhow::anyhow!("No focused window (Hyprland)"));
        }

        Ok((window.class, window.title))
    }

    async fn get_active_window_wayland() -> Result<(String, String)> {
        let connection = zbus::Connection::session().await?;

        let response = connection.call_method(
            Some("org.gnome.Shell"),
            "/org/gnome/Shell/Extensions/Windows",
            Some("org.gnome.Shell.Extensions.Windows"),
            "List",
            &(),
        ).await?;

        // The response is a string directly, not a variant
        let json_str: String = response.body().deserialize()?;

        let windows: Vec<WindowInfo> = serde_json::from_str(&json_str)?;

        let focused_window = windows.iter()
            .find(|w| w.focus)
            .ok_or(anyhow::anyhow!("No focused window found"))?;

        Ok((focused_window.wm_class.clone(), focused_window.title.clone()))
    }

    // Get both app and window info in a single call
    pub async fn get_active_window_info_async(&self) -> Result<(String, Option<String>)> {
        // Try active-win-pos-rs first (works for X11 and some Wayland compositors)
        match get_active_window() {
            Ok(active_window) => {
                let app_name = self.fix_app_name(active_window.app_name.clone());
                log::info!("Detected app: {}", app_name);
                let mut window_title = if active_window.title.is_empty() || active_window.title == active_window.app_name {
                    None
                } else {
                    Some(active_window.title.clone())
                };

                // Enhance title for terminal apps if process_id is available
                if Self::is_terminal_app(&app_name) {
                    let current_title = window_title.as_deref().unwrap_or("");
                    log::info!("Main path terminal title before enhancement: '{}'", current_title);

                    // Extract directory from prompt if it looks like a shell prompt
                    let mut enhanced = if current_title.contains("@") && current_title.contains(": ") {
                        current_title.split(": ").last().unwrap_or(current_title).to_string()
                    } else {
                        current_title.to_string()
                    };
                    log::info!("Main path terminal title after directory extraction: '{}'", enhanced);

                    if active_window.process_id != 0 {
                        let pid = active_window.process_id;
                        enhanced = {
                             #[cfg(target_os = "linux")]
                             { if let Some(info) = Self::inspect_process_tree(pid) {
                                 let mut title = enhanced;
                                 if let Some(window) = info.tmux_window {
                                     title = format!("{} - {}", window, title);
                                 } else if info.has_tmux {
                                     let session = info.tmux_session.unwrap_or("session".to_string());
                                     title = format!("tmux: {} - {}", session, title);
                                 }
                                 if let Some(editor) = info.editor_info {
                                     title = format!("{} ({}) - {}", editor.filename, editor.filepath, title);
                                 }
                                 title
                             } else {
                                 enhanced
                             } }
                             #[cfg(not(target_os = "linux"))]
                             { enhanced }
                        };
                    }

                    if enhanced != current_title {
                        window_title = Some(enhanced);
                    }
                }
                return Ok((app_name, window_title));
            }
            Err(_) => {
                // Fallbacks based on platform/session type
                if self.use_wayland {
                    // Try Hyprland first if detected
                    if self.use_hyprland {
                        match Self::get_active_window_hyprland().await {
                            Ok((wm_class, title)) => {
                                let app_name = self.fix_app_name(wm_class);
                                log::info!("Detected app (Hyprland): {}", app_name);
                                return Ok((app_name, Some(title)));
                            }
                            Err(e) => {
                                log::debug!("Hyprland window detection failed: {}, trying GNOME D-Bus", e);
                            }
                        }
                    }

                    // Try GNOME extension for Wayland
                    if let Ok((wm_class, title)) = Self::get_active_window_wayland().await {
                        let app_name = self.fix_app_name(wm_class);
                        return Ok((app_name, Some(title)));
                    }
                } else {
                    // Try GNOME extension for Wayland
                    match Self::get_active_window_wayland().await {
                        Ok((wm_class, mut title)) => {
                            log::info!("Wayland fallback title: '{}'", title);
                            let app_name = self.fix_app_name(wm_class);
                            // Extract directory from prompt if it looks like a shell prompt
                            if Self::is_terminal_app(&app_name) && title.contains("@") && title.contains(": ")
                                && let Some(dir) = title.split(": ").last() {
                                    title = dir.to_string();
                                    log::info!("Wayland fallback title after extraction: '{}'", title);
                                }
                            return Ok((app_name, Some(title)));
                        }
                        Err(_) => {
                            // Try xdotool/xprop for X11 fallback on Linux
                            #[cfg(target_os = "linux")]
                            {
                                if let Ok((wm_class, title)) = Self::get_active_window_x11().await {
                                    let app_name = self.fix_app_name(wm_class);
                                    return Ok((app_name, Some(title)));
                                }
                            }
                        }
                    }
                }

                // macOS AppleScript fallback
                #[cfg(target_os = "macos")]
                {
                    if let Ok((app, title, pid)) = Self::get_active_window_info_macos().await {
                        let app_name = self.fix_app_name(app);
                        return Ok((app_name, if title.is_empty() { None } else { Some(title) }));
                    }
                }
            }
        }

        Err(anyhow::anyhow!("Failed to get window info"))
    }

    #[cfg(target_os = "linux")]
    fn inspect_process_tree(pid: u64) -> Option<process_inspection::ProcessInfo> {
        process_inspection::inspect_process_tree(pid)
    }

    #[cfg(target_os = "linux")]
    async fn get_active_window_x11() -> Result<(String, String)> {
        use std::process::Command;

        // Get focused window ID
        let window_id_output = Command::new("xdotool")
            .arg("getwindowfocus")
            .output()?;

        if !window_id_output.status.success() {
            return Err(anyhow::anyhow!("xdotool getwindowfocus failed"));
        }

        let wid = String::from_utf8_lossy(&window_id_output.stdout).trim().to_string();

        // Get window name
        let title_output = Command::new("xdotool")
            .arg("getwindowname")
            .arg(&wid)
            .output()?;

        if !title_output.status.success() {
            return Err(anyhow::anyhow!("xdotool getwindowname failed"));
        }

        let title = String::from_utf8_lossy(&title_output.stdout).trim().to_string();

        // Get WM_CLASS
        let wm_class_output = Command::new("xprop")
            .arg("-id")
            .arg(&wid)
            .arg("WM_CLASS")
            .output()?;

        if !wm_class_output.status.success() {
            return Err(anyhow::anyhow!("xprop WM_CLASS failed"));
        }

        let wm_class_str = String::from_utf8_lossy(&wm_class_output.stdout);
        let class = wm_class_str
            .lines()
            .find(|line| line.contains("WM_CLASS"))
            .and_then(|line| line.split('"').nth(1))
            .unwrap_or("")
            .to_string();

        Ok((class, title))
    }

    #[cfg(target_os = "macos")]
    async fn get_active_window_info_macos() -> Result<(String, String, u64)> {
        use std::process::Command;

        // Use System Events to get window info - works reliably for all apps
        let script = r#"
            tell application "System Events"
                set frontApp to first application process whose frontmost is true
                set appName to name of frontApp
                set pid to unix id of frontApp
                try
                    set windowTitle to name of front window of frontApp
                    return appName & "|" & windowTitle & "|" & pid
                on error
                    return appName & "|" & "|" & pid
                end try
            end tell
        "#;

        let output = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()?;

        if output.status.success() {
            let result = String::from_utf8_lossy(&output.stdout).trim().to_string();

            let parts: Vec<&str> = result.split('|').collect();
            if parts.len() >= 3 {
                let app_name = parts[0].trim().to_string();
                let window_title = parts[1].trim().to_string();
                if let Ok(pid) = parts[2].trim().parse::<u64>() {
                    log::debug!("AppleScript returned: app='{}', title='{}', pid={}", app_name, window_title, pid);

                    if !app_name.is_empty() {
                        return Ok((app_name, window_title, pid));
                    }
                }
            }
        }

        Err(anyhow::anyhow!("Failed to get window info via AppleScript"))
    }

    fn is_terminal_app(app: &str) -> bool {
        let app_lower = app.to_lowercase();
        app_lower.contains("terminal") ||
        app_lower.contains("iterm") ||
        app_lower.contains("alacritty") ||
        app_lower.contains("cmd") ||
        app_lower.contains("powershell") ||
        app_lower.contains("wt") ||
        app_lower == "hyper" ||
        app_lower == "tabby" ||
        app_lower == "warp" ||
        app_lower.contains("kitty") ||
        app_lower.contains("konsole") ||
        app_lower.contains("wezterm")
    }

    // Get both app and window info in a single call (more efficient for macOS AppleScript)
    pub async fn get_active_app_async(&self) -> Result<String> {
        if self.use_wayland {
            // Try Hyprland first if detected
            if self.use_hyprland {
                match Self::get_active_window_hyprland().await {
                    Ok((wm_class, _title)) => {
                        log::info!("Detected active app (Hyprland): {}", wm_class);
                        return Ok(self.fix_app_name(wm_class));
                    }
                    Err(e) => {
                        log::debug!("Hyprland window detection failed: {}, trying GNOME D-Bus", e);
                    }
                }
            }

            // Try GNOME Shell extension
            match Self::get_active_window_wayland().await {
                Ok((wm_class, _title)) => {
                    log::info!("Detected active app (Wayland): {}", wm_class);
                    Ok(self.fix_app_name(wm_class))
                }
                Err(e) => {
                    let error_msg = if self.use_hyprland {
                        format!(
                            "Wayland window detection failed: {}. \
                            hyprctl and GNOME D-Bus both unavailable.",
                            e
                        )
                    } else {
                        format!(
                            "Wayland window detection failed: {}. \
                            Make sure the 'Window Calls' GNOME extension is installed and enabled. \
                            Install from: https://extensions.gnome.org/extension/4724/window-calls/",
                            e
                        )
                    };
                    log::warn!("{}", error_msg);
                    Err(anyhow::anyhow!(error_msg))
                }
            }
        } else {
            // Use platform-specific native APIs
            match get_active_window() {
                Ok(active_window) => {
                    // Platform-specific debug logging
                    #[cfg(target_os = "macos")]
                    log::debug!("[macOS] Raw window - app: '{}', title: '{}', path: {:?}, position: {:?}",
                               active_window.app_name,
                               active_window.title,
                               active_window.process_path,
                               active_window.position);

                    #[cfg(target_os = "windows")]
                    log::debug!("[Windows] Raw window - app: '{}', title: '{}', path: {:?}, position: {:?}",
                               active_window.app_name,
                               active_window.title,
                               active_window.process_path,
                               active_window.position);

                    #[cfg(target_os = "linux")]
                    log::debug!("[Linux/X11] Raw window - app: '{}', title: '{}'",
                               active_window.app_name,
                               active_window.title);

                    let original_name = active_window.app_name.clone();
                    let fixed_name = self.fix_app_name(original_name.clone());

                    if original_name != fixed_name {
                        log::info!("App detected: '{}' (normalized from '{}')", fixed_name, original_name);
                    } else {
                        log::info!("App detected: '{}'", fixed_name);
                    }

                    Ok(fixed_name)
                }
                Err(e) => {
                    log::error!("Failed to get active window: {:?}", e);

                    // On macOS, try AppleScript as fallback
                    #[cfg(target_os = "macos")]
                    {
                        log::info!("active-win-pos-rs failed, trying AppleScript fallback...");
                        match Self::get_active_app_macos().await {
                            Ok(app_name) => {
                                log::info!("AppleScript successfully detected app: '{}'", app_name);
                                return Ok(self.fix_app_name(app_name));
                            }
                            Err(applescript_err) => {
                                log::error!("AppleScript fallback also failed: {}", applescript_err);
                            }
                        }
                    }

                    let error_msg = "Failed to get active window";
                    log::warn!("{}", error_msg);
                    Err(anyhow::anyhow!(error_msg))
                }
            }
        }
    }

    pub async fn get_active_window_name_async(&self) -> Result<String> {
        if self.use_wayland {
            // Try Hyprland first if detected
            if self.use_hyprland {
                match Self::get_active_window_hyprland().await {
                    Ok((_wm_class, mut title)) => {
                        if title.contains("@") && title.contains(": ")
                            && let Some(dir) = title.rsplit(": ").next() {
                                title = dir.to_string();
                            }
                        return Ok(title);
                    }
                    Err(e) => {
                        log::debug!("Hyprland window title failed: {}, trying GNOME D-Bus", e);
                    }
                }
            }

            // Try GNOME Shell extension
            match Self::get_active_window_wayland().await {
                Ok((_wm_class, mut title)) => {
                    // Extract directory from prompt if it looks like a shell prompt
                    if title.contains("@") && title.contains(": ")
                        && let Some(dir) = title.rsplit(": ").next() {
                            title = dir.to_string();
                        }
                    Ok(title)
                },
                Err(_) => {
                    log::warn!("Failed to get active window title (Wayland).");
                    Ok("Unknown Window".to_string())
                }
            }
        } else {
            // Use platform-specific native APIs
            match get_active_window() {
                Ok(active_window) => {
                    let mut title = active_window.title.clone();
                    #[allow(unused_variables)]
                    let app_name = active_window.app_name.clone();

                    #[cfg(target_os = "macos")]
                    {
                        if title == app_name || title.is_empty() || title == "Unknown" {
                            log::debug!("Generic title detected for '{}', trying AppleScript fallback", app_name);
                            if let Ok(detailed_title) = Self::get_window_title_macos(&app_name).await {
                                if !detailed_title.is_empty() && detailed_title != app_name {
                                    log::info!("AppleScript retrieved title for {}: '{}'", app_name, detailed_title);
                                    return Ok(detailed_title);
                                }
                            }
                        }
                    }

                    // On Windows, if we get a generic title, try PowerShell fallback
                    #[cfg(target_os = "windows")]
                    {
                        if title == app_name || title.is_empty() || title == "Unknown" {
                            log::debug!("Generic title detected for '{}', trying PowerShell fallback", app_name);
                            if let Ok(detailed_title) = Self::get_window_title_windows(&app_name).await {
                                if !detailed_title.is_empty() && detailed_title != app_name {
                                    log::info!("PowerShell retrieved title for {}: '{}'", app_name, detailed_title);
                                    return Ok(detailed_title);
                                }
                            }
                        }
                    }

                    // On Linux, enhance title with process inspection
                    #[cfg(target_os = "linux")]
                    {
                        // First, extract directory from prompt if it looks like a shell prompt
                        if title.contains("@") && title.contains(": ")
                            && let Some(dir) = title.rsplit(": ").next() {
                                title = dir.to_string();
                            }
                        let pid = active_window.process_id;
                        if pid != 0
                             && let Some(info) = process_inspection::inspect_process_tree(pid) {
                                 if let Some(window) = info.tmux_window {
                                     title = format!("{} - {}", window, title);
                                 } else if info.has_tmux {
                                     let session = info.tmux_session.unwrap_or("session".to_string());
                                     title = format!("tmux: {} - {}", session, title);
                                 }
                                 if let Some(editor) = info.editor_info {
                                     title = format!("{} ({}) - {}", editor.filename, editor.filepath, title);
                                 }
                             }
                    }

                    Ok(title)
                }
                Err(_) => {
                    log::warn!("Failed to get active window title.");
                    Ok("Unknown Window".to_string())
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    async fn get_active_app_macos() -> Result<String> {
        use std::process::Command;

        // AppleScript to get the frontmost application name
        let script = r#"
            tell application "System Events"
                set frontApp to first application process whose frontmost is true
                return name of frontApp
            end tell
        "#;

        let output = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()?;

        if output.status.success() {
            let app_name = String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_string();

            if !app_name.is_empty() {
                log::debug!("AppleScript returned app: '{}'", app_name);
                return Ok(app_name);
            }
        } else {
            let error = String::from_utf8_lossy(&output.stderr);
            log::warn!("AppleScript failed to get app: {}", error);
        }

        Err(anyhow::anyhow!("Failed to get active app via AppleScript"))
    }

    #[cfg(target_os = "macos")]
    async fn get_window_title_macos(_app_name: &str) -> Result<String> {
        use std::process::Command;

        // Use System Events to get window title - works for all apps including browsers
        let script = r#"
            tell application "System Events"
                set frontApp to first application process whose frontmost is true
                tell frontApp
                    try
                        set windowTitle to name of front window
                        return windowTitle
                    on error
                        return ""
                    end try
                end tell
            end tell
        "#;

        let output = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()?;

        if output.status.success() {
            let title = String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_string();

            if !title.is_empty() {
                log::debug!("AppleScript returned title: '{}'", title);
                return Ok(title);
            }
        } else {
            let error = String::from_utf8_lossy(&output.stderr);
            log::warn!("AppleScript failed: {}", error);
        }

        Err(anyhow::anyhow!("Failed to get window title via AppleScript"))
    }

    #[cfg(target_os = "windows")]
    async fn get_window_title_windows(app_name: &str) -> Result<String> {
        use std::process::Command;

        // PowerShell script to get the window title of the active window
        let script = format!(
            r#"
            Add-Type @"
                using System;
                using System.Runtime.InteropServices;
                using System.Text;
                public class Win32 {{
                    [DllImport("user32.dll")]
                    public static extern IntPtr GetForegroundWindow();
                    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
                    public static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int count);
                }}
"@
            $hwnd = [Win32]::GetForegroundWindow()
            $title = New-Object System.Text.StringBuilder 256
            [Win32]::GetWindowText($hwnd, $title, 256) | Out-Null
            $title.ToString()
            "#
        );

        let output = Command::new("powershell")
            .arg("-NoProfile")
            .arg("-Command")
            .arg(&script)
            .output()?;

        if output.status.success() {
            let title = String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_string();

            if !title.is_empty() {
                log::debug!("PowerShell returned: '{}'", title);
                return Ok(title);
            }
        } else {
            let error = String::from_utf8_lossy(&output.stderr);
            log::warn!("PowerShell failed: {}", error);
        }

        Err(anyhow::anyhow!("Failed to get window title via PowerShell"))
    }

    fn fix_app_name(&self, app: String) -> String {
        let app_lower = app.to_lowercase();

        // Linux-specific: Handle Wayland wm_class format (e.g., "org.gnome.Nautilus", "firefox_firefox")
        #[cfg(target_os = "linux")]
        let normalized = {
            if app_lower.contains('.') {
                app_lower.split('.').next_back().unwrap_or(&app_lower).to_string()
            } else if app_lower.contains('_') {
                app_lower.split('_').next().unwrap_or(&app_lower).to_string()
            } else {
                app_lower.clone()
            }
        };

        // macOS/Windows: No normalization, use lowercase as-is
        #[cfg(not(target_os = "linux"))]
        let normalized = app_lower.clone();

        // Cross-platform app detection (works on all platforms)
        if normalized.contains("chrome") || normalized.contains("chromium") || normalized.contains("google-chrome") {
            return "chrome".to_string();
        } else if normalized.contains("firefox") {
            return "firefox".to_string();
        } else if normalized.contains("code") || normalized.contains("vscode") || normalized.contains("vscodium") {
            return "vscode".to_string();
        } else if normalized.contains("slack") {
            return "slack".to_string();
        } else if normalized.contains("discord") {
            return "discord".to_string();
        } else if normalized.contains("telegram") {
            return "telegram".to_string();
        } else if normalized.contains("zoom") {
            return "zoom".to_string();
        } else if normalized.contains("teams") {
            return "teams".to_string();
        } else if normalized.contains("skype") {
            return "skype".to_string();
        } else if normalized.contains("spotify") {
            return "spotify".to_string();
        } else if normalized.contains("vlc") {
            return "vlc".to_string();
        }

        // Linux-ONLY app detection (GNOME, KDE-specific apps)
        #[cfg(target_os = "linux")]
        {
            if normalized.contains("gnome-terminal") || normalized.contains("terminal") {
                return "gnome-terminal".to_string();
            } else if normalized == "soffice" || app_lower == "soffice.bin" {
                return "libreoffice".to_string();
            } else if normalized.contains("nautilus") || normalized.contains("files") || normalized.contains("thunar") || normalized.contains("dolphin") || normalized.contains("nemo") {
                return "file-manager".to_string();
            } else if normalized.contains("alacritty") || normalized.contains("kitty") || normalized.contains("wezterm") || normalized.contains("konsole") {
                return "terminal".to_string();
            } else if normalized.contains("vim") || normalized.contains("nvim") || normalized.contains("emacs") || normalized.contains("nano") || normalized.contains("gedit") || normalized.contains("kate") || normalized.contains("mousepad") {
                return "editor".to_string();
            } else if normalized.contains("rhythmbox") || normalized.contains("audacious") || normalized.contains("clementine") {
                return "media".to_string();
            } else if normalized.contains("thunderbird") || normalized.contains("evolution") || normalized.contains("geary") {
                return "email".to_string();
            } else if normalized.contains("signal") || normalized.contains("element") || normalized.contains("matrix") {
                return "chat".to_string();
            }
        }

        // Return original or normalized name
        if normalized.len() < app.len() && !normalized.is_empty() {
            normalized
        } else {
            app
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_active_app_async() {
        let monitor = AppMonitor::new();
        // Note: This test may fail if no active window is available
        let app = monitor.get_active_app_async().await.unwrap_or_else(|_| "test".to_string());
        assert!(!app.is_empty());
    }

    #[tokio::test]
    async fn test_get_active_window_name_async() {
        let monitor = AppMonitor::new();
        // Note: This test may fail if no active window is available
        let window_name = monitor.get_active_window_name_async().await.unwrap_or_else(|_| "test".to_string());
        assert!(!window_name.is_empty());
    }

    #[test]
    fn test_hyprland_window_deserialize_full() {
        let json = r#"{
            "class": "firefox",
            "title": "GitHub - Mozilla Firefox",
            "address": "0x123",
            "workspace": {"id": 1, "name": "1"},
            "pid": 12345,
            "focused": true
        }"#;
        let window: HyprlandWindow = serde_json::from_str(json).unwrap();
        assert_eq!(window.class, "firefox");
        assert_eq!(window.title, "GitHub - Mozilla Firefox");
    }

    #[test]
    fn test_hyprland_window_deserialize_empty() {
        // hyprctl returns {} when no window is focused
        let json = "{}";
        let window: HyprlandWindow = serde_json::from_str(json).unwrap();
        assert_eq!(window.class, "");
        assert_eq!(window.title, "");
    }

    #[test]
    fn test_hyprland_window_deserialize_minimal() {
        let json = r#"{"class": "kitty", "title": "~"}"#;
        let window: HyprlandWindow = serde_json::from_str(json).unwrap();
        assert_eq!(window.class, "kitty");
        assert_eq!(window.title, "~");
    }

    #[test]
    fn test_is_hyprland_with_env() {
        let original = env::var("HYPRLAND_INSTANCE_SIGNATURE").ok();
        // SAFETY: test-only env manipulation, tests run single-threaded with -- --test-threads=1
        unsafe { env::set_var("HYPRLAND_INSTANCE_SIGNATURE", "test_signature"); }
        assert!(AppMonitor::is_hyprland());
        unsafe {
            if let Some(val) = original {
                env::set_var("HYPRLAND_INSTANCE_SIGNATURE", val);
            } else {
                env::remove_var("HYPRLAND_INSTANCE_SIGNATURE");
            }
        }
    }

    #[test]
    fn test_is_hyprland_without_env() {
        let original = env::var("HYPRLAND_INSTANCE_SIGNATURE").ok();
        // SAFETY: test-only env manipulation
        unsafe { env::remove_var("HYPRLAND_INSTANCE_SIGNATURE"); }
        assert!(!AppMonitor::is_hyprland());
        unsafe {
            if let Some(val) = original {
                env::set_var("HYPRLAND_INSTANCE_SIGNATURE", val);
            }
        }
    }

    #[test]
    fn test_fix_app_name_hyprland_classes() {
        let monitor = AppMonitor { use_wayland: true, use_hyprland: true };
        // Hyprland class names are typically lowercase app identifiers
        assert_eq!(monitor.fix_app_name("firefox".to_string()), "firefox");
        assert_eq!(monitor.fix_app_name("chrome".to_string()), "chrome");
        assert_eq!(monitor.fix_app_name("Slack".to_string()), "slack");
        assert_eq!(monitor.fix_app_name("discord".to_string()), "discord");
        assert_eq!(monitor.fix_app_name("spotify".to_string()), "spotify");
    }

    #[test]
    fn test_is_wayland_via_socket_fallback() {
        // When env vars are missing, is_wayland should detect via socket file
        let original_wayland = env::var("WAYLAND_DISPLAY").ok();
        let original_session = env::var("XDG_SESSION_TYPE").ok();
        let runtime_dir = env::var("XDG_RUNTIME_DIR").ok();

        // SAFETY: test-only env manipulation
        unsafe {
            env::remove_var("WAYLAND_DISPLAY");
            env::remove_var("XDG_SESSION_TYPE");
        }

        if let Some(ref dir) = runtime_dir {
            let wayland_socket = std::path::Path::new(dir).join("wayland-0");
            if wayland_socket.exists() {
                // On a Wayland system with socket present, should detect Wayland
                assert!(AppMonitor::is_wayland(), "Should detect Wayland via socket fallback");
            } else {
                // No socket, no env vars — should not detect Wayland
                assert!(!AppMonitor::is_wayland(), "Should not detect Wayland without socket or env vars");
            }
        }

        // Restore
        unsafe {
            if let Some(val) = original_wayland {
                env::set_var("WAYLAND_DISPLAY", val);
            }
            if let Some(val) = original_session {
                env::set_var("XDG_SESSION_TYPE", val);
            }
        }
    }

    #[test]
    fn test_is_wayland_via_env_var() {
        let original = env::var("WAYLAND_DISPLAY").ok();
        // SAFETY: test-only env manipulation
        unsafe { env::set_var("WAYLAND_DISPLAY", "wayland-0"); }
        assert!(AppMonitor::is_wayland());
        unsafe {
            if let Some(val) = original {
                env::set_var("WAYLAND_DISPLAY", val);
            } else {
                env::remove_var("WAYLAND_DISPLAY");
            }
        }
    }

    #[test]
    fn test_is_wayland_via_session_type() {
        let original_wayland = env::var("WAYLAND_DISPLAY").ok();
        let original_session = env::var("XDG_SESSION_TYPE").ok();
        // SAFETY: test-only env manipulation
        unsafe {
            env::remove_var("WAYLAND_DISPLAY");
            env::set_var("XDG_SESSION_TYPE", "wayland");
        }
        assert!(AppMonitor::is_wayland());
        unsafe {
            if let Some(val) = original_wayland {
                env::set_var("WAYLAND_DISPLAY", val);
            }
            if let Some(val) = original_session {
                env::set_var("XDG_SESSION_TYPE", val);
            } else {
                env::remove_var("XDG_SESSION_TYPE");
            }
        }
    }
}
