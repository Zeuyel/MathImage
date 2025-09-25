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
pub struct ApiConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PromptMode {
    Predefined(String),
    UserInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutputMode {
    Clipboard,
    Dialog,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub api_config: ApiConfig,
    pub prompt_mode: PromptMode,
    pub output_mode: OutputMode,
    // 移除hotkey字段 - 热键应该是全局的，不属于单个profile
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub global_hotkey: String,
    pub switch_profile_hotkey: String,
    pub profiles: Vec<Profile>,
    pub active_profile_id: Option<String>,
    pub sound_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        // 创建默认Profile
        let default_profile = Profile {
            id: uuid::Uuid::new_v4().to_string(),
            name: "默认配置".to_string(),
            api_config: ApiConfig {
                base_url: "http://210.126.8.197:11434/v1".to_string(),
                api_key: "".to_string(),
                model: "".to_string(),
            },
            prompt_mode: PromptMode::Predefined(
                "识别公式和文字，返回使用pandoc语法的markdown排版内容。公式请用katex语法包裹，文字内容不要丢失。只返回内容不需要其他解释。".to_string()
            ),
            output_mode: OutputMode::Clipboard,
        };

        Self {
            global_hotkey: "cmd+shift+m".to_string(),
            switch_profile_hotkey: "cmd+shift+p".to_string(),
            profiles: vec![default_profile.clone()],
            active_profile_id: Some(default_profile.id),
            sound_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigUpdates {
    pub active_profile_id: Option<String>,
    pub sound_enabled: Option<bool>,
    pub global_hotkey: Option<String>,
    pub switch_profile_hotkey: Option<String>,
}

#[derive(Debug, Default)]
pub struct ProfileConfigUpdate {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub prompt_mode: Option<PromptMode>,
    pub output_mode: Option<OutputMode>,
}

#[derive(Clone)]
pub struct AppState {
    config: Arc<Mutex<Config>>,
    current_global_hotkey: Arc<Mutex<Option<String>>>,
    current_switch_hotkey: Arc<Mutex<Option<String>>>,
    http_client: reqwest::Client,
    loaded_models: Arc<Mutex<Vec<String>>>,
    // Store references to CheckMenuItems for dynamic updates
    model_check_items: Arc<Mutex<std::collections::HashMap<String, tauri::menu::CheckMenuItem<tauri::Wry>>>>,
    // Store reference to the model submenu for title updates
    model_submenu: Arc<Mutex<Option<tauri::menu::Submenu<tauri::Wry>>>>,
    // Store references to Profile CheckMenuItems for profile switching
    profile_check_items: Arc<Mutex<std::collections::HashMap<String, tauri::menu::CheckMenuItem<tauri::Wry>>>>,
    // Store reference to the profile submenu for title updates
    profile_submenu: Arc<Mutex<Option<tauri::menu::Submenu<tauri::Wry>>>>,
    // Store references to hotkey and sound menu items to allow text updates without rebuilding tray
    global_hotkey_item: Arc<Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>>>,
    switch_hotkey_item: Arc<Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>>>,
    sound_item: Arc<Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>>>,
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
            current_global_hotkey: Arc::new(Mutex::new(None)),
            current_switch_hotkey: Arc::new(Mutex::new(None)),
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
            profile_check_items: Arc::new(Mutex::new(std::collections::HashMap::new())),
            profile_submenu: Arc::new(Mutex::new(None)),
            global_hotkey_item: Arc::new(Mutex::new(None)),
            switch_hotkey_item: Arc::new(Mutex::new(None)),
            sound_item: Arc::new(Mutex::new(None)),
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

    // 改进的配置保存方法 - 确保原子性操作
    async fn save_config_atomic(config: &Config) -> Result<(), String> {
        let config_path = Self::get_config_path()?;
        let temp_path = config_path.with_extension("tmp");

        // 先写入临时文件
        let config_data = serde_json::to_string_pretty(config)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;

        fs::write(&temp_path, config_data)
            .map_err(|e| format!("Failed to write temp config file: {}", e))?;

        // 原子性重命名
        fs::rename(&temp_path, &config_path)
            .map_err(|e| format!("Failed to save config file: {}", e))?;

        println!("Config saved atomically to: {:?}", config_path);
        Ok(())
    }

    // 安全的配置更新方法 - 在一个事务中完成更新和保存
    async fn update_and_save_config<F>(&self, updater: F) -> Result<(), String> 
    where
        F: FnOnce(&mut Config) -> Result<(), String>,
    {
        let mut config = self.config.lock().await;
        
        // 先应用更新
        updater(&mut *config)?;
        
        // 然后原子性保存
        let config_clone = config.clone();
        drop(config); // 释放锁后再保存，避免长时间持有锁
        
        Self::save_config_atomic(&config_clone).await
    }

    // 简化的Profile管理方法
    
    // 核心方法1：创建新Profile并自动切换
    async fn create_new_profile(&self, name: String) -> Result<String, String> {
        let mut result_profile_id = String::new();
        
        self.update_and_save_config(|config| {
            // 验证profile name是否重复
            if config.profiles.iter().any(|p| p.name == name) {
                return Err(format!("Profile name '{}' already exists", name));
            }
            
            // 创建默认Profile
            let new_profile = Profile {
                id: uuid::Uuid::new_v4().to_string(),
                name: name.clone(),
                api_config: ApiConfig {
                    base_url: "http://210.126.8.197:11434/v1".to_string(),
                    api_key: "".to_string(),
                    model: "".to_string(),
                },
                prompt_mode: PromptMode::Predefined(
                    "识别公式和文字，返回使用pandoc语法的markdown排版内容。公式请用katex语法包裹，文字内容不要丢失。只返回内容不需要其他解释。".to_string()
                ),
                output_mode: OutputMode::Clipboard,
            };
            
            let profile_id = new_profile.id.clone();
            result_profile_id = profile_id.clone();
            config.profiles.push(new_profile);
            config.active_profile_id = Some(profile_id.clone());
            
            println!("   📝 Created and activated new profile: {} ({})", name, profile_id);
            Ok(())
        }).await?;
        
        Ok(result_profile_id)
    }
    
    // 核心方法2：更新当前活跃Profile的配置
    async fn update_active_profile_config(&self, updates: ProfileConfigUpdate) -> Result<(), String> {
        self.update_and_save_config(|config| {
            let active_id = config.active_profile_id.clone()
                .ok_or("No active profile")?;

            let profile = config.profiles.iter_mut()
                .find(|p| p.id == active_id)
                .ok_or("Active profile not found")?;

            // 只更新提供的字段
            if let Some(name) = updates.name {
                profile.name = name;
            }
            if let Some(base_url) = updates.base_url {
                profile.api_config.base_url = base_url;
            }
            if let Some(api_key) = updates.api_key {
                profile.api_config.api_key = api_key;
            }
            if let Some(model) = updates.model {
                profile.api_config.model = model;
            }
            if let Some(prompt_mode) = updates.prompt_mode {
                profile.prompt_mode = prompt_mode;
            }
            if let Some(output_mode) = updates.output_mode {
                profile.output_mode = output_mode;
            }
            
            println!("   📝 Updated active profile configuration");
            Ok(())
        }).await
    }
    
    // 批量操作 - 减少IO次数
    async fn update_multiple_settings(&self, updates: ConfigUpdates) -> Result<(), String> {
        self.update_and_save_config(|config| {
            if let Some(active_id) = updates.active_profile_id {
                config.active_profile_id = Some(active_id);
            }
            
            if let Some(sound_enabled) = updates.sound_enabled {
                config.sound_enabled = sound_enabled;
            }
            
            // 注意：热键更新应该独立处理，不在profile中
            if let Some(global_hotkey) = updates.global_hotkey {
                config.global_hotkey = global_hotkey;
            }
            
            if let Some(switch_hotkey) = updates.switch_profile_hotkey {
                config.switch_profile_hotkey = switch_hotkey;
            }
            
            println!("   📝 Updated multiple settings atomically");
            Ok(())
        }).await
    }
    async fn get_active_profile(&self) -> Result<Profile, String> {
        let config = self.config.lock().await;

        if let Some(active_id) = &config.active_profile_id {
            if let Some(profile) = config.profiles.iter().find(|p| &p.id == active_id) {
                return Ok(profile.clone());
            }
        }

        // 如果没有活跃profile或找不到，返回第一个profile
        config.profiles.first()
            .cloned()
            .ok_or_else(|| "No profiles available".to_string())
    }

    async fn set_active_profile(&self, profile_id: String) -> Result<(), String> {
        self.update_and_save_config(|config| {
            // 验证profile存在
            if !config.profiles.iter().any(|p| p.id == profile_id) {
                return Err(format!("Profile with id '{}' not found", profile_id));
            }

            config.active_profile_id = Some(profile_id);
            println!("Active profile updated");
            Ok(())
        }).await
    }

    async fn get_next_profile_id(&self) -> Result<String, String> {
        let config = self.config.lock().await;

        if config.profiles.is_empty() {
            return Err("No profiles available".to_string());
        }

        if config.profiles.len() == 1 {
            return Ok(config.profiles[0].id.clone());
        }

        // 找到当前活跃profile的索引
        let current_index = if let Some(active_id) = &config.active_profile_id {
            config.profiles.iter().position(|p| &p.id == active_id).unwrap_or(0)
        } else {
            0
        };

        // 获取下一个profile的索引（循环）
        let next_index = (current_index + 1) % config.profiles.len();
        Ok(config.profiles[next_index].id.clone())
    }
}

// Profile切换功能
async fn switch_to_next_profile(app_handle: tauri::AppHandle) -> Result<(), String> {
    let app_state = app_handle.state::<AppState>();

    // 获取下一个profile ID
    let next_profile_id = app_state.get_next_profile_id().await?;

    // 切换到下一个profile
    app_state.set_active_profile(next_profile_id.clone()).await?;

    // 获取新的活跃profile信息
    let active_profile = app_state.get_active_profile().await?;

    // 显示系统通知
    show_profile_switch_notification(&app_handle, &active_profile).await?;

    // 更新托盘菜单中的profile选择状态
    update_profile_menu_selection(&app_handle, &next_profile_id).await?;

    // Update profile submenu title
    println!("🔧 [DEBUG] Updating profile submenu title from switch hotkey...");
    update_profile_submenu_title(&app_handle, &active_profile.name).await?;

    println!("Switched to profile: {} ({})", active_profile.name, active_profile.id);
    Ok(())
}

async fn show_profile_switch_notification(app_handle: &tauri::AppHandle, profile: &Profile) -> Result<(), String> {
    // 使用Tauri的通知API显示profile切换信息
    let notification_text = format!("Profile: {}", profile.name);

    // 这里使用系统通知，类似Mac输入法切换的效果
    app_handle.emit("profile-switched", &notification_text)
        .map_err(|e| format!("Failed to emit profile switch event: {}", e))?;

    println!("Profile switch notification sent: {}", notification_text);
    Ok(())
}

async fn update_profile_menu_selection(app_handle: &tauri::AppHandle, selected_profile_id: &str) -> Result<(), String> {
    let app_state = app_handle.state::<AppState>();

    // 更新profile CheckMenuItem的选中状态（radio button行为）
    let profile_check_items = app_state.profile_check_items.lock().await;

    if profile_check_items.is_empty() {
        println!("No profile CheckMenuItem references found for update");
        return Ok(());
    }

    for (profile_id, check_item) in profile_check_items.iter() {
        let should_be_checked = profile_id == selected_profile_id;

        match check_item.set_checked(should_be_checked) {
            Ok(()) => {
                println!("Updated profile '{}' checked state to: {}", profile_id, should_be_checked);
            }
            Err(e) => {
                println!("Failed to update profile '{}' checked state: {}", profile_id, e);
            }
        }
    }

    Ok(())
}

async fn update_profile_submenu_title(app_handle: &tauri::AppHandle, profile_name: &str) -> Result<(), String> {
    println!("📝 [DEBUG] Updating profile submenu title to: '{}'", profile_name);
    
    let state = app_handle.state::<AppState>();
    
    // Update the profile submenu title to show the new profile name
    match state.profile_submenu.try_lock() {
        Ok(submenu_ref) => {
            if let Some(submenu) = &*submenu_ref {
                let new_title = format!("Profile: {}", profile_name);
                println!("   📝 Attempting to update profile submenu title to: '{}'", new_title);
                
                match submenu.set_text(&new_title) {
                    Ok(()) => {
                        println!("   ✅ Successfully updated profile submenu title to '{}'", new_title);
                    }
                    Err(e) => {
                        println!("   ❌ Failed to update profile submenu title: {}", e);
                    }
                }
            } else {
                println!("   ⚠️  No profile submenu reference available for title update");
            }
        }
        Err(e) => {
            println!("   ❌ Failed to acquire profile submenu lock for title update: {}", e);
        }
    }
    
    Ok(())
}

async fn update_model_submenu_title(app_handle: &tauri::AppHandle, model_name: &str) -> Result<(), String> {
    println!("📝 [DEBUG] Updating model submenu title to: '{}'", model_name);

    let state = app_handle.state::<AppState>();
    match state.model_submenu.try_lock() {
        Ok(submenu_ref) => {
            if let Some(submenu) = &*submenu_ref {
                let new_title = format!("Model: {}", model_name);
                println!("   📝 Attempting to update model submenu title to: '{}'", new_title);
                match submenu.set_text(&new_title) {
                    Ok(()) => println!("   ✅ Successfully updated model submenu title"),
                    Err(e) => println!("   ❌ Failed to update model submenu title: {}", e),
                }
            } else {
                println!("   ⚠️  No model submenu reference available for title update");
            }
        }
        Err(e) => println!("   ❌ Failed to acquire model submenu lock for title update: {}", e),
    }

    Ok(())
}

async fn update_model_menu_selection(app_handle: &tauri::AppHandle, selected_model_id: &str) -> Result<(), String> {
    let app_state = app_handle.state::<AppState>();
    let items = app_state.model_check_items.lock().await;
    if items.is_empty() {
        println!("No model CheckMenuItem references found for update");
        return Ok(());
    }
    for (model_id, check_item) in items.iter() {
        let should_be_checked = model_id == selected_model_id;
        if let Err(e) = check_item.set_checked(should_be_checked) {
            println!("Failed to update model '{}' checked state: {}", model_id, e);
        }
    }
    Ok(())
}

async fn update_hotkey_menu_text(app_handle: &tauri::AppHandle, global_hotkey: &str, switch_hotkey: &str) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    let formatted_global = format_hotkey_for_display(global_hotkey);
    let formatted_switch = format_hotkey_for_display(switch_hotkey);

    if let Ok(item_guard) = state.global_hotkey_item.try_lock() {
        if let Some(item) = &*item_guard {
            if let Err(e) = item.set_text(&format!("Global: {}", formatted_global)) {
                println!("Failed to update global hotkey item text: {}", e);
            }
        }
    }

    if let Ok(item_guard) = state.switch_hotkey_item.try_lock() {
        if let Some(item) = &*item_guard {
            if let Err(e) = item.set_text(&format!("Switch: {}", formatted_switch)) {
                println!("Failed to update switch hotkey item text: {}", e);
            }
        }
    }

    Ok(())
}

async fn update_sound_menu_text(app_handle: &tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    let text = if enabled { "Enabled" } else { "Disabled" };
    if let Ok(item_guard) = state.sound_item.try_lock() {
        if let Some(item) = &*item_guard {
            if let Err(e) = item.set_text(&format!("Sound: {}", text)) {
                println!("Failed to update sound item text: {}", e);
            }
        }
    }
    Ok(())
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

// 简化的Tauri命令 - 保持前端兼容

#[tauri::command]
async fn create_profile(state: State<'_, AppState>, profile: serde_json::Value) -> Result<String, String> {
    println!("🔧 [DEBUG] Creating profile from frontend data...");
    
    // 从前端数据中提取profile名称
    let name = profile.get("name")
        .and_then(|v| v.as_str())
        .ok_or("Profile name is required")?;
    
    // 使用简化的内部方法
    let profile_id = state.create_new_profile(name.to_string()).await?;
    println!("✅ [DEBUG] Profile created successfully: {} ({})", name, profile_id);
    Ok(profile_id)
}

#[tauri::command]
async fn update_profile_config(state: State<'_, AppState>, profile_data: serde_json::Value) -> Result<(), String> {
    println!("🔧 [DEBUG] Updating profile config (legacy compatibility)...");
    
    // 直接使用新的update_active_profile逻辑
    update_active_profile(state, profile_data).await
}

#[tauri::command]
async fn update_active_profile(state: State<'_, AppState>, update_data: serde_json::Value) -> Result<(), String> {
    println!("🔧 [DEBUG] Updating active profile configuration...");
    
    let mut updates = ProfileConfigUpdate::default();
    
    // 解析更新字段
    if let Some(name) = update_data.get("profileName").and_then(|v| v.as_str()) {
        if !name.trim().is_empty() {
            updates.name = Some(name.trim().to_string());
        }
    }
    
    if let Some(base_url) = update_data.get("apiBaseUrl").and_then(|v| v.as_str()) {
        updates.base_url = Some(base_url.to_string());
    }
    
    if let Some(api_key) = update_data.get("apiKey").and_then(|v| v.as_str()) {
        updates.api_key = Some(api_key.to_string());
    }
    
    if let Some(model) = update_data.get("model").and_then(|v| v.as_str()) {
        updates.model = Some(model.to_string());
    }
    
    // 解析prompt模式
    if let Some(prompt_mode) = update_data.get("promptMode").and_then(|v| v.as_str()) {
        match prompt_mode {
            "user_input" => {
                updates.prompt_mode = Some(PromptMode::UserInput);
            }
            "predefined" | _ => {
                let prompt_text = update_data.get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("识别公式和文字，返回使用pandoc语法的markdown排版内容。公式请用katex语法包裹，文字内容不要丢失。只返回内容不需要其他解释。");
                updates.prompt_mode = Some(PromptMode::Predefined(prompt_text.to_string()));
            }
        }
    }
    
    // 解析输出模式  
    if let Some(output_mode) = update_data.get("outputMode").and_then(|v| v.as_str()) {
        match output_mode {
            "dialog" => {
                updates.output_mode = Some(OutputMode::Dialog);
            }
            "clipboard" | _ => {
                updates.output_mode = Some(OutputMode::Clipboard);
            }
        }
    }
    
    state.update_active_profile_config(updates).await?;
    
    // 同时更新全局设置（如果提供）
    if let Some(sound_enabled) = update_data.get("soundEnabled").and_then(|v| v.as_bool()) {
        let global_updates = ConfigUpdates {
            sound_enabled: Some(sound_enabled),
            active_profile_id: None,
            global_hotkey: None,
            switch_profile_hotkey: None,
        };
        state.update_multiple_settings(global_updates).await?;
    }
    
    println!("✅ [DEBUG] Active profile updated successfully");
    Ok(())
}

#[tauri::command]
async fn delete_profile(state: State<'_, AppState>, profile_id: String) -> Result<(), String> {
    println!("🔧 [DEBUG] Deleting profile: {}", profile_id);
    
    state.update_and_save_config(|config| {
        if config.profiles.len() <= 1 {
            return Err("Cannot delete the last profile".to_string());
        }

        let profile_index = config.profiles.iter()
            .position(|p| p.id == profile_id)
            .ok_or("Profile not found")?;

        let deleted_profile = config.profiles.remove(profile_index);

        // 如果删除的是活跃profile，切换到第一个profile
        if config.active_profile_id.as_ref() == Some(&profile_id) {
            config.active_profile_id = config.profiles.first().map(|p| p.id.clone());
            println!("   📝 Switched active profile to: {:?}", config.active_profile_id);
        }

        println!("   📝 Deleted profile: {} ({})", deleted_profile.name, profile_id);
        Ok(())
    }).await?;

    println!("✅ [DEBUG] Profile deleted successfully: {}", profile_id);
    Ok(())
}

#[tauri::command]
async fn set_active_profile(app_handle: tauri::AppHandle, state: State<'_, AppState>, profile_id: String) -> Result<(), String> {
    state.set_active_profile(profile_id.clone()).await?;
    
    // Get the new active profile for submenu title update
    let active_profile = state.get_active_profile().await?;
    
    // Update profile submenu title
    println!("🔧 [DEBUG] Updating profile submenu title from Settings page...");
    update_profile_submenu_title(&app_handle, &active_profile.name).await?;
    
    Ok(())
}

#[tauri::command]
async fn update_config(state: State<'_, AppState>, new_config: Config) -> Result<(), String> {
    println!("🔧 [DEBUG] Updating entire configuration...");
    
    // 先原子性保存到文件
    AppState::save_config_atomic(&new_config).await?;
    
    // 然后更新内存中的配置
    let mut config = state.config.lock().await;
    *config = new_config;
    
    println!("✅ [DEBUG] Configuration updated successfully");
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

// 新的分析函数，支持自定义prompt
async fn analyze_image_with_prompt(
    image_data: String,
    state: State<'_, AppState>,
    custom_prompt: Option<String>,
    app_handle: Option<tauri::AppHandle>,
) -> Result<String, String> {
    // 使用活跃profile的配置
    let active_profile = state.get_active_profile().await?;
    let config = state.config.lock().await;
    let sound_enabled = config.sound_enabled;
    drop(config);

    // 验证API配置
    if active_profile.api_config.api_key.is_empty() || active_profile.api_config.base_url.is_empty() {
        // Show system dialog for missing API config (only for hotkey usage)
        if let Some(ref _handle) = app_handle {
            if sound_enabled {
                // Play error sound
                if let Err(sound_err) = play_error_sound().await {
                    println!("Failed to play error sound: {}", sound_err);
                }
            }

            // Show macOS system dialog
            if let Err(dialog_err) = show_system_dialog(
                "MathImage Error".to_string(),
                format!("Profile '{}': API key and base URL are required. Please configure them in Settings.", active_profile.name),
                "error".to_string()
            ).await {
                println!("Failed to show system dialog: {}", dialog_err);
            }
        }
        return Err(format!("Profile '{}': API key and base URL are required", active_profile.name));
    }

    if active_profile.api_config.model.is_empty() {
        // Show system dialog for missing model (only for hotkey usage)
        if let Some(ref _handle) = app_handle {
            if sound_enabled {
                // Play error sound
                if let Err(sound_err) = play_error_sound().await {
                    println!("Failed to play error sound: {}", sound_err);
                }
            }

            // Show macOS system dialog
            if let Err(dialog_err) = show_system_dialog(
                "MathImage Error".to_string(),
                format!("Profile '{}': Please select a model first. Check Settings to load available models.", active_profile.name),
                "error".to_string()
            ).await {
                println!("Failed to show system dialog: {}", dialog_err);
            }
        }
        return Err(format!("Profile '{}': Please select a model first", active_profile.name));
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
    let url = format!("{}/chat/completions", active_profile.api_config.base_url);

    println!("Analyzing image with profile '{}' using model: {}", active_profile.name, active_profile.api_config.model);
    println!("Image data size: {} chars", image_data.len());

    // Check if image data is too large (some APIs have limits)
    if image_data.len() > 100_000 {
        println!("Warning: Image data is large ({} chars), this may cause timeouts", image_data.len());
    }

    // 确定使用的prompt：自定义prompt优先，否则使用Profile的prompt模式
    let prompt_text = if let Some(custom) = custom_prompt {
        println!("Using custom prompt: {}", custom);
        custom
    } else {
        match &active_profile.prompt_mode {
            PromptMode::Predefined(prompt) => {
                println!("Using predefined prompt from profile: {}", prompt);
                prompt.clone()
            },
            PromptMode::UserInput => {
                // TODO: 实现用户输入prompt的逻辑
                println!("Profile requires user input prompt, using default");
                "识别公式和文字，返回使用pandoc语法的markdown排版内容。公式请用katex语法包裹，文字内容不要丢失。只返回内容不需要其他解释。".to_string()
            }
        }
    };

    let payload = serde_json::json!({
        "model": active_profile.api_config.model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": prompt_text
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
    if !active_profile.api_config.api_key.is_empty() {
        request = request.header("Authorization", format!("Bearer {}", active_profile.api_config.api_key));
    }

    // 继续使用现有的请求处理逻辑...
    analyze_image_request_internal(request, payload).await
}

// 保持向后兼容的原函数
async fn analyze_image_internal(
    image_data: String,
    state: State<'_, AppState>,
    app_handle: Option<tauri::AppHandle>,
) -> Result<String, String> {
    analyze_image_with_prompt(image_data, state, None, app_handle).await
}

// 提取请求处理逻辑为独立函数
async fn analyze_image_request_internal(
    request: reqwest::RequestBuilder,
    payload: serde_json::Value,
) -> Result<String, String> {

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
    // Update model submenu title and check selection without rebuilding the tray
    update_model_submenu_title(&app_handle, &model_name).await?;
    update_model_menu_selection(&app_handle, &model_name).await?;
    Ok(())
}

async fn update_tray_menu(app_handle: tauri::AppHandle, model_name: Option<String>, sound_enabled: Option<bool>) -> Result<(), String> {
    println!("🔄 [DEBUG] Updating tray menu in-place (no rebuild)...");
    // Get current config
    let app_state = app_handle.state::<AppState>();
    let config = app_state.config.lock().await;
    let current_config = config.clone();
    drop(config);

    // Update profile submenu title
    if let Some(active_id) = &current_config.active_profile_id {
        if let Some(profile) = current_config.profiles.iter().find(|p| &p.id == active_id) {
            update_profile_submenu_title(&app_handle, &profile.name).await.ok();
        }
    }

    // Update model submenu title and checked state
    let model_display = if let Some(name) = model_name.clone() {
        name
    } else if let Some(active_id) = &current_config.active_profile_id {
        if let Some(profile) = current_config.profiles.iter().find(|p| &p.id == active_id) {
            if profile.api_config.model.is_empty() { "Not Selected".to_string() } else { profile.api_config.model.clone() }
        } else { "Not Selected".to_string() }
    } else { "Not Selected".to_string() };

    update_model_submenu_title(&app_handle, &model_display).await.ok();
    if let Some(name) = model_name {
        update_model_menu_selection(&app_handle, &name).await.ok();
    }

    // Update hotkey display items
    update_hotkey_menu_text(&app_handle, &current_config.global_hotkey, &current_config.switch_profile_hotkey).await.ok();

    // Update sound menu item text
    let sound_state = sound_enabled.unwrap_or(current_config.sound_enabled);
    update_sound_menu_text(&app_handle, sound_state).await.ok();

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
                        // Handle profile selection
                        if event.id().as_ref().starts_with("profile_") {
                            let profile_id = event.id().as_ref().strip_prefix("profile_").unwrap().to_string();
                            println!("Profile selected from tray: {}", profile_id);

                            let app_handle = app_handle_clone.clone();
                            tauri::async_runtime::spawn(async move {
                                match select_profile_in_tray(app_handle, profile_id.clone()).await {
                                    Ok(()) => println!("Successfully selected profile: {}", profile_id),
                                    Err(e) => println!("Failed to select profile {}: {}", profile_id, e),
                                }
                            });
                        }
                        // Handle model selection
                        else if event.id().as_ref().starts_with("model_") {
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

async fn select_profile_in_tray(app_handle: tauri::AppHandle, profile_id: String) -> Result<(), String> {
    println!("🔍 [DEBUG] Selecting profile from tray: {}", profile_id);

    let app_state = app_handle.state::<AppState>();

    // Set the active profile
    app_state.set_active_profile(profile_id.clone()).await?;

    // Update profile CheckMenuItem selection (radio button behavior)
    update_profile_menu_selection(&app_handle, &profile_id).await?;

    // Get the new active profile for notification
    let active_profile = app_state.get_active_profile().await?;

    // Show profile switch notification
    show_profile_switch_notification(&app_handle, &active_profile).await?;

    // Update tray menu to reflect the new active profile name in submenu title
    println!("🔧 [DEBUG] Updating profile submenu title...");
    update_profile_submenu_title(&app_handle, &active_profile.name).await?;

    println!("✅ [DEBUG] Profile '{}' selected successfully from tray", active_profile.name);
    Ok(())
}

async fn refresh_models_in_tray(app_handle: tauri::AppHandle) -> Result<(), String> {
    println!("Loading models for tray menu update...");
    
    // Get current active profile's API settings
    let app_state = app_handle.state::<AppState>();
    let active_profile = app_state.get_active_profile().await?;
    let api_key = active_profile.api_config.api_key.clone();
    let base_url = active_profile.api_config.base_url.clone();
    
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
    
    println!("Successfully loaded {} models for tray (will update on next build or selection)", models.len());

    Ok(())
}

#[allow(dead_code)]
async fn select_model_in_tray(app_handle: tauri::AppHandle, model_id: String) -> Result<(), String> {
    println!("🔍 [DEBUG] Selecting model from tray: {}", model_id);
    
    let state = app_handle.state::<AppState>();
    
    // Update active profile with selected model using atomic save
    state.update_and_save_config(|config| {
        // Clone active profile ID to avoid borrow conflicts
        let active_id = config.active_profile_id.clone()
            .ok_or("No active profile")?;

        // Find and update the profile
        let profile = config.profiles.iter_mut()
            .find(|p| p.id == active_id)
            .ok_or("Active profile not found")?;

        profile.api_config.model = model_id.clone();
        println!("   📝 Updated model to: {}", model_id);
        Ok(())
    }).await?;
    
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
    println!("🔧 [DEBUG] Toggling sound setting...");
    
    let state = app_handle.state::<AppState>();
    
    state.update_and_save_config(|config| {
        config.sound_enabled = !config.sound_enabled;
        let new_sound_state = config.sound_enabled;
        println!("   📝 Sound setting toggled to: {}", new_sound_state);
        Ok(())
    }).await?;

    // Read new state and update tray menu item text
    let state = app_handle.state::<AppState>();
    let enabled = {
        let cfg = state.config.lock().await;
        cfg.sound_enabled
    };
    if let Err(e) = update_sound_menu_text(&app_handle, enabled).await {
        println!("⚠️ [WARNING] Failed to update sound menu text: {}", e);
    }

    println!("✅ [DEBUG] Sound setting updated successfully");
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
async fn refresh_tray_menu(app_handle: tauri::AppHandle) -> Result<(), String> {
    // 刷新整个托盘菜单，包括Profile列表
    println!("Refreshing tray menu with updated profiles");

    // 获取当前配置
    let app_state = app_handle.state::<AppState>();
    let config = app_state.config.lock().await;
    let current_config = config.clone();
    drop(config);

    // 获取活跃profile的模型信息
    let model_display = if let Some(active_id) = &current_config.active_profile_id {
        if let Some(profile) = current_config.profiles.iter().find(|p| &p.id == active_id) {
            if profile.api_config.model.is_empty() {
                "Not Selected".to_string()
            } else {
                profile.api_config.model.clone()
            }
        } else {
            "Not Selected".to_string()
        }
    } else {
        "Not Selected".to_string()
    };

    // 使用现有的托盘菜单更新逻辑
    update_tray_menu(app_handle, Some(model_display), Some(current_config.sound_enabled)).await
}

#[tauri::command]
async fn update_hotkeys(app_handle: tauri::AppHandle, state: State<'_, AppState>, global_hotkey: String, switch_hotkey: String) -> Result<(), String> {
    println!("🔧 [DEBUG] Updating hotkeys - Global: {}, Switch: {}", global_hotkey, switch_hotkey);

    // Update config atomically
    state.update_and_save_config(|config| {
        config.global_hotkey = global_hotkey.clone();
        config.switch_profile_hotkey = switch_hotkey.clone();
        println!("   📝 Updated hotkeys in config");
        Ok(())
    }).await?;

    // Update current hotkey tracking
    {
        let mut current_global = state.current_global_hotkey.lock().await;
        *current_global = Some(global_hotkey.clone());
    }
    {
        let mut current_switch = state.current_switch_hotkey.lock().await;
        *current_switch = Some(switch_hotkey.clone());
    }

    // Use internal registration function (clone to avoid moving the originals)
    let gh = global_hotkey.clone();
    let sh = switch_hotkey.clone();
    register_hotkeys_internal(app_handle.clone(), gh, sh).await?;

    // Update tray menu items text in-place
    println!("🔧 [DEBUG] Updating tray menu hotkey labels in-place...");
    if let Err(e) = update_hotkey_menu_text(&app_handle, &global_hotkey, &switch_hotkey).await {
        println!("⚠️ [WARNING] Failed to update hotkey labels: {}", e);
    }

    println!("✅ [DEBUG] Hotkeys updated and re-registered successfully - no restart required!");
    Ok(())
}

// 内部热键注册函数，不包含托盘菜单更新
async fn register_hotkeys_internal(app_handle: tauri::AppHandle, global_hotkey: String, switch_hotkey: String) -> Result<(), String> {
    println!("🔧 [DEBUG] Registering hotkeys internally - Global: {}, Switch: {}", global_hotkey, switch_hotkey);
    
    // Unregister all current shortcuts
    if let Err(e) = app_handle.global_shortcut().unregister_all() {
        println!("⚠️ [WARNING] Failed to unregister existing shortcuts: {}", e);
    } else {
        println!("✅ [DEBUG] Unregistered all existing shortcuts");
    }
    
    // Parse and register new shortcuts
    let global_shortcut = global_hotkey.parse::<tauri_plugin_global_shortcut::Shortcut>()
        .map_err(|e| format!("Invalid global hotkey '{}': {}", global_hotkey, e))?;
    
    let switch_shortcut = switch_hotkey.parse::<tauri_plugin_global_shortcut::Shortcut>()
        .map_err(|e| format!("Invalid switch hotkey '{}': {}", switch_hotkey, e))?;
    
    // Register global shortcut
    app_handle.global_shortcut().on_shortcut(global_shortcut.clone(), move |app, shortcut, event| {
        if event.state == ShortcutState::Pressed {
            println!("Global shortcut triggered: {}", shortcut);
            let app_handle = app.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                handle_global_hotkey(app_handle).await;
            });
        }
    }).map_err(|e| format!("Failed to register global hotkey '{}': {}", global_hotkey, e))?;
    
    // Register switch shortcut  
    app_handle.global_shortcut().on_shortcut(switch_shortcut.clone(), move |app, shortcut, event| {
        if event.state == ShortcutState::Pressed {
            println!("Switch shortcut triggered: {}", shortcut);
            let app_handle = app.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                handle_switch_hotkey(app_handle).await;
            });
        }
    }).map_err(|e| format!("Failed to register switch hotkey '{}': {}", switch_hotkey, e))?;

    println!("✅ [DEBUG] Hotkeys registered successfully");
    Ok(())
}

// 保持向后兼容的单热键更新函数
#[tauri::command]
async fn update_hotkey(app_handle: tauri::AppHandle, new_hotkey: String, state: State<'_, AppState>) -> Result<(), String> {
    println!("🔧 [DEBUG] Updating global hotkey to: {}", new_hotkey);

    // Parse the new hotkey
    let shortcut: Shortcut = new_hotkey.parse()
        .map_err(|e| format!("Invalid hotkey format '{}': {}", new_hotkey, e))?;

    // Get current global hotkey and unregister it
    let current_hotkey = {
        let current_hotkey_lock = state.current_global_hotkey.lock().await;
        current_hotkey_lock.clone()
    };

    if let Some(current) = current_hotkey {
        println!("Unregistering current global hotkey: {}", current);
        if let Ok(current_shortcut) = current.parse::<Shortcut>() {
            if let Err(e) = app_handle.global_shortcut().unregister(current_shortcut) {
                println!("Warning: Failed to unregister current global hotkey '{}': {}", current, e);
            }
        }
    }

    // Register new hotkey
    if let Err(e) = app_handle.global_shortcut().register(shortcut) {
        return Err(format!("Failed to register new global hotkey '{}': {}", new_hotkey, e));
    }

    // Update stored current hotkey
    {
        let mut current_hotkey_lock = state.current_global_hotkey.lock().await;
        *current_hotkey_lock = Some(new_hotkey.clone());
    }

    // Update config atomically
    state.update_and_save_config(|config| {
        config.global_hotkey = new_hotkey.clone();
        println!("   📝 Updated global hotkey in config");
        Ok(())
    }).await?;

    println!("✅ [DEBUG] Global hotkey successfully updated to: {}", new_hotkey);
    Ok(())
}

// 热键处理函数
async fn handle_global_hotkey(app_handle: tauri::AppHandle) {
    println!("Handling global hotkey - taking screenshot and analyzing");

    // 获取当前活跃的profile
    if let Some(state) = app_handle.try_state::<AppState>() {
        match state.get_active_profile().await {
            Ok(active_profile) => {
                println!("Using profile: {} ({})", active_profile.name, active_profile.id);

                // 根据profile的prompt模式处理
                match active_profile.prompt_mode {
                    PromptMode::Predefined(prompt) => {
                        // 使用预定义prompt进行截图和分析
                        handle_screenshot_with_prompt(app_handle, prompt, active_profile.output_mode).await;
                    }
                    PromptMode::UserInput => {
                        // 实现用户输入prompt的逻辑
                        println!("User input prompt mode - showing input dialog");
                        handle_screenshot_with_user_input(app_handle, active_profile.output_mode).await;
                    }
                }
            }
            Err(e) => {
                println!("Failed to get active profile: {}", e);
            }
        }
    }
}

async fn handle_switch_hotkey(app_handle: tauri::AppHandle) {
    println!("Handling switch hotkey - switching to next profile");

    match switch_to_next_profile(app_handle).await {
        Ok(()) => {
            println!("Profile switched successfully");
        }
        Err(e) => {
            println!("Failed to switch profile: {}", e);
        }
    }
}

async fn handle_screenshot_with_prompt(app_handle: tauri::AppHandle, prompt: String, output_mode: OutputMode) {
    match take_interactive_screenshot().await {
        Ok(image_data) => {
            if let Some(state) = app_handle.try_state::<AppState>() {
                // 使用新的analyze_image_with_prompt函数，传递自定义prompt
                match analyze_image_with_prompt(image_data, state, Some(prompt), Some(app_handle.clone())).await {
                    Ok(result) => {
                        println!("Analysis result: {}", result);

                        // 根据output_mode处理结果
                        match output_mode {
                            OutputMode::Clipboard => {
                                if let Err(e) = copy_to_clipboard(result.clone()).await {
                                    println!("Failed to copy to clipboard: {}", e);
                                }
                            }
                            OutputMode::Dialog => {
                                // 显示系统对话框
                                if let Err(e) = show_system_dialog(
                                    "MathImage Analysis Result".to_string(),
                                    result.clone(),
                                    "info".to_string()
                                ).await {
                                    println!("Failed to show system dialog: {}", e);
                                }
                            }
                        }

                        // 播放成功音效
                        if let Some(state) = app_handle.try_state::<AppState>() {
                            let config = state.config.lock().await;
                            if config.sound_enabled {
                                if let Err(e) = play_system_sound().await {
                                    println!("Failed to play sound: {}", e);
                                }
                            }
                        }

                        // 发送事件到前端
                        let _ = app_handle.emit("analysis_result", result);
                    }
                    Err(e) => {
                        println!("Analysis error: {}", e);
                        let _ = app_handle.emit("analysis_error", sanitize_error(&e));
                    }
                }
            }
        }
        Err(e) => {
            println!("Screenshot error: {}", e);
            let _ = app_handle.emit("screenshot_error", e);
        }
    }
}

async fn show_input_dialog(_app_handle: tauri::AppHandle, title: String, default_text: String) -> Result<String, String> {
    use std::process::Command;
    println!("Showing input dialog: {}", title);
    
    // Use macOS osascript to show text input dialog
    let script = format!(
        r#"display dialog "{}" default answer "{}" with title "MathImage - User Input" with icon note buttons {{"Cancel", "OK"}} default button "OK""#,
        title.replace("\"", "\\\""),
        default_text.replace("\"", "\\\"")
    );
    
    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("Failed to execute osascript: {}", e))?;
        
    if output.status.success() {
        let result = String::from_utf8_lossy(&output.stdout);
        // Parse the result - AppleScript returns "button returned:OK, text returned:user_input"
        if let Some(text_start) = result.find("text returned:") {
            let user_text = &result[text_start + 14..].trim();
            Ok(user_text.to_string())
        } else {
            Err("Failed to parse dialog result".to_string())
        }
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        if error.contains("User canceled") || error.contains("-128") {
            Err("User cancelled dialog".to_string())
        } else {
            Err(format!("osascript failed: {}", error))
        }
    }
}

async fn handle_screenshot_with_user_input(app_handle: tauri::AppHandle, output_mode: OutputMode) {
    // 首先显示输入对话框获取用户自定义prompt
    match show_input_dialog(app_handle.clone(), "Enter your prompt:".to_string(), "请输入分析图片的提示词...".to_string()).await {
        Ok(user_prompt) => {
            if !user_prompt.trim().is_empty() {
                println!("User provided prompt: {}", user_prompt);
                // 使用用户输入的prompt处理截图
                handle_screenshot_with_prompt(app_handle, user_prompt, output_mode).await;
            } else {
                println!("User cancelled or provided empty prompt");
            }
        }
        Err(e) => {
            println!("Failed to get user input: {}", e);
        }
    }
}

#[tokio::main]
async fn main() {
    let app_state = AppState::new();
    
    // Get initial hotkeys for plugin setup
    let (global_hotkey, switch_hotkey) = {
        let config = app_state.config.lock().await;
        println!("Loading global hotkey from config: {}", config.global_hotkey);
        println!("Loading switch hotkey from config: {}", config.switch_profile_hotkey);
        (config.global_hotkey.clone(), config.switch_profile_hotkey.clone())
    };

    println!("Registering global shortcuts: {} (global), {} (switch)", global_hotkey, switch_hotkey);

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .build(),
        )
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            update_config,
            // Profile管理API (保持前端兼容)
            create_profile,
            update_profile_config,
            delete_profile,
            set_active_profile,
            // 其他功能
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
            refresh_tray_menu,
            update_hotkey,
            update_hotkeys
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

            // Initialize hotkey registration
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    let config = state.config.lock().await;
                    let global_hotkey = config.global_hotkey.clone();
                    let switch_hotkey = config.switch_profile_hotkey.clone();
                    drop(config);
                    
                    println!("🔧 [DEBUG] Registering initial hotkeys: {} (global), {} (switch)", global_hotkey, switch_hotkey);
                    
                    // 使用内部热键注册函数，避免触发托盘菜单更新
                    if let Err(e) = register_hotkeys_internal(app_handle.clone(), global_hotkey, switch_hotkey).await {
                        eprintln!("❌ [ERROR] Failed to register initial hotkeys: {}", e);
                    } else {
                        println!("✅ [DEBUG] Initial hotkeys registered successfully");
                    }
                }
            });

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

            // Get active profile for display
            let active_profile = initial_config.profiles.iter()
                .find(|p| Some(&p.id) == initial_config.active_profile_id.as_ref())
                .cloned()
                .unwrap_or_else(|| initial_config.profiles.first().cloned().unwrap_or_else(|| {
                    // Create a default profile if none exists
                    Profile {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: "默认配置".to_string(),
                        api_config: ApiConfig {
                            base_url: "http://210.126.8.197:11434/v1".to_string(),
                            api_key: "".to_string(),
                            model: "".to_string(),
                        },
                        prompt_mode: PromptMode::Predefined("识别公式和文字，返回使用pandoc语法的markdown排版内容。公式请用katex语法包裹，文字内容不要丢失。只返回内容不需要其他解释。".to_string()),
                        output_mode: OutputMode::Clipboard,
                    }
                }));

            // Profile selection submenu
            let mut profile_submenu_builder = SubmenuBuilder::new(app, &format!("Profile: {}", active_profile.name));

            // Store Profile CheckMenuItem references for dynamic updates
            let mut profile_check_items_for_storage = std::collections::HashMap::new();

            // Add each profile as a CheckMenuItem
            for profile in &initial_config.profiles {
                let is_current = Some(&profile.id) == initial_config.active_profile_id.as_ref();

                println!("🔍 [DEBUG] Creating Profile CheckMenuItem for '{}', checked={}", profile.name, is_current);

                let profile_item = CheckMenuItemBuilder::new(&profile.name)
                    .id(&format!("profile_{}", profile.id))
                    .checked(is_current)
                    .build(app)?;

                // Store the CheckMenuItem reference
                profile_check_items_for_storage.insert(profile.id.clone(), profile_item.clone());
                println!("   📝 Stored Profile CheckMenuItem reference for '{}'", profile.name);

                profile_submenu_builder = profile_submenu_builder.item(&profile_item);
            }

            let profile_submenu = profile_submenu_builder.build()?;

            // Store the profile submenu reference for dynamic updates
            println!("🔄 [DEBUG] Storing profile submenu reference for title updates...");
            match app_state.profile_submenu.try_lock() {
                Ok(mut submenu_ref) => {
                    *submenu_ref = Some(profile_submenu.clone());
                    println!("✅ [DEBUG] Profile submenu reference stored successfully");
                }
                Err(e) => {
                    println!("❌ [DEBUG] Failed to store profile submenu reference: {}", e);
                }
            }

            // Store profile CheckMenuItem references
            println!("🔄 [DEBUG] Storing Profile CheckMenuItem references...");
            match app_state.profile_check_items.try_lock() {
                Ok(mut profile_check_items) => {
                    *profile_check_items = profile_check_items_for_storage;
                    println!("✅ [DEBUG] Profile CheckMenuItem references stored successfully");
                }
                Err(e) => {
                    println!("❌ [DEBUG] Failed to store Profile CheckMenuItem references: {}", e);
                }
            }

            // Model selection submenu - use active profile's model
            let model_display = if active_profile.api_config.model.is_empty() {
                "Not Selected"
            } else {
                &active_profile.api_config.model
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
                    let is_current = model_id == &active_profile.api_config.model;
                    
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

            // Hotkey display - show both global and switch hotkeys
            let formatted_global_hotkey = format_hotkey_for_display(&initial_config.global_hotkey);
            let formatted_switch_hotkey = format_hotkey_for_display(&initial_config.switch_profile_hotkey);

            let global_hotkey_item = MenuItemBuilder::new(&format!("Global: {}", formatted_global_hotkey))
                .id("global_hotkey_info")
                .enabled(false)
                .build(app)?;

            let switch_hotkey_item = MenuItemBuilder::new(&format!("Switch: {}", formatted_switch_hotkey))
                .id("switch_hotkey_info")
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
                .item(&profile_submenu)
                .item(&model_submenu)
                .item(&global_hotkey_item)
                .item(&switch_hotkey_item)
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

            // Before creating tray, store references to items we want to update dynamically
            {
                let app_state = app.state::<AppState>();
                if let Ok(mut g) = app_state.global_hotkey_item.try_lock() { *g = Some(global_hotkey_item.clone()); }
                if let Ok(mut s) = app_state.switch_hotkey_item.try_lock() { *s = Some(switch_hotkey_item.clone()); }
                if let Ok(mut snd) = app_state.sound_item.try_lock() { *snd = Some(sound_item.clone()); }
                if let Ok(mut p) = app_state.profile_submenu.try_lock() { *p = Some(profile_submenu.clone()); }
                if let Ok(mut m) = app_state.model_submenu.try_lock() { *m = Some(model_submenu.clone()); };
            }

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
