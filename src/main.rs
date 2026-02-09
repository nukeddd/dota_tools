use eframe::egui;
use std::fs;
use std::path::{Path, PathBuf};
use std::io::Write;
use regex::Regex;

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

struct DotaToolsApp {
    accounts: Vec<SteamAccount>,
    selected_source_idx: Option<usize>,
    selected_target_idx: Option<usize>,
    source_search: String,
    target_search: String,
    status_message: String,
    steam_userdata_path: Option<PathBuf>,
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
            loading: true,
        };

        if let Some(path) = &app.steam_userdata_path {
            app.accounts = get_accounts(path);
        } else {
            app.status_message = "Steam userdata not found!".to_string();
        }
        app.loading = false;

        app
    }
}

impl eframe::App for DotaToolsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
        viewport: egui::ViewportBuilder::default().with_inner_size([500.0, 400.0]),
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

fn get_accounts(userdata_path: &Path) -> Vec<SteamAccount> {
    let mut accounts = Vec::new();
    let cache_dir = dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")).join("dota_tools/avatars");
    fs::create_dir_all(&cache_dir).ok();

    if let Ok(entries) = fs::read_dir(userdata_path) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_dir() {
                    if let Ok(name) = entry.file_name().into_string() {
                        if name.chars().all(char::is_numeric) {
                            if name == "0" { continue; }

                            let (persona_name, avatar_url) = get_account_info(userdata_path, &name);

                            let mut avatar_path = None;
                            if let Some(url) = avatar_url {
                                if let Some(path) = download_avatar(&url, &name, &cache_dir) {
                                    avatar_path = Some(path);
                                }
                            }

                            accounts.push(SteamAccount {
                                id: name,
                                persona_name,
                                avatar_path,
                            });
                        }
                    }
                }
            }
        }
    }
    accounts.sort_by(|a, b| a.persona_name.to_lowercase().cmp(&b.persona_name.to_lowercase()));
    accounts
}

fn get_account_info(userdata_path: &Path, account_id: &str) -> (String, Option<String>) {
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

    let mut avatar_url = None;

    if let Ok(id32) = account_id.parse::<u64>() {
        let id64 = id32 + 76561197960265728;
        let url = format!("https://steamcommunity.com/profiles/{}?xml=1", id64);

        if let Ok(response) = reqwest::blocking::get(&url) {
            if let Ok(text) = response.text() {
                if persona_name.is_empty() {
                    let re_name = Regex::new(r"<steamID><!\[CDATA\[(.*?)\]\]></steamID>").unwrap();
                    if let Some(caps) = re_name.captures(&text) {
                        if let Some(name) = caps.get(1) {
                            persona_name = name.as_str().to_string();
                        }
                    }
                }

                let re_avatar = Regex::new(r"<avatarMedium><!\[CDATA\[(.*?)\]\]></avatarMedium>").unwrap();
                if let Some(caps) = re_avatar.captures(&text) {
                    if let Some(url) = caps.get(1) {
                        avatar_url = Some(url.as_str().to_string());
                    }
                }
            }
        }
    }

    if persona_name.is_empty() {
        persona_name = account_id.to_string();
    }

    (persona_name, avatar_url)
}

fn download_avatar(url: &str, account_id: &str, cache_dir: &Path) -> Option<PathBuf> {
    let file_name = format!("{}.jpg", account_id);
    let file_path = cache_dir.join(file_name);

    if file_path.exists() {
        return Some(file_path);
    }

    if let Ok(response) = reqwest::blocking::get(url) {
        if let Ok(bytes) = response.bytes() {
            if let Ok(mut file) = fs::File::create(&file_path) {
                if file.write_all(&bytes).is_ok() {
                    return Some(file_path);
                }
            }
        }
    }
    None
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
