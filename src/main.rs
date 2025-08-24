use tauri::{State, Manager, Emitter, tray::TrayIconBuilder, menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder, CheckMenuItemBuilder}};
use image;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use screenshots::Screen;
use base64::{Engine as _, engine::general_purpose};
use arboard::Clipboard;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub api_base_url: String,
    pub api_key: String,
    pub model: String,
    pub prompt: String,
    pub hotkey: String,
    pub sound_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_base_url: "http://210.126.8.197:11434/v1".to_string(),
            api_key: "".to_string(),
            model: "".to_string(),
            prompt: "识别公式和文字，返回使用pandoc语法的markdown排版内容。公式请用katex语法包裹，文字内容不要丢失。只返回内容不需要其他解释。".to_string(),
            hotkey: "cmd+shift+m".to_string(),
            sound_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
}

#[derive(Clone)]
pub struct AppState {
    config: Arc<Mutex<Config>>,
    current_hotkey: Arc<Mutex<Option<String>>>,
    http_client: reqwest::Client,
    loaded_models: Arc<Mutex<Vec<String>>>,
    // Store references to CheckMenuItems for dynamic updates
    model_check_items: Arc<Mutex<std::collections::HashMap<String, tauri::menu::CheckMenuItem<tauri::Wry>>>>,
    // Store reference to the model submenu for title updates
    model_submenu: Arc<Mutex<Option<tauri::menu::Submenu<tauri::Wry>>>>,
}

impl AppState {
    fn new() -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(10)
            .http2_keep_alive_interval(std::time::Duration::from_secs(30))
            .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
            .http2_keep_alive_while_idle(true)
            .build()
            .expect("Failed to create HTTP client");

        // Load config from file or use default
        let config = Self::load_config().unwrap_or_else(|e| {
            println!("Failed to load config: {}, using default", e);
            Config::default()
        });

        Self {
            config: Arc::new(Mutex::new(config)),
            current_hotkey: Arc::new(Mutex::new(None)),
            http_client,
            loaded_models: Arc::new(Mutex::new({
                // Try to load cached models on startup
                Self::load_cached_models().unwrap_or_else(|e| {
                    println!("Failed to load cached models: {}, starting with empty list", e);
                    Vec::new()
                })
            })),
            model_check_items: Arc::new(Mutex::new(std::collections::HashMap::new())),
            model_submenu: Arc::new(Mutex::new(None)),
        }
    }

    fn get_config_path() -> Result<PathBuf, String> {
        let home_dir = dirs_next::home_dir().ok_or("Failed to get home directory")?;
        let config_dir = home_dir.join(".mathimage");
        
        // Create config directory if it doesn't exist
        fs::create_dir_all(&config_dir)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
        
        Ok(config_dir.join("config.json"))
    }

    fn load_config() -> Result<Config, String> {
        let config_path = Self::get_config_path()?;
        
        if !config_path.exists() {
            return Ok(Config::default());
        }

        let config_data = fs::read_to_string(&config_path)
            .map_err(|e| format!("Failed to read config file: {}", e))?;
        
        let config: Config = serde_json::from_str(&config_data)
            .map_err(|e| format!("Failed to parse config file: {}", e))?;
        
        Ok(config)
    }

    fn save_loaded_models(models: &[String]) -> Result<(), String> {
        let config_dir = Self::get_config_path()?.parent().unwrap().to_path_buf();
        let models_file = config_dir.join("models.json");
        
        let json = serde_json::to_string_pretty(models)
            .map_err(|e| format!("Failed to serialize models: {}", e))?;
        
        std::fs::write(&models_file, json)
            .map_err(|e| format!("Failed to write models file: {}", e))?;
        
        println!("Saved {} models to cache", models.len());
        Ok(())
    }
    
    fn load_cached_models() -> Result<Vec<String>, String> {
        let config_dir = Self::get_config_path()?.parent().unwrap().to_path_buf();
        let models_file = config_dir.join("models.json");
        
        if !models_file.exists() {
            return Ok(Vec::new());
        }
        
        let content = std::fs::read_to_string(&models_file)
            .map_err(|e| format!("Failed to read models file: {}", e))?;
        
        let models: Vec<String> = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse models file: {}", e))?;
        
        println!("Loaded {} models from cache", models.len());
        Ok(models)
    }

    fn save_config(config: &Config) -> Result<(), String> {
        let config_path = Self::get_config_path()?;
        
        let config_data = serde_json::to_string_pretty(config)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        
        fs::write(&config_path, config_data)
            .map_err(|e| format!("Failed to write config file: {}", e))?;
        
        println!("Config saved to: {:?}", config_path);
        Ok(())
    }
}

// Sanitize error messages to avoid information leakage
fn sanitize_error(error: &str) -> String {
    if error.contains("Connection refused") || error.contains("timeout") {
        "Network connection failed".to_string()
    } else if error.contains("401") || error.contains("403") {
        "Authentication failed".to_string()
    } else if error.contains("404") {
        "Service not found".to_string()
    } else if error.contains("500") || error.contains("502") || error.contains("503") {
        "Server error".to_string()
    } else {
        "Request failed".to_string()
    }
}



#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> Result<Config, String> {
    let config = state.config.lock().await;
    Ok(config.clone())
}

#[tauri::command]
async fn update_config(state: State<'_, AppState>, new_config: Config) -> Result<(), String> {
    // Save to file first
    AppState::save_config(&new_config)?;
    
    // Then update in-memory config
    let mut config = state.config.lock().await;
    *config = new_config;
    Ok(())
}

#[tauri::command]
async fn get_models(base_url: String, api_key: String, state: State<'_, AppState>) -> Result<Vec<ModelInfo>, String> {
    if api_key.is_empty() || base_url.is_empty() {
        return Err("API key and base URL are required".to_string());
    }

    let url = format!("{}/models", base_url);

    let response = state.http_client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await
        .map_err(|e| sanitize_error(&e.to_string()))?;

    if !response.status().is_success() {
        return Err(sanitize_error(&format!("Status: {}", response.status())));
    }

    let response_text = response.text().await
        .map_err(|_| "Failed to read response".to_string())?;

    let json: serde_json::Value = serde_json::from_str(&response_text)
        .map_err(|_| "Invalid response format".to_string())?;

    if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
        let models: Vec<ModelInfo> = data.iter()
            .filter_map(|model| {
                if let (Some(id), Some(object)) = (
                    model.get("id").and_then(|i| i.as_str()),
                    model.get("object").and_then(|o| o.as_str())
                ) {
                    Some(ModelInfo {
                        id: id.to_string(),
                        object: object.to_string(),
                    })
                } else {
                    None
                }
            })
            .collect();
        Ok(models)
    } else {
        Err("Invalid response format".to_string())
    }
}



#[tauri::command]
async fn take_interactive_screenshot() -> Result<String, String> {
    use std::process::Command;
    use std::fs;

    // Create temp file path with timestamp for uniqueness
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let temp_path = format!("/tmp/mathimage_screenshot_{}.png", timestamp);

    // Use macOS screencapture with interactive selection
    let output = Command::new("screencapture")
        .arg("-i")  // Interactive selection
        .arg("-r")  // Do not add drop shadow
        .arg(&temp_path)
        .output()
        .map_err(|e| format!("Failed to execute screencapture: {}", e))?;

    if !output.status.success() {
        return Err("Screenshot was cancelled".to_string()); // 用户取消，不显示对话框
    }

    // Check if file was created and has content
    if !std::path::Path::new(&temp_path).exists() {
        return Err("Screenshot was cancelled".to_string()); // 用户取消，不显示对话框
    }

    let metadata = fs::metadata(&temp_path)
        .map_err(|_| "Screenshot was cancelled".to_string())?; // 用户取消，不显示对话框

    if metadata.len() == 0 {
        // Clean up empty file
        let _ = fs::remove_file(&temp_path);
        return Err("Screenshot was cancelled".to_string()); // 用户取消，不显示对话框
    }

    // Read the image file with size limit (10MB max)
    const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
    if metadata.len() > MAX_FILE_SIZE {
        let _ = fs::remove_file(&temp_path);
        return Err("Screenshot file too large".to_string());
    }

    let image_data = fs::read(&temp_path)
        .map_err(|e| format!("Failed to read screenshot file: {}", e))?;

    // Clean up temp file
    let _ = fs::remove_file(&temp_path);

    // Convert to base64
    let base64_image = general_purpose::STANDARD.encode(&image_data);
    println!("Interactive screenshot captured, size: {} bytes", image_data.len());

    Ok(format!("data:image/png;base64,{}", base64_image))
}

#[tauri::command]
async fn take_screenshot_region(x: Option<u32>, y: Option<u32>, width: Option<u32>, height: Option<u32>) -> Result<String, String> {
    let screens = Screen::all().map_err(|_| "Failed to access screen".to_string())?;

    if screens.is_empty() {
        return Err("No screens found".to_string());
    }

    let screen = &screens[0]; // Use primary screen

    // Validate region size to prevent memory issues
    if let (Some(_), Some(_), Some(w), Some(h)) = (x, y, width, height) {
        const MAX_DIMENSION: u32 = 4096; // 4K max
        if w > MAX_DIMENSION || h > MAX_DIMENSION {
            return Err("Screenshot region too large".to_string());
        }

        // Check memory requirement (4 bytes per pixel for RGBA)
        const MAX_MEMORY: u64 = 64 * 1024 * 1024; // 64MB max
        let memory_needed = (w as u64) * (h as u64) * 4;
        if memory_needed > MAX_MEMORY {
            return Err("Screenshot would require too much memory".to_string());
        }
    }

    let image = if let (Some(x), Some(y), Some(w), Some(h)) = (x, y, width, height) {
        // Capture specific region
        screen.capture_area(x as i32, y as i32, w, h)
            .map_err(|_| "Failed to capture region".to_string())?
    } else {
        // Capture full screen
        screen.capture().map_err(|_| "Failed to capture screen".to_string())?
    };

    // Convert to base64 - screenshots::Image has rgba() method
    let rgba_data = image.rgba();
    let width = image.width();
    let height = image.height();

    // Create image from raw RGBA data
    let img = image::RgbaImage::from_raw(width, height, rgba_data.to_vec())
        .ok_or("Failed to create image from RGBA data")?;

    // Resize image if too large (max 512x512 to reduce size further)
    let max_size = 512;
    let (new_width, new_height) = if width > max_size || height > max_size {
        let scale = (max_size as f32 / width.max(height) as f32).min(1.0);
        ((width as f32 * scale) as u32, (height as f32 * scale) as u32)
    } else {
        (width, height)
    };

    let resized_img = if new_width != width || new_height != height {
        image::imageops::resize(&img, new_width, new_height, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    // Convert RGBA to RGB for JPEG (JPEG doesn't support alpha channel)
    let rgb_img: image::RgbImage = image::ImageBuffer::from_fn(new_width, new_height, |x, y| {
        let rgba = resized_img.get_pixel(x, y);
        image::Rgb([rgba[0], rgba[1], rgba[2]]) // Drop alpha channel
    });

    // Convert to JPEG for smaller size
    let mut buffer = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut buffer);
        image::write_buffer_with_format(
            &mut cursor,
            rgb_img.as_raw(),
            new_width,
            new_height,
            image::ColorType::Rgb8,
            image::ImageFormat::Jpeg,
        ).map_err(|e| format!("Failed to encode image: {}", e))?;
    }

    let base64_image = general_purpose::STANDARD.encode(&buffer);
    println!("Screenshot captured: {}x{} -> {}x{}, size: {} bytes",
             width, height, new_width, new_height, buffer.len());
    Ok(format!("data:image/jpeg;base64,{}", base64_image))
}

async fn analyze_image_internal(
    image_data: String,
    state: State<'_, AppState>,
    app_handle: Option<tauri::AppHandle>,
) -> Result<String, String> {
    let config = state.config.lock().await;

    if config.api_key.is_empty() || config.api_base_url.is_empty() {
        // Show system dialog for missing API config (only for hotkey usage)
        if let Some(ref _handle) = app_handle {
            if config.sound_enabled {
                // Play error sound
                if let Err(sound_err) = play_error_sound().await {
                    println!("Failed to play error sound: {}", sound_err);
                }
            }
            
            // Show macOS system dialog
            if let Err(dialog_err) = show_system_dialog(
                "MathImage Error".to_string(),
                "API key and base URL are required. Please configure them in Settings.".to_string(),
                "error".to_string()
            ).await {
                println!("Failed to show system dialog: {}", dialog_err);
            }
        }
        return Err("API key and base URL are required".to_string());
    }

    if config.model.is_empty() {
        // Show system dialog for missing model (only for hotkey usage)
        if let Some(ref _handle) = app_handle {
            if config.sound_enabled {
                // Play error sound
                if let Err(sound_err) = play_error_sound().await {
                    println!("Failed to play error sound: {}", sound_err);
                }
            }
            
            // Show macOS system dialog
            if let Err(dialog_err) = show_system_dialog(
                "MathImage Error".to_string(),
                "Please select a model first. Check Settings to load available models.".to_string(),
                "error".to_string()
            ).await {
                println!("Failed to show system dialog: {}", dialog_err);
            }
        }
        return Err("Please select a model first".to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .pool_max_idle_per_host(10)
        .http2_keep_alive_interval(std::time::Duration::from_secs(30))
        .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
    let url = format!("{}/chat/completions", config.api_base_url);

    println!("Analyzing image with model: {}", config.model);
    println!("Image data size: {} chars", image_data.len());

    // Check if image data is too large (some APIs have limits)
    if image_data.len() > 100_000 {
        println!("Warning: Image data is large ({} chars), this may cause timeouts", image_data.len());
    }

    let payload = serde_json::json!({
        "model": config.model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": config.prompt
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": image_data
                        }
                    }
                ]
            }
        ],
        "temperature": 1,
        "top_p": 1,
        "stream": true
    });

    println!("Sending request to: {}", url);
    println!("Payload size: {} bytes", serde_json::to_string(&payload).unwrap_or_default().len());

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json");

    // Only add auth headers if API key is provided
    if !config.api_key.is_empty() {
        request = request.header("Authorization", format!("Bearer {}", config.api_key));
    }

    // Retry logic for connection issues
    let mut last_error = String::new();
    for attempt in 1..=3 {
        println!("Attempt {} of 3", attempt);

        let response_result = request
            .try_clone()
            .ok_or("Failed to clone request")?
            .json(&payload)
            .send()
            .await;

        match response_result {
            Ok(response) => {
                println!("Request successful on attempt {}", attempt);

                if response.status().is_success() {
                    // Handle streaming response
                    use futures_util::StreamExt;

                    let mut stream = response.bytes_stream();
                    let mut full_content = String::new();
                    let mut buffer = String::new();

                    while let Some(chunk) = stream.next().await {
                        let chunk = chunk.map_err(|e| format!("Failed to read chunk: {}", e))?;
                        let chunk_str = String::from_utf8_lossy(&chunk);
                        buffer.push_str(&chunk_str);

                        // Process complete lines
                        while let Some(line_end) = buffer.find('\n') {
                            let line = buffer[..line_end].trim().to_string();
                            buffer = buffer[line_end + 1..].to_string();

                            if line.starts_with("data: ") {
                                let data = &line[6..]; // Remove "data: " prefix

                                if data == "[DONE]" {
                                    break;
                                }

                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                    if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
                                        if let Some(first_choice) = choices.first() {
                                            if let Some(delta) = first_choice.get("delta") {
                                                if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                                                    full_content.push_str(content);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if !full_content.is_empty() {
                        return Ok(full_content);
                    } else {
                        return Err("No content received from stream".to_string());
                    }
                } else {
                    let status = response.status();
                    let error_text = response.text().await.unwrap_or_default();
                    return Err(format!("Analysis failed with status {}: {}", status, error_text));
                }
            }
            Err(e) => {
                last_error = format!("Request failed: {}", e);
                println!("Attempt {} failed: {}", attempt, last_error);

                if attempt < 3 {
                    println!("Retrying in 2 seconds...");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    Err(format!("All 3 attempts failed. Last error: {}", last_error))
}

#[tauri::command]
async fn analyze_image(
    image_data: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    analyze_image_internal(image_data, state, None).await
}

#[tauri::command]
async fn copy_to_clipboard(text: String) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|e| format!("Failed to access clipboard: {}", e))?;
    clipboard.set_text(text).map_err(|e| format!("Failed to copy to clipboard: {}", e))?;
    println!("Text copied to clipboard");
    Ok(())
}

#[tauri::command]
async fn show_system_dialog(title: String, message: String, dialog_type: String) -> Result<(), String> {
    use std::process::Command;

    println!("Showing system dialog: {} - {}", title, message);

    // Determine the icon based on dialog type
    let icon = match dialog_type.as_str() {
        "error" => "stop",
        "warning" => "caution", 
        "info" => "note",
        _ => "note",
    };

    // Use macOS osascript to show system dialog
    let script = format!(
        r#"display dialog "{}" with title "{}" with icon {} buttons {{"OK"}} default button "OK""#,
        message.replace("\"", "\\\""),
        title.replace("\"", "\\\""),
        icon
    );

    println!("AppleScript: {}", script);

    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("Failed to show dialog: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        println!("osascript error: {}", stderr);
        return Err(format!("Failed to show system dialog: {}", stderr));
    }

    println!("System dialog shown successfully");
    Ok(())
}

#[tauri::command]
async fn play_system_sound() -> Result<(), String> {
    use std::process::Command;

    // Play macOS system sound (Glass)
    let output = Command::new("afplay")
        .arg("/System/Library/Sounds/Glass.aiff")
        .output()
        .map_err(|e| format!("Failed to play sound: {}", e))?;

    if !output.status.success() {
        return Err("Failed to play system sound".to_string());
    }

    Ok(())
}

#[tauri::command]
async fn play_error_sound() -> Result<(), String> {
    use std::process::Command;

    // Play macOS system error sound (Basso)
    let output = Command::new("afplay")
        .arg("/System/Library/Sounds/Basso.aiff")
        .output()
        .map_err(|e| format!("Failed to play error sound: {}", e))?;

    if !output.status.success() {
        return Err("Failed to play error sound".to_string());
    }

    Ok(())
}

#[allow(dead_code)]
fn format_hotkey_for_display(hotkey: &str) -> String {
    hotkey
        .replace("cmd", "Cmd")
        .replace("ctrl", "Ctrl")
        .replace("alt", "Alt")
        .replace("shift", "Shift")
        .split('+')
        .collect::<Vec<&str>>()
        .join("+")
}

#[tauri::command]
async fn update_tray_model(app_handle: tauri::AppHandle, model_name: String) -> Result<(), String> {
    update_tray_menu(app_handle, Some(model_name), None).await
}

async fn update_tray_menu(app_handle: tauri::AppHandle, model_name: Option<String>, sound_enabled: Option<bool>) -> Result<(), String> {
    // In Tauri v2, we need to recreate the tray menu to update it
    println!("Tray menu update requested for v2");

    // Get current config
    let app_state = app_handle.state::<AppState>();
    let config = app_state.config.lock().await;
    let current_config = config.clone();
    drop(config);

    // Use provided values or current config values
    let model_display = model_name.unwrap_or_else(|| {
        if current_config.model.is_empty() {
            "Not Selected".to_string()
        } else {
            current_config.model.clone()
        }
    });

    let sound_state = sound_enabled.unwrap_or(current_config.sound_enabled);

    println!("Updating tray menu - Model: {}, Sound: {}", model_display, sound_state);

    // Note: In Tauri v2, tray menu updates require recreating the tray
    // This is a limitation that may be addressed in future versions
    Ok(())
}

async fn rebuild_tray_with_models(app_handle: tauri::AppHandle, models: Vec<String>) -> Result<(), String> {
    println!("Models loaded: {:?}", models.iter().take(3).collect::<Vec<_>>());
    
    // Store the models in app state and save to cache
    let app_state = app_handle.state::<AppState>();
    let mut loaded_models = app_state.loaded_models.lock().await;
    *loaded_models = models.clone();
    drop(loaded_models);
    
    // Save models to persistent cache
    if let Err(e) = AppState::save_loaded_models(&models) {
        println!("Failed to save models to cache: {}", e);
    }
    
    println!("Successfully loaded {} models. Restart app to see them in tray menu.", models.len());
    
    Ok(())
}

fn create_tray_icon_with_menu(
    app_handle: &tauri::AppHandle,
    icon: tauri::image::Image<'_>,
    menu: tauri::menu::Menu<tauri::Wry>,
) -> Result<tauri::tray::TrayIcon, String> {
    TrayIconBuilder::new()
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_tray_icon_event(|_tray, event| {
            // Only log important events, not every mouse move
            match event {
                tauri::tray::TrayIconEvent::Click { .. } => {
                    println!("Tray icon clicked");
                }
                _ => {} // Don't log move, enter, leave events
            }
        })
        .on_menu_event({
            let app_handle_clone = app_handle.clone();
            move |app, event| {
                println!("Tray menu event: {:?}", event.id());
                match event.id().as_ref() {
                    "settings" => {
                        println!("Settings clicked - trying to show window");
                        if let Some(webview_window) = app.get_webview_window("main") {
                            let _ = webview_window.show();
                            let _ = webview_window.set_focus();
                            println!("Window shown successfully");
                        } else {
                            println!("Warning: No webview window named 'main' found");
                        }
                    }
                    "load_models" => {
                        println!("Load models clicked from tray");
                        let app_handle = app.app_handle().clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = refresh_models_in_tray(app_handle).await {
                                println!("Failed to refresh models: {}", e);
                            }
                        });
                    }
                    "toggle_sound" => {
                        println!("Toggle sound clicked");
                        let app_handle = app.app_handle().clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = toggle_sound_setting(app_handle).await {
                                println!("Failed to toggle sound: {}", e);
                            }
                        });
                    }
                    "quit" => {
                        println!("Quit clicked");
                        std::process::exit(0);
                    }
                    _ => {
                        // Handle model selection
                        if event.id().as_ref().starts_with("model_") {
                            let model_id = event.id().as_ref().strip_prefix("model_").unwrap().to_string();
                            println!("Model selected from tray: {}", model_id);

                            let app_handle = app_handle_clone.clone();
                            tauri::async_runtime::spawn(async move {
                                match select_model_in_tray(app_handle, model_id.clone()).await {
                                    Ok(()) => println!("Successfully selected model: {}", model_id),
                                    Err(e) => println!("Failed to select model {}: {}", model_id, e),
                                }
                            });
                        } else {
                            println!("Unknown menu item: {:?}", event.id());
                        }
                    }
                }
            }
        })
        .build(app_handle)
        .map_err(|e| format!("Failed to create tray icon: {}", e))
}

async fn refresh_models_in_tray(app_handle: tauri::AppHandle) -> Result<(), String> {
    println!("Loading models for tray menu update...");
    
    // Get current config to get API settings
    let app_state = app_handle.state::<AppState>();
    let config = app_state.config.lock().await;
    let api_key = config.api_key.clone();
    let base_url = config.api_base_url.clone();
    drop(config);
    
    if api_key.is_empty() || base_url.is_empty() {
        return Err("API key and base URL must be configured first".to_string());
    }
    
    // Get models using the same logic as get_models command
    let url = format!("{}/models", base_url);
    let response = app_state.http_client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch models: {}", e))?;
    
    if !response.status().is_success() {
        return Err(format!("API error: Status {}", response.status()));
    }
    
    let response_text = response.text().await
        .map_err(|_| "Failed to read response".to_string())?;
    
    let json: serde_json::Value = serde_json::from_str(&response_text)
        .map_err(|_| "Invalid JSON response".to_string())?;
    
    let models = if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
        data.iter()
            .filter_map(|model| {
                model.get("id").and_then(|i| i.as_str()).map(|s| s.to_string())
            })
            .collect::<Vec<String>>()
    } else {
        return Err("Invalid response format".to_string());
    };
    
    // Store the models in app state and save to cache
    let mut loaded_models = app_state.loaded_models.lock().await;
    *loaded_models = models.clone();
    drop(loaded_models);
    
    // Save models to persistent cache
    if let Err(e) = AppState::save_loaded_models(&models) {
        println!("Failed to save models to cache: {}", e);
    }
    
    println!("Successfully loaded {} models for tray", models.len());
    
    // Try to rebuild tray menu with the loaded models
    match rebuild_tray_with_models(app_handle, models).await {
        Ok(()) => println!("Tray menu rebuild completed"),
        Err(e) => println!("Failed to rebuild tray menu: {}", e),
    }
    
    Ok(())
}

#[allow(dead_code)]
async fn select_model_in_tray(app_handle: tauri::AppHandle, model_id: String) -> Result<(), String> {
    println!("🔍 [DEBUG] Selecting model from tray: {}", model_id);
    
    let state = app_handle.state::<AppState>();
    
    // Update config with selected model
    {
        let mut config = state.config.lock().await;
        config.model = model_id.clone();
        AppState::save_config(&*config)?;
    }
    
    println!("✓ [DEBUG] Model '{}' selected and saved to config", model_id);
    
    // Debug CheckMenuItem references availability
    {
        let model_check_items = state.model_check_items.lock().await;
        println!("🔍 [DEBUG] CheckMenuItem storage status:");
        println!("   - Total stored references: {}", model_check_items.len());
        
        if model_check_items.is_empty() {
            println!("❌ [DEBUG] No CheckMenuItem references found!");
            println!("   This means dynamic updates won't work - menu updates will be visible after app restart");
            println!("   The references may not have been stored yet or storage failed");
            return Ok(());
        }
        
        println!("   - Available model IDs: {:?}", model_check_items.keys().collect::<Vec<_>>());
        println!("   - Target model ID: '{}'", model_id);
        
        // Verify target model exists in our references
        if !model_check_items.contains_key(&model_id) {
            println!("⚠️  [DEBUG] Target model '{}' not found in CheckMenuItem references!", model_id);
            println!("   This could indicate a mismatch between loaded models and stored references");
        }
        
        println!("📝 [DEBUG] Implementing RadioButton behavior - updating {} CheckMenuItem states", model_check_items.len());
        
        let mut success_count = 0;
        let mut failure_count = 0;
        
        for (item_model_id, check_item) in model_check_items.iter() {
            let should_be_checked = item_model_id == &model_id;
            
            println!("   🔄 Processing '{}': setting checked={}", item_model_id, should_be_checked);
            
            // Use the dynamic update API
            match check_item.set_checked(should_be_checked) {
                Ok(()) => {
                    success_count += 1;
                    println!("      ✓ Successfully updated '{}' to checked={}", item_model_id, should_be_checked);
                }
                Err(e) => {
                    failure_count += 1;
                    println!("      ✗ Failed to update '{}': {}", item_model_id, e);
                }
            }
        }
        
        println!("📊 [DEBUG] RadioButton update summary:");
        println!("   - Successful updates: {}", success_count);
        println!("   - Failed updates: {}", failure_count);
        println!("   - Total processed: {}", model_check_items.len());
        
        if failure_count > 0 {
            println!("⚠️  [DEBUG] Some CheckMenuItem updates failed - dynamic updates may not be fully working");
        } else {
            println!("✅ [DEBUG] All CheckMenuItem updates completed successfully");
        }
    }
    
    // Update the submenu title to reflect the new selection
    println!("🔄 [DEBUG] Updating submenu title to show selected model...");
    {
        match state.model_submenu.try_lock() {
            Ok(submenu_ref) => {
                if let Some(submenu) = &*submenu_ref {
                    let new_title = format!("Model: {}", model_id);
                    println!("   📝 Attempting to update submenu title to: '{}'", new_title);
                    
                    match submenu.set_text(&new_title) {
                        Ok(()) => {
                            println!("   ✅ Successfully updated submenu title to '{}'", new_title);
                        }
                        Err(e) => {
                            println!("   ❌ Failed to update submenu title: {}", e);
                        }
                    }
                } else {
                    println!("   ⚠️  No submenu reference available for title update");
                }
            }
            Err(e) => {
                println!("   ❌ Failed to acquire submenu lock for title update: {}", e);
            }
        }
    }
    
    println!("✅ [DEBUG] Model '{}' selection process completed", model_id);
    Ok(())
}

#[allow(dead_code)]
async fn toggle_sound_setting(app_handle: tauri::AppHandle) -> Result<(), String> {
    // Toggle sound setting
    let state = app_handle.state::<AppState>();
    let mut config = state.config.lock().await;
    config.sound_enabled = !config.sound_enabled;
    let new_sound_state = config.sound_enabled;
    AppState::save_config(&*config)?;
    drop(config);

    println!("Sound setting toggled to: {}", new_sound_state);

    // Update tray menu to reflect the change
    if let Err(e) = update_tray_menu(app_handle, None, Some(new_sound_state)).await {
        println!("Failed to update tray menu after sound toggle: {}", e);
    }

    Ok(())
}

#[tauri::command]
async fn get_loaded_models(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let loaded_models = state.loaded_models.lock().await;
    Ok(loaded_models.clone())
}

#[tauri::command]
async fn refresh_tray_models(app_handle: tauri::AppHandle) -> Result<(), String> {
    refresh_models_in_tray(app_handle).await
}

#[tauri::command]
async fn update_hotkey(app_handle: tauri::AppHandle, new_hotkey: String, state: State<'_, AppState>) -> Result<(), String> {
    println!("Updating hotkey to: {}", new_hotkey);
    
    // Parse the new hotkey
    let shortcut: Shortcut = new_hotkey.parse()
        .map_err(|e| format!("Invalid hotkey format '{}': {}", new_hotkey, e))?;
    
    // Get current hotkey and unregister it  
    let current_hotkey = {
        let current_hotkey_lock = state.current_hotkey.lock().await;
        current_hotkey_lock.clone()
    };
    
    if let Some(current) = current_hotkey {
        println!("Unregistering current hotkey: {}", current);
        if let Ok(current_shortcut) = current.parse::<Shortcut>() {
            if let Err(e) = app_handle.global_shortcut().unregister(current_shortcut) {
                println!("Warning: Failed to unregister current hotkey '{}': {}", current, e);
            }
        }
    } else {
        // If no current hotkey stored, try to unregister the default one from config
        let config = state.config.lock().await;
        let default_hotkey = config.hotkey.clone();
        drop(config);
        
        if !default_hotkey.is_empty() && default_hotkey != new_hotkey {
            println!("Unregistering default hotkey: {}", default_hotkey);
            if let Ok(default_shortcut) = default_hotkey.parse::<Shortcut>() {
                if let Err(e) = app_handle.global_shortcut().unregister(default_shortcut) {
                    println!("Warning: Failed to unregister default hotkey '{}': {}", default_hotkey, e);
                }
            }
        }
    }
    
    // Register new hotkey (v2 doesn't support callbacks, events handled elsewhere)
    if let Err(e) = app_handle.global_shortcut().register(shortcut) {
        return Err(format!("Failed to register new hotkey '{}': {}", new_hotkey, e));
    }
    
    // Update stored current hotkey
    {
        let mut current_hotkey_lock = state.current_hotkey.lock().await;
        *current_hotkey_lock = Some(new_hotkey.clone());
    }
    
    // Update config
    {
        let mut config = state.config.lock().await;
        config.hotkey = new_hotkey.clone();
    }
    
    println!("Hotkey successfully updated to: {}", new_hotkey);
    Ok(())
}

#[tokio::main]
async fn main() {
    let app_state = AppState::new();
    
    // Get initial hotkey for plugin setup
    let initial_hotkey = {
        let config = app_state.config.lock().await;
        println!("Loading hotkey from config: {}", config.hotkey);
        config.hotkey.clone()
    };

    println!("Registering global shortcut: {}", initial_hotkey);

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcuts([initial_hotkey.as_str()]).unwrap()
                .with_handler(|app, shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        println!("Global shortcut triggered: {}", shortcut);
                        
                        // Get app handle for async operations
                        let app_handle = app.app_handle().clone();
                        tauri::async_runtime::spawn(async move {
                            // Handle screenshot and analysis
                            match take_interactive_screenshot().await {
                                Ok(image_data) => {
                                    if let Some(state) = app_handle.try_state::<AppState>() {
                                        match analyze_image_internal(image_data, state, Some(app_handle.clone())).await {
                                            Ok(result) => {
                                                println!("Hotkey analysis result: {}", result);
                                                
                                                // Copy to clipboard
                                                if let Err(e) = copy_to_clipboard(result.clone()).await {
                                                    println!("Failed to copy to clipboard: {}", e);
                                                }
                                                
                                                // Play sound if enabled
                                                if let Some(state) = app_handle.try_state::<AppState>() {
                                                    let config = state.config.lock().await;
                                                    if config.sound_enabled {
                                                        if let Err(e) = play_system_sound().await {
                                                            println!("Failed to play sound: {}", e);
                                                        }
                                                    }
                                                }
                                                
                                                // Emit event to frontend
                                                let _ = app_handle.emit("analysis_result", result);
                                            }
                                            Err(e) => {
                                                println!("Analysis error: {}", e);
                                                
                                                // Play error sound if enabled
                                                if let Some(state) = app_handle.try_state::<AppState>() {
                                                    let config = state.config.lock().await;
                                                    if config.sound_enabled {
                                                        if let Err(sound_err) = play_error_sound().await {
                                                            println!("Failed to play error sound: {}", sound_err);
                                                        }
                                                        
                                                        // Show macOS system dialog
                                                        if let Err(dialog_err) = show_system_dialog(
                                                            "MathImage Analysis Error".to_string(),
                                                            format!("Analysis failed: {}", e),
                                                            "error".to_string()
                                                        ).await {
                                                            println!("Failed to show system dialog: {}", dialog_err);
                                                        }
                                                    }
                                                }
                                                
                                                let _ = app_handle.emit("analysis_error", e);
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    if e.contains("Screenshot was cancelled") {
                                        println!("Screenshot was cancelled by user");
                                    } else {
                                        println!("Screenshot error: {}", e);
                                        
                                        // Show error dialog for real errors
                                        if let Some(state) = app_handle.try_state::<AppState>() {
                                            let config = state.config.lock().await;
                                            if config.sound_enabled {
                                                if let Err(sound_err) = play_error_sound().await {
                                                    println!("Failed to play error sound: {}", sound_err);
                                                }
                                                
                                                if let Err(dialog_err) = show_system_dialog(
                                                    "MathImage Screenshot Error".to_string(),
                                                    format!("Screenshot failed: {}", e),
                                                    "error".to_string()
                                                ).await {
                                                    println!("Failed to show system dialog: {}", dialog_err);
                                                }
                                            }
                                        }
                                        
                                        let _ = app_handle.emit("screenshot_error", e);
                                    }
                                }
                            }
                        });
                    }
                })
                .build(),
        )
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            update_config,
            get_models,
            get_loaded_models,
            take_interactive_screenshot,
            take_screenshot_region,
            analyze_image,
            copy_to_clipboard,
            update_tray_model,
            play_system_sound,
            play_error_sound,
            show_system_dialog,
            refresh_tray_models,
            update_hotkey
        ])
        .on_window_event(|webview_window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                // Hide window instead of closing
                webview_window.hide().unwrap();
                api.prevent_close();
            }
            _ => {}
        })
        .setup(|app| {
            // Get app state to load initial config
            let app_state = app.state::<AppState>();
            let initial_config = {
                // Use blocking lock since we're in setup
                match app_state.config.try_lock() {
                    Ok(config) => config.clone(),
                    Err(_) => Config::default()
                }
            };

            // Try to get pre-loaded models from app state
            let loaded_models = {
                match app_state.loaded_models.try_lock() {
                    Ok(models) => models.clone(),
                    Err(_) => Vec::new()
                }
            };

            println!("Creating tray menu with {} pre-loaded models", loaded_models.len());

            // Create comprehensive tray menu with models if available
            let settings_item = MenuItemBuilder::new("Settings").id("settings").build(app)?;

            // Model selection submenu - include loaded models if available
            let model_display = if initial_config.model.is_empty() {
                "Not Selected"
            } else {
                &initial_config.model
            };

            // Create model submenu with load action and available models
            let mut model_submenu_builder = SubmenuBuilder::new(app, &format!("Model: {}", model_display));
            
            // Always add "Load Models" option first
            let load_models_item = MenuItemBuilder::new("Load Models").id("load_models").build(app)?;
            model_submenu_builder = model_submenu_builder.item(&load_models_item);
            
            // If we have loaded models, add them to the menu
            if !loaded_models.is_empty() {
                model_submenu_builder = model_submenu_builder.separator();
                
                // Store CheckMenuItem references directly during creation
                let mut check_items_for_storage = std::collections::HashMap::new();
                
                // If we have loaded models, add them as CheckMenuItems
                for model_id in &loaded_models {
                    let is_current = model_id == &initial_config.model;
                    
                    println!("🔍 [DEBUG] Creating CheckMenuItem for model '{}', checked={}", model_id, is_current);
                    
                    let model_item = CheckMenuItemBuilder::new(model_id)
                        .id(&format!("model_{}", model_id))
                        .checked(is_current)
                        .build(app)?;
                    
                    // Store the CheckMenuItem reference immediately
                    check_items_for_storage.insert(model_id.clone(), model_item.clone());
                    println!("   📝 Stored CheckMenuItem reference for '{}'", model_id);
                    
                    model_submenu_builder = model_submenu_builder.item(&model_item);
                }
                
                println!("✓ [DEBUG] Added {} models to tray menu with CheckMenuItem support", loaded_models.len());
                println!("📦 [DEBUG] Prepared {} CheckMenuItem references for storage", check_items_for_storage.len());
                
                // Store references immediately without async delay
                println!("🔄 [DEBUG] Storing CheckMenuItem references immediately...");
                {
                    let storage_count = check_items_for_storage.len();
                    
                    // Use try_lock to avoid blocking in async context
                    match app_state.model_check_items.try_lock() {
                        Ok(mut model_check_items) => {
                            println!("📋 [DEBUG] Before storage - current references count: {}", model_check_items.len());
                            *model_check_items = check_items_for_storage;
                            println!("✅ [DEBUG] After storage - new references count: {}", model_check_items.len());
                            
                            println!("🎉 [DEBUG] CheckMenuItem references stored successfully for dynamic updates!");
                            println!("   - Expected count: {}", storage_count);
                            println!("   - Actual count: {}", model_check_items.len());
                            
                            if model_check_items.len() != storage_count {
                                println!("⚠️  [DEBUG] Count mismatch detected in CheckMenuItem storage!");
                            }
                            
                            // Debug list stored model IDs
                            let stored_ids: Vec<_> = model_check_items.keys().collect();
                            println!("📑 [DEBUG] Stored model IDs: {:?}", stored_ids);
                        }
                        Err(e) => {
                            println!("❌ [DEBUG] Failed to acquire lock for CheckMenuItem storage: {}", e);
                            println!("   CheckMenuItem references will not be available for dynamic updates");
                        }
                    }
                }
            }
            
            let model_submenu = model_submenu_builder.build()?;
            
            // Store the model submenu reference for dynamic updates
            println!("🔄 [DEBUG] Storing model submenu reference for title updates...");
            match app_state.model_submenu.try_lock() {
                Ok(mut submenu_ref) => {
                    *submenu_ref = Some(model_submenu.clone());
                    println!("✅ [DEBUG] Model submenu reference stored successfully");
                }
                Err(e) => {
                    println!("❌ [DEBUG] Failed to store model submenu reference: {}", e);
                }
            }

            // Hotkey display
            let formatted_hotkey = format_hotkey_for_display(&initial_config.hotkey);
            let hotkey_item = MenuItemBuilder::new(&format!("Hotkey: {}", formatted_hotkey))
                .id("hotkey_info")
                .enabled(false)
                .build(app)?;

            // Sound setting
            let sound_text = if initial_config.sound_enabled { "Enabled" } else { "Disabled" };
            let sound_item = MenuItemBuilder::new(&format!("Sound: {}", sound_text))
                .id("toggle_sound")
                .build(app)?;

            let quit_item = MenuItemBuilder::new("Quit").id("quit").build(app)?;

            // Build comprehensive menu
            let menu = MenuBuilder::new(app)
                .item(&model_submenu)
                .item(&hotkey_item)
                .item(&sound_item)
                .separator()
                .item(&settings_item)
                .separator()
                .item(&quit_item)
                .build()?;

            // Create tray icon with proper configuration
            // Load icon from embedded bytes - decode PNG first
            let icon_bytes = include_bytes!("../icons/32x32.png");
            let icon = image::load_from_memory(icon_bytes)
                .map_err(|e| format!("Failed to load icon: {}", e))?
                .to_rgba8();
            let (width, height) = icon.dimensions();
            let icon = tauri::image::Image::new_owned(icon.into_raw(), width, height);

            // Create tray using the helper function
            let _tray = create_tray_icon_with_menu(&app.handle(), icon, menu)
                .map_err(|e| {
                    eprintln!("Failed to create tray icon: {}", e);
                    format!("Failed to create tray icon: {}", e)
                })?;

            // Store the tray icon in app state for dynamic menu updates
            // Note: Skip storing in setup due to async limitations
            println!("Tray icon created successfully with {} models", loaded_models.len());

            println!("Comprehensive tray menu created successfully");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}