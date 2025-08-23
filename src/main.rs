use tauri::{State, GlobalShortcutManager, Manager, SystemTray, SystemTrayMenu, SystemTrayMenuItem, SystemTraySubmenu, CustomMenuItem, SystemTrayEvent};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use screenshots::Screen;
use base64::{Engine as _, engine::general_purpose};
use arboard::Clipboard;

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

        Self {
            config: Arc::new(Mutex::new(Config::default())),
            current_hotkey: Arc::new(Mutex::new(None)),
            http_client,
        }
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


fn create_system_tray() -> SystemTray {
    let model_submenu = SystemTrayMenu::new()
        .add_item(CustomMenuItem::new("model_loading".to_string(), "Loading models..."));

    let model_item = SystemTraySubmenu::new("Model: Not Selected", model_submenu);
    let hotkey_item = CustomMenuItem::new("hotkey".to_string(), "Hotkey: Cmd+Shift+M");
    let sound_item = CustomMenuItem::new("toggle_sound".to_string(), "Sound: Enabled");
    let settings_item = CustomMenuItem::new("settings".to_string(), "Settings");
    let quit_item = CustomMenuItem::new("quit".to_string(), "Quit");

    let tray_menu = SystemTrayMenu::new()
        .add_submenu(model_item)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(hotkey_item)
        .add_item(sound_item)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(settings_item)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(quit_item);

    SystemTray::new()
        .with_menu(tray_menu)
}

#[tauri::command]
async fn update_tray_model(app_handle: tauri::AppHandle, model_name: String) -> Result<(), String> {
    update_tray_menu(app_handle, Some(model_name), None).await
}

async fn update_tray_menu(app_handle: tauri::AppHandle, model_name: Option<String>, sound_enabled: Option<bool>) -> Result<(), String> {
    // Get current config if values not provided
    let state = app_handle.state::<AppState>();
    let config = state.config.lock().await;

    let current_model = model_name.unwrap_or_else(|| config.model.clone());
    let current_sound = sound_enabled.unwrap_or(config.sound_enabled);
    let current_hotkey = config.hotkey.clone();
    drop(config);

    let models_submenu = SystemTrayMenu::new()
        .add_item(CustomMenuItem::new("refresh_models".to_string(), "Refresh Models"));

    let model_item = SystemTraySubmenu::new(
        &format!("Model: {}", if current_model.is_empty() { "Not Selected" } else { &current_model }),
        models_submenu
    );

    // Format hotkey for display (capitalize modifiers)
    let formatted_hotkey = format_hotkey_for_display(&current_hotkey);
    let hotkey_item = CustomMenuItem::new("hotkey".to_string(), &format!("Hotkey: {}", formatted_hotkey));
    let sound_item = CustomMenuItem::new("toggle_sound".to_string(),
        &format!("Sound: {}", if current_sound { "Enabled" } else { "Disabled" }));
    let settings_item = CustomMenuItem::new("settings".to_string(), "Settings");
    let quit_item = CustomMenuItem::new("quit".to_string(), "Quit");

    let tray_menu = SystemTrayMenu::new()
        .add_submenu(model_item)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(hotkey_item)
        .add_item(sound_item)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(settings_item)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(quit_item);

    app_handle.tray_handle().set_menu(tray_menu)
        .map_err(|e| format!("Failed to update tray menu: {}", e))?;

    Ok(())
}

async fn refresh_models_in_tray(app_handle: tauri::AppHandle) -> Result<(), String> {
    // Get current config
    let state = app_handle.state::<AppState>();
    let config = state.config.lock().await;

    if config.api_base_url.is_empty() || config.api_key.is_empty() {
        return Err("API URL and key not configured".to_string());
    }

    let base_url = config.api_base_url.clone();
    let api_key = config.api_key.clone();
    let current_model = config.model.clone();
    drop(config); // Release the lock

    // Fetch models
    match get_models(base_url, api_key, state).await {
        Ok(models) => {
            // Create submenu with models
            let mut models_submenu = SystemTrayMenu::new()
                .add_item(CustomMenuItem::new("refresh_models".to_string(), "Refresh Models"))
                .add_native_item(SystemTrayMenuItem::Separator);

            let models_count = models.len();
            for model in &models {
                let is_selected = model.id == current_model;
                let item_text = if is_selected {
                    format!("✓ {}", model.id)
                } else {
                    model.id.clone()
                };

                models_submenu = models_submenu.add_item(
                    CustomMenuItem::new(format!("select_model_{}", model.id), item_text)
                );
            }

            // Update tray menu
            let model_item = SystemTraySubmenu::new(
                &format!("Model: {}", if current_model.is_empty() { "Not Selected" } else { &current_model }),
                models_submenu
            );

            // Get current hotkey from config for display
            let state = app_handle.state::<AppState>();
            let config = state.config.lock().await;
            let current_hotkey = config.hotkey.clone();
            drop(config);
            
            let formatted_hotkey = format_hotkey_for_display(&current_hotkey);
            let hotkey_item = CustomMenuItem::new("hotkey".to_string(), &format!("Hotkey: {}", formatted_hotkey));
            let settings_item = CustomMenuItem::new("settings".to_string(), "Settings");
            let quit_item = CustomMenuItem::new("quit".to_string(), "Quit");

            let tray_menu = SystemTrayMenu::new()
                .add_submenu(model_item)
                .add_native_item(SystemTrayMenuItem::Separator)
                .add_item(hotkey_item)
                .add_native_item(SystemTrayMenuItem::Separator)
                .add_item(settings_item)
                .add_native_item(SystemTrayMenuItem::Separator)
                .add_item(quit_item);

            app_handle.tray_handle().set_menu(tray_menu)
                .map_err(|e| format!("Failed to update tray menu: {}", e))?;

            println!("Tray menu updated with {} models", models_count);
            Ok(())
        }
        Err(e) => {
            println!("Failed to fetch models: {}", e);
            Err(e)
        }
    }
}

async fn select_model_in_tray(app_handle: tauri::AppHandle, model_id: String) -> Result<(), String> {
    // Update config with selected model
    let state = app_handle.state::<AppState>();
    let mut config = state.config.lock().await;
    config.model = model_id.clone();
    drop(config);

    println!("Selected model: {}", model_id);

    // Refresh the tray menu to show the new selection
    refresh_models_in_tray(app_handle).await
}

async fn toggle_sound_setting(app_handle: tauri::AppHandle) -> Result<(), String> {
    // Toggle sound setting
    let state = app_handle.state::<AppState>();
    let mut config = state.config.lock().await;
    config.sound_enabled = !config.sound_enabled;
    let new_sound_state = config.sound_enabled;
    drop(config);

    println!("Sound setting toggled to: {}", new_sound_state);

    // Update tray menu to show the new state
    update_tray_menu(app_handle, None, Some(new_sound_state)).await
}

#[tauri::command]
async fn refresh_tray_models(app_handle: tauri::AppHandle) -> Result<(), String> {
    refresh_models_in_tray(app_handle).await
}

#[tauri::command]
async fn update_hotkey(app_handle: tauri::AppHandle, new_hotkey: String, state: State<'_, AppState>) -> Result<(), String> {
    println!("Updating hotkey to: {}", new_hotkey);
    
    let mut shortcut_manager = app_handle.global_shortcut_manager();
    
    // Get current hotkey and unregister it
    let current_hotkey = {
        let current_hotkey_lock = state.current_hotkey.lock().await;
        current_hotkey_lock.clone()
    };
    
    if let Some(current) = current_hotkey {
        println!("Unregistering current hotkey: {}", current);
        if let Err(e) = shortcut_manager.unregister(&current) {
            println!("Warning: Failed to unregister current hotkey '{}': {}", current, e);
        }
    } else {
        // If no current hotkey stored, try to unregister the default one from config
        let config = state.config.lock().await;
        let default_hotkey = config.hotkey.clone();
        drop(config);
        
        if !default_hotkey.is_empty() && default_hotkey != new_hotkey {
            println!("Unregistering default hotkey: {}", default_hotkey);
            if let Err(e) = shortcut_manager.unregister(&default_hotkey) {
                println!("Warning: Failed to unregister default hotkey '{}': {}", default_hotkey, e);
            }
        }
    }
    
    // Register new hotkey
    let app_handle_clone = app_handle.clone();
    if let Err(e) = shortcut_manager.register(&new_hotkey, move || {
        let app_handle = app_handle_clone.clone();
        tauri::async_runtime::spawn(async move {
            // Use interactive screenshot for hotkey
            match take_interactive_screenshot().await {
                Ok(image_data) => {
                    // Get current config
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

                                let _ = app_handle.emit_all("analysis_result", result);
                            }
                            Err(e) => {
                                println!("Analysis error: {}", e);

                                // Play error sound and show system dialog if enabled
                                if let Some(state) = app_handle.try_state::<AppState>() {
                                    let config = state.config.lock().await;
                                    if config.sound_enabled {
                                        // Play error sound
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

                                let _ = app_handle.emit_all("analysis_error", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    println!("Screenshot error: {}", e);

                    // Only show dialog for real errors, not user cancellations
                    if e == "Screenshot was cancelled" {
                        // User cancelled - just log, no dialog
                        println!("User cancelled screenshot selection");
                    } else {
                        // Real error - show dialog and play error sound if enabled
                        if let Some(state) = app_handle.try_state::<AppState>() {
                            let config = state.config.lock().await;
                            if config.sound_enabled {
                                // Play error sound
                                if let Err(sound_err) = play_error_sound().await {
                                    println!("Failed to play error sound: {}", sound_err);
                                }
                                
                                // Show macOS system dialog
                                if let Err(dialog_err) = show_system_dialog(
                                    "MathImage Screenshot Error".to_string(),
                                    format!("Screenshot failed: {}", e),
                                    "error".to_string()
                                ).await {
                                    println!("Failed to show system dialog: {}", dialog_err);
                                }
                            }
                        }
                    }

                    let _ = app_handle.emit_all("screenshot_error", e);
                }
            }
        });
    }) {
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

    tauri::Builder::default()
        .manage(app_state)
        .system_tray(create_system_tray())
        .on_system_tray_event(|app, event| match event {
            SystemTrayEvent::MenuItemClick { id, .. } => {
                match id.as_str() {
                    "settings" => {
                        let window = app.get_window("main").unwrap();
                        window.show().unwrap();
                        window.set_focus().unwrap();
                    }
                    "refresh_models" => {
                        let app_handle = app.app_handle();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = refresh_models_in_tray(app_handle).await {
                                println!("Failed to refresh models: {}", e);
                            }
                        });
                    }
                    "toggle_sound" => {
                        let app_handle = app.app_handle();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = toggle_sound_setting(app_handle).await {
                                println!("Failed to toggle sound: {}", e);
                            }
                        });
                    }
                    "quit" => {
                        std::process::exit(0);
                    }
                    id if id.starts_with("select_model_") => {
                        let model_id = id.strip_prefix("select_model_").unwrap().to_string();
                        let app_handle = app.app_handle();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = select_model_in_tray(app_handle, model_id).await {
                                println!("Failed to select model: {}", e);
                            }
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            update_config,
            get_models,
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
        .on_window_event(|event| match event.event() {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                // Hide window instead of closing
                event.window().hide().unwrap();
                api.prevent_close();
            }
            _ => {}
        })
        .setup(|app| {
            let app_handle = app.handle();

            // Register global shortcut for screenshot using config hotkey
            let mut shortcut_manager = app.global_shortcut_manager();
            let initial_hotkey = {
                let state = app_handle.state::<AppState>();
                let config = futures::executor::block_on(state.config.lock());
                config.hotkey.clone()
            };
            
            // Store the initial hotkey
            {
                let state = app_handle.state::<AppState>();
                let mut current_hotkey = futures::executor::block_on(state.current_hotkey.lock());
                *current_hotkey = Some(initial_hotkey.clone());
            }
            
            shortcut_manager.register(&initial_hotkey, move || {
                let app_handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    // Use interactive screenshot for hotkey
                    match take_interactive_screenshot().await {
                        Ok(image_data) => {
                            // Get current config
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

                                        let _ = app_handle.emit_all("analysis_result", result);
                                    }
                                    Err(e) => {
                                        println!("Analysis error: {}", e);

                                        // Play error sound and show system dialog if enabled
                                        if let Some(state) = app_handle.try_state::<AppState>() {
                                            let config = state.config.lock().await;
                                            if config.sound_enabled {
                                                // Play error sound
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

                                        let _ = app_handle.emit_all("analysis_error", e);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("Screenshot error: {}", e);

                            // Only show dialog for real errors, not user cancellations
                            if e == "Screenshot was cancelled" {
                                // User cancelled - just log, no dialog
                                println!("User cancelled screenshot selection");
                            } else {
                                // Real error - show dialog and play error sound if enabled
                                if let Some(state) = app_handle.try_state::<AppState>() {
                                    let config = state.config.lock().await;
                                    if config.sound_enabled {
                                        // Play error sound
                                        if let Err(sound_err) = play_error_sound().await {
                                            println!("Failed to play error sound: {}", sound_err);
                                        }
                                        
                                        // Show macOS system dialog
                                        if let Err(dialog_err) = show_system_dialog(
                                            "MathImage Screenshot Error".to_string(),
                                            format!("Screenshot failed: {}", e),
                                            "error".to_string()
                                        ).await {
                                            println!("Failed to show system dialog: {}", dialog_err);
                                        }
                                    }
                                }
                            }

                            let _ = app_handle.emit_all("screenshot_error", e);
                        }
                    }
                });
            }).map_err(|e| format!("Failed to register shortcut: {}", e))?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}