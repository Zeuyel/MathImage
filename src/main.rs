use tauri::{State, Manager, Emitter, tray::TrayIconBuilder, menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder}};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
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


async fn setup_tray_and_shortcuts(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let app_state = app.state::<AppState>();
    let initial_config = {
        let config = app_state.config.lock().await;
        config.clone()
    };

    // Create menu items
    let model_submenu = MenuBuilder::new(app)
        .item(&MenuItemBuilder::new("Loading models...").id("model_loading").build(app)?)
        .build()?;

    let model_display = if initial_config.model.is_empty() { "Not Selected" } else { &initial_config.model };
    let model_item = SubmenuBuilder::new(app, &format!("Model: {}", model_display))
        .build()?;
    
    // Format hotkey for display
    let formatted_hotkey = format_hotkey_for_display(&initial_config.hotkey);
    let hotkey_item = MenuItemBuilder::new(&format!("Hotkey: {}", formatted_hotkey))
        .id("hotkey")
        .build(app)?;
    
    let sound_text = if initial_config.sound_enabled { "Enabled" } else { "Disabled" };
    let sound_item = MenuItemBuilder::new(&format!("Sound: {}", sound_text))
        .id("toggle_sound")
        .build(app)?;
    
    let settings_item = MenuItemBuilder::new("Settings").id("settings").build(app)?;
    let quit_item = MenuItemBuilder::new("Quit").id("quit").build(app)?;

    // Build main menu
    let menu = MenuBuilder::new(app)
        .item(&model_item)
        .separator()
        .item(&hotkey_item) 
        .item(&sound_item)
        .separator()
        .item(&settings_item)
        .separator()
        .item(&quit_item)
        .build()?;

    // Create tray icon
    let _tray = TrayIconBuilder::new()
        .menu(&menu)
        .on_menu_event({
            let app_handle = app.handle().clone();
            move |app, event| {
                match event.id().as_ref() {
                    "settings" => {
                        if let Some(webview_window) = app.get_webview_window("main") {
                            let _ = webview_window.show();
                            let _ = webview_window.set_focus();
                        }
                    }
                    "refresh_models" => {
                        let app_handle = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = refresh_models_in_tray(app_handle).await {
                                println!("Failed to refresh models: {}", e);
                            }
                        });
                    }
                    "toggle_sound" => {
                        let app_handle = app_handle.clone();
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
                        let app_handle = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = select_model_in_tray(app_handle, model_id).await {
                                println!("Failed to select model: {}", e);
                            }
                        });
                    }
                    _ => {}
                }
            }
        })
        .build(app)?;

    // Register initial global shortcut (just register, handle events separately)
    let initial_hotkey = initial_config.hotkey.clone();
    if !initial_hotkey.is_empty() {
        if let Ok(shortcut) = initial_hotkey.parse::<Shortcut>() {
            if let Err(e) = app.global_shortcut().register(shortcut) {
                println!("Failed to register shortcut '{}': {}", initial_hotkey, e);
            } else {
                println!("Registered Accelerator: {}", shortcut);
            }
        }
    }

    Ok(())
}

#[tauri::command]
async fn update_tray_model(app_handle: tauri::AppHandle, model_name: String) -> Result<(), String> {
    update_tray_menu(app_handle, Some(model_name), None).await
}

async fn update_tray_menu(app_handle: tauri::AppHandle, model_name: Option<String>, sound_enabled: Option<bool>) -> Result<(), String> {
    // TODO: Implement tray menu update for Tauri v2
    println!("Tray menu update requested - not yet implemented in v2");
    Ok(())
}

async fn refresh_models_in_tray(app_handle: tauri::AppHandle) -> Result<(), String> {
    // TODO: Implement tray models refresh for Tauri v2  
    println!("Tray models refresh requested - not yet implemented in v2");
    Ok(())
}

async fn select_model_in_tray(app_handle: tauri::AppHandle, model_id: String) -> Result<(), String> {
    // Update config with selected model
    let state = app_handle.state::<AppState>();
    let mut config = state.config.lock().await;
    config.model = model_id.clone();
    AppState::save_config(&*config)?;
    drop(config);

    println!("Selected model: {}", model_id);
    Ok(())
}

async fn toggle_sound_setting(app_handle: tauri::AppHandle) -> Result<(), String> {
    // Toggle sound setting
    let state = app_handle.state::<AppState>();
    let mut config = state.config.lock().await;
    config.sound_enabled = !config.sound_enabled;
    let new_sound_state = config.sound_enabled;
    AppState::save_config(&*config)?;
    drop(config);

    println!("Sound setting toggled to: {}", new_sound_state);
    Ok(())
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

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(app_state)
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
        .on_window_event(|webview_window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                // Hide window instead of closing
                webview_window.hide().unwrap();
                api.prevent_close();
            }
            _ => {}
        })
        .setup(|app| {
            // Setup tray and shortcuts
            tauri::async_runtime::block_on(setup_tray_and_shortcuts(app))?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}