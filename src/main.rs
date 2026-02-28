use eframe::egui;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinSet;

#[cfg(target_os = "windows")]
use winreg::enums::*;
#[cfg(target_os = "windows")]
use winreg::RegKey;

#[derive(Debug, Clone)]
struct SteamAccount {
    id: String,
    persona_name: String,
    avatar_path: Option<PathBuf>,
}

impl SteamAccount {
    fn display_name(&self) -> String {
        if self.persona_name.is_empty() {
            self.id.clone()
        } else {
            format!("{} ({})", self.persona_name, self.id)
        }
    }

    fn avatar_uri(&self) -> Option<String> {
        self.avatar_path.as_ref().map(|p| format!("file://{}", p.to_string_lossy()))
    }
}

#[derive(Debug)]
struct AccountUpdate {
    id: String,
    persona_name: Option<String>,
    avatar_path: Option<PathBuf>,
}

struct DotaToolsApp {
    accounts: Vec<SteamAccount>,
    selected_source_idx: Option<usize>,
    selected_target_idx: Option<usize>,
    source_search: String,
    target_search: String,
    status_message: String,
    steam_userdata_path: Option<PathBuf>,
    account_updates_rx: Option<mpsc::Receiver<AccountUpdate>>,
    loading: bool,
}

impl DotaToolsApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let mut app = Self {
            accounts: Vec::new(),
            selected_source_idx: None,
            selected_target_idx: None,
            source_search: String::new(),
            target_search: String::new(),
            status_message: "Ready".to_owned(),
            steam_userdata_path: find_steam_userdata(),
            account_updates_rx: None,
            loading: true,
        };

        if let Some(path) = &app.steam_userdata_path {
            app.accounts = get_accounts(path);
            if !app.accounts.is_empty() {
                app.spawn_account_updates();
            }
        } else {
            app.status_message = "Steam userdata not found!".to_string();
        }
        app.loading = false;

        app
    }

    fn spawn_account_updates(&mut self) {
        let accounts = self.accounts.clone();
        let (tx, rx) = mpsc::channel();
        self.account_updates_rx = Some(rx);

        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build();
            let Ok(rt) = rt else {
                return;
            };

            rt.block_on(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(8))
                    .build();
                let Ok(client) = client else {
                    return;
                };

                let cache_dir = avatar_cache_dir();
                let _ = tokio::fs::create_dir_all(&cache_dir).await;

                let mut join_set = JoinSet::new();
                for account in accounts {
                    let client = client.clone();
                    let cache_dir = cache_dir.clone();
                    join_set.spawn(async move {
                        fetch_account_update(account, client, cache_dir).await
                    });
                }

                while let Some(result) = join_set.join_next().await {
                    if let Ok(Some(update)) = result {
                        let _ = tx.send(update);
                    }
                }
            });
        });
    }

    fn apply_account_updates(&mut self, ctx: &egui::Context) {
        let mut updated = false;
        let mut disconnected = false;

        if let Some(rx) = self.account_updates_rx.as_ref() {
            loop {
                match rx.try_recv() {
                    Ok(update) => {
                        if let Some(account) = self.accounts.iter_mut().find(|a| a.id == update.id) {
                            if let Some(name) = update.persona_name {
                                account.persona_name = name;
                            }
                            if let Some(path) = update.avatar_path {
                                account.avatar_path = Some(path);
                            }
                            updated = true;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        if disconnected {
            self.account_updates_rx = None;
        }

        if updated {
            ctx.request_repaint();
        }

        if self.account_updates_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

impl eframe::App for DotaToolsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_account_updates(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Dota 2 Config Copier");
            ui.add_space(20.0);

            if self.loading {
                ui.spinner();
                ui.label("Loading accounts...");
                return;
            }

            if self.accounts.is_empty() {
                ui.label("No Steam accounts found.");
                return;
            }

            // Helper to render account selection row with custom popup
            let render_account_selector = |ui: &mut egui::Ui, label: &str, selected_idx: &mut Option<usize>, search_query: &mut String, id_salt: &str| {
                ui.vertical(|ui| {
                    ui.label(label);
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        // Avatar preview
                        if let Some(idx) = *selected_idx {
                            if let Some(uri) = self.accounts[idx].avatar_uri() {
                                ui.add(egui::Image::new(uri).fit_to_exact_size(egui::vec2(32.0, 32.0)).rounding(4.0));
                            } else {
                                ui.allocate_ui(egui::vec2(32.0, 32.0), |ui| { ui.label("?"); });
                            }
                        } else {
                             ui.allocate_ui(egui::vec2(32.0, 32.0), |_| {});
                        }

                        let selected_text = selected_idx
                            .map(|i| self.accounts[i].display_name())
                            .unwrap_or_else(|| "Select Account".to_string());

                        let popup_id = ui.make_persistent_id(id_salt);
                        let btn_response = ui.add(egui::Button::new(selected_text).min_size(egui::vec2(300.0, 0.0)));

                        if btn_response.clicked() {
                            ui.memory_mut(|m| m.toggle_popup(popup_id));
                        }

                        if ui.memory(|m| m.is_popup_open(popup_id)) {
                            let area = egui::Area::new(popup_id)
                                .order(egui::Order::Foreground)
                                .fixed_pos(btn_response.rect.left_bottom())
                                .constrain(true);

                            let area_response = area.show(ui.ctx(), |ui| {
                                egui::Frame::popup(ui.style()).show(ui, |ui| {
                                    ui.set_min_width(300.0);
                                    ui.add(egui::TextEdit::singleline(search_query).hint_text("Search..."));
                                    ui.separator();

                                    egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                                        let mut found_any = false;
                                        for (i, account) in self.accounts.iter().enumerate() {
                                            if !search_query.is_empty() && !account.display_name().to_lowercase().contains(&search_query.to_lowercase()) {
                                                continue;
                                            }
                                            found_any = true;

                                            ui.horizontal(|ui| {
                                                ui.set_height(24.0);
                                                if let Some(uri) = account.avatar_uri() {
                                                    ui.add(egui::Image::new(uri).fit_to_exact_size(egui::vec2(24.0, 24.0)).rounding(2.0));
                                                }

                                                if ui.selectable_value(selected_idx, Some(i), account.display_name()).clicked() {
                                                    ui.memory_mut(|m| m.close_popup());
                                                }
                                            });
                                        }
                                        if !found_any {
                                            ui.label("No results");
                                        }
                                    });
                                }).response
                            });

                            // Close popup if clicked outside
                            if ui.input(|i| i.pointer.any_pressed()) {
                                let pointer_pos = ui.input(|i| i.pointer.interact_pos());
                                if let Some(pos) = pointer_pos {
                                    if !btn_response.rect.contains(pos) && !area_response.inner.rect.contains(pos) {
                                        ui.memory_mut(|m| m.close_popup());
                                    }
                                }
                            }
                        }
                    });
                });
            };

            render_account_selector(ui, "Source Account", &mut self.selected_source_idx, &mut self.source_search, "source_combo");
            ui.add_space(20.0);
            render_account_selector(ui, "Target Account", &mut self.selected_target_idx, &mut self.target_search, "target_combo");

            ui.add_space(30.0);

            if ui.button("Copy Config").clicked() {
                if let (Some(src_idx), Some(target_idx)) = (self.selected_source_idx, self.selected_target_idx) {
                    if src_idx == target_idx {
                        self.status_message = "Source and Target are the same.".to_string();
                    } else {
                        let src = &self.accounts[src_idx];
                        let target = &self.accounts[target_idx];

                        if let Some(base_path) = &self.steam_userdata_path {
                            match copy_dota_config(base_path, &src.id, &target.id) {
                                Ok(_) => self.status_message = format!("Copied from {} to {}", src.persona_name, target.persona_name),
                                Err(e) => self.status_message = format!("Error: {}", e),
                            }
                        }
                    }
                } else {
                    self.status_message = "Please select both accounts.".to_string();
                }
            }

            ui.add_space(10.0);
            ui.label(&self.status_message);
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([500.0, 400.0])
            .with_app_id("dota_tools"),
        ..Default::default()
    };
    eframe::run_native(
        "Dota Tools",
        options,
        Box::new(|cc| Box::new(DotaToolsApp::new(cc))),
    )
}

fn find_steam_userdata() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        // Try to find Steam path from Registry
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        if let Ok(steam) = hklm.open_subkey("SOFTWARE\\Wow6432Node\\Valve\\Steam") {
            if let Ok(path_str) = steam.get_value::<String, _>("InstallPath") {
                let path = PathBuf::from(path_str).join("userdata");
                if path.exists() {
                    return Some(path);
                }
            }
        }

        // Fallback to standard paths
        let possible_paths = [
            PathBuf::from(r"C:\Program Files (x86)\Steam\userdata"),
            PathBuf::from(r"C:\Program Files\Steam\userdata"),
        ];

        for path in possible_paths {
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(home) = dirs::home_dir() {
            let possible_paths = [
                home.join(".steam/steam/userdata"),
                home.join(".local/share/Steam/userdata"),
                home.join(".steam/debian-installation/userdata"),
            ];

            for path in possible_paths {
                if path.exists() && path.is_dir() {
                    return Some(path);
                }
            }
        }
    }

    None
}

fn avatar_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("dota_tools/avatars")
}

fn cached_avatar_path(cache_dir: &Path, account_id: &str) -> Option<PathBuf> {
    let file_path = cache_dir.join(format!("{}.jpg", account_id));
    if file_path.exists() {
        Some(file_path)
    } else {
        None
    }
}

fn get_accounts(userdata_path: &Path) -> Vec<SteamAccount> {
    let mut accounts = Vec::new();
    let cache_dir = avatar_cache_dir();
    fs::create_dir_all(&cache_dir).ok();

    if let Ok(entries) = fs::read_dir(userdata_path) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_dir() {
                    if let Ok(name) = entry.file_name().into_string() {
                        if name.chars().all(char::is_numeric) {
                            if name == "0" { continue; }

                            let id = name;
                            let persona_name = get_account_info_local(userdata_path, &id);
                            let avatar_path = cached_avatar_path(&cache_dir, &id);

                            accounts.push(SteamAccount {
                                id,
                                persona_name,
                                avatar_path,
                            });
                        }
                    }
                }
            }
        }
    }
    accounts.sort_by(|a, b| {
        let a_key = if a.persona_name.is_empty() { &a.id } else { &a.persona_name };
        let b_key = if b.persona_name.is_empty() { &b.id } else { &b.persona_name };
        a_key.to_lowercase().cmp(&b_key.to_lowercase())
    });
    accounts
}

fn get_account_info_local(userdata_path: &Path, account_id: &str) -> String {
    let mut persona_name = String::new();

    let config_path = userdata_path.join(account_id).join("config/localconfig.vdf");
    if config_path.exists() {
        if let Ok(content) = fs::read_to_string(&config_path) {
            let re = Regex::new(r#""PersonaName"\s+"([^"]+)""#).unwrap();
            if let Some(caps) = re.captures(&content) {
                if let Some(name) = caps.get(1) {
                    persona_name = name.as_str().to_string();
                }
            }
        }
    }

    persona_name
}

struct RemoteAccountInfo {
    persona_name: Option<String>,
    avatar_url: Option<String>,
}

async fn fetch_remote_account_info(
    client: &reqwest::Client,
    account_id: &str,
) -> Option<RemoteAccountInfo> {
    let id32 = account_id.parse::<u64>().ok()?;
    let id64 = id32 + 76561197960265728;
    let url = format!("https://steamcommunity.com/profiles/{}?xml=1", id64);

    let response = client.get(url).send().await.ok()?;
    let response = response.error_for_status().ok()?;
    let text = response.text().await.ok()?;

    let re_name = Regex::new(r"<steamID><!\[CDATA\[(.*?)\]\]></steamID>").unwrap();
    let re_avatar = Regex::new(r"<avatarMedium><!\[CDATA\[(.*?)\]\]></avatarMedium>").unwrap();

    let persona_name = re_name
        .captures(&text)
        .and_then(|caps| caps.get(1))
        .map(|name| name.as_str().to_string());

    let avatar_url = re_avatar
        .captures(&text)
        .and_then(|caps| caps.get(1))
        .map(|url| url.as_str().to_string());

    Some(RemoteAccountInfo {
        persona_name,
        avatar_url,
    })
}

async fn download_avatar_async(
    client: &reqwest::Client,
    url: &str,
    account_id: &str,
    cache_dir: &Path,
) -> Option<PathBuf> {
    let file_name = format!("{}.jpg", account_id);
    let file_path = cache_dir.join(file_name);

    if file_path.exists() {
        return Some(file_path);
    }

    let response = client.get(url).send().await.ok()?;
    let response = response.error_for_status().ok()?;
    let bytes = response.bytes().await.ok()?;

    if let Ok(mut file) = tokio::fs::File::create(&file_path).await {
        if file.write_all(&bytes).await.is_ok() {
            return Some(file_path);
        }
    }

    None
}

async fn fetch_account_update(
    account: SteamAccount,
    client: reqwest::Client,
    cache_dir: PathBuf,
) -> Option<AccountUpdate> {
    let SteamAccount {
        id,
        persona_name,
        avatar_path,
    } = account;

    let needs_name = persona_name.is_empty();
    let needs_avatar = avatar_path.is_none();
    if !needs_name && !needs_avatar {
        return None;
    }

    let remote = fetch_remote_account_info(&client, &id).await?;

    let mut update = AccountUpdate {
        id,
        persona_name: None,
        avatar_path: None,
    };

    if needs_name {
        if let Some(name) = remote.persona_name {
            if !name.is_empty() {
                update.persona_name = Some(name);
            }
        }
    }

    if needs_avatar {
        if let Some(url) = remote.avatar_url {
            if let Some(path) = download_avatar_async(&client, &url, &update.id, &cache_dir).await {
                update.avatar_path = Some(path);
            }
        }
    }

    if update.persona_name.is_none() && update.avatar_path.is_none() {
        None
    } else {
        Some(update)
    }
}

fn copy_dota_config(base_path: &Path, src_id: &str, target_id: &str) -> anyhow::Result<()> {
    let src_path_remote = base_path.join(src_id).join("570/remote/cfg");
    let target_path_remote = base_path.join(target_id).join("570/remote/cfg");

    if !src_path_remote.exists() {
        return Err(anyhow::anyhow!("Source config directory not found: {:?}", src_path_remote));
    }

    if !target_path_remote.exists() {
        fs::create_dir_all(&target_path_remote)?;
    }

    for entry in fs::read_dir(src_path_remote)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name() {
                let target_file = target_path_remote.join(name);
                fs::copy(&path, &target_file)?;
            }
        }
    }

    Ok(())
}
