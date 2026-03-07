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

// ─── Conduct Stats ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct ConductStats {
    account_id:       String,
    behavior_score:   i32,
    behavior_rating:  String,
    commend_count:    i32,
    reports_count:    i32,
    matches_clean:    i32,
    matches_abandoned: i32,
    comms_reports:    i32,
    matches_in_report: i32,
}

impl ConductStats {
    fn rating_label(&self) -> &str {
        match self.behavior_rating.as_str() {
            "k_eBehaviorGood"    => "Good",
            "k_eBehaviorNeutral" => "Neutral",
            "k_eBehaviorBad"     => "Bad",
            "k_eBehaviorVerybad" => "Very Bad",
            s if !s.is_empty()   => s,
            _                    => "Unknown",
        }
    }

    fn rating_color(&self) -> egui::Color32 {
        match self.behavior_rating.as_str() {
            "k_eBehaviorGood"    => egui::Color32::from_rgb(72, 199, 116),
            "k_eBehaviorNeutral" => egui::Color32::from_rgb(220, 180, 50),
            "k_eBehaviorBad"     => egui::Color32::from_rgb(230, 80,  80),
            "k_eBehaviorVerybad" => egui::Color32::from_rgb(180, 30,  30),
            _                    => egui::Color32::from_rgb(140, 140, 140),
        }
    }

    /// Score bar fill  0.0–1.0  (max reasonable score ≈ 12 000)
    fn score_fraction(&self) -> f32 {
        (self.behavior_score as f32 / 12_000.0).clamp(0.0, 1.0)
    }
}

fn parse_conduct_stats(content: &str) -> Option<ConductStats> {
    let re = Regex::new(r"(\w+):\s*(\S+)").unwrap();
    let mut s = ConductStats::default();

    for caps in re.captures_iter(content) {
        let key   = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let value = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        match key {
            "account_id"        => s.account_id       = value.to_string(),
            "raw_behavior_score"=> s.behavior_score   = value.parse().unwrap_or(0),
            "behavior_rating"   => s.behavior_rating  = value.to_string(),
            "commend_count"     => s.commend_count    = value.parse().unwrap_or(0),
            "reports_count"     => s.reports_count    = value.parse().unwrap_or(0),
            "matches_clean"     => s.matches_clean    = value.parse().unwrap_or(0),
            "matches_abandoned" => s.matches_abandoned= value.parse().unwrap_or(0),
            "comms_reports"     => s.comms_reports    = value.parse().unwrap_or(0),
            "matches_in_report" => s.matches_in_report= value.parse().unwrap_or(0),
            _ => {}
        }
    }

    if s.account_id.is_empty() { None } else { Some(s) }
}

fn read_conduct_stats(dota_cfg: &Path, account_id: &str) -> Option<ConductStats> {
    let path = dota_cfg.join(format!("latest_conduct_1.{}.txt", account_id));
    let content = fs::read_to_string(&path).ok()?;
    parse_conduct_stats(&content)
}

/// Build the dota cfg sub-path relative to a Steam library root.
fn dota_cfg_in(library_root: &Path) -> PathBuf {
    library_root
        .join("steamapps")
        .join("common")
        .join("dota 2 beta")
        .join("game")
        .join("dota")
        .join("cfg")
}

/// Collect all Steam library roots we should search.
/// Priority order:
///  1. Steam install dir (from userdata parent or registry)
///  2. Extra libraries listed in libraryfolders.vdf
///  3. Common install sub-dirs on every available drive letter (Windows)
///     or common Linux/macOS paths
fn collect_steam_library_roots(userdata_path: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();

    // 1. Primary: parent of userdata
    if let Some(steam_root) = userdata_path.parent() {
        roots.push(steam_root.to_path_buf());

        // 2. libraryfolders.vdf next to steamapps
        let vdf_path = steam_root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(vdf) = fs::read_to_string(&vdf_path) {
            let re = Regex::new(r#""path"\s+"([^"]+)""#).unwrap();
            for caps in re.captures_iter(&vdf) {
                if let Some(m) = caps.get(1) {
                    let p = PathBuf::from(m.as_str());
                    if p.exists() && !roots.contains(&p) {
                        roots.push(p);
                    }
                }
            }
        }
    }

    // 3a. Windows – brute-force every drive letter
    #[cfg(target_os = "windows")]
    {
        // Common relative paths Steam is installed under on a given drive
        let steam_rel: &[&str] = &[
            "Steam",
            "Program Files (x86)\\Steam",
            "Program Files\\Steam",
            "Games\\Steam",
            "SteamLibrary",
            "Games\\SteamLibrary",
        ];

        for letter in b'A'..=b'Z' {
            let drive = format!("{}:\\", letter as char);
            let drive_path = PathBuf::from(&drive);
            if !drive_path.exists() {
                continue;
            }
            for rel in steam_rel {
                let candidate = drive_path.join(rel);
                if candidate.exists() && !roots.contains(&candidate) {
                    roots.push(candidate.clone());

                    // Also parse that library's libraryfolders.vdf
                    let vdf = candidate.join("steamapps").join("libraryfolders.vdf");
                    if let Ok(content) = fs::read_to_string(&vdf) {
                        let re = Regex::new(r#""path"\s+"([^"]+)""#).unwrap();
                        for caps in re.captures_iter(&content) {
                            if let Some(m) = caps.get(1) {
                                let p = PathBuf::from(m.as_str());
                                if p.exists() && !roots.contains(&p) {
                                    roots.push(p);
                                }
                            }
                        }
                    }
                }
            }

            // Also check bare "SteamLibrary" roots (games installed directly)
            let bare = drive_path.join("SteamLibrary");
            if bare.exists() && !roots.contains(&bare) {
                roots.push(bare);
            }
        }
    }

    // 3b. Linux / macOS – common extra paths
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(home) = dirs::home_dir() {
            let extras = [
                home.join(".steam/steam"),
                home.join(".local/share/Steam"),
                PathBuf::from("/mnt/games/Steam"),
                PathBuf::from("/media/games/Steam"),
            ];
            for p in extras {
                if p.exists() && !roots.contains(&p) {
                    roots.push(p);
                }
            }
        }
    }

    roots
}

/// Locate  <steam_library>/steamapps/common/dota 2 beta/game/dota/cfg
/// Searches every Steam library root, including all drive letters on Windows.
fn find_dota_cfg_path(userdata_path: &Path) -> Option<PathBuf> {
    for root in collect_steam_library_roots(userdata_path) {
        let cfg = dota_cfg_in(&root);
        if cfg.exists() {
            return Some(cfg);
        }
    }
    None
}

// ─── Steam Account ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SteamAccount {
    id:           String,
    persona_name: String,
    avatar_path:  Option<PathBuf>,
    conduct:      Option<ConductStats>,
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
        self.avatar_path
            .as_ref()
            .map(|p| format!("file://{}", p.to_string_lossy()))
    }
}

#[derive(Debug)]
struct AccountUpdate {
    id:           String,
    persona_name: Option<String>,
    avatar_path:  Option<PathBuf>,
}

// ─── App State ────────────────────────────────────────────────────────────────

struct DotaToolsApp {
    accounts:            Vec<SteamAccount>,
    selected_source_idx: Option<usize>,
    selected_target_idx: Option<usize>,
    source_search:       String,
    target_search:       String,
    status_message:      String,
    steam_userdata_path: Option<PathBuf>,
    dota_cfg_path:       Option<PathBuf>,
    account_updates_rx:  Option<mpsc::Receiver<AccountUpdate>>,
    loading:             bool,
}

impl DotaToolsApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);

        // Dark theme tweaks
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals.window_rounding   = egui::Rounding::same(8.0);
        style.visuals.widgets.noninteractive.rounding = egui::Rounding::same(6.0);
        style.visuals.widgets.inactive.rounding       = egui::Rounding::same(6.0);
        style.visuals.widgets.active.rounding         = egui::Rounding::same(6.0);
        style.visuals.widgets.hovered.rounding        = egui::Rounding::same(6.0);
        cc.egui_ctx.set_style(style);

        let steam_userdata_path = find_steam_userdata();
        let dota_cfg_path = steam_userdata_path
            .as_deref()
            .and_then(find_dota_cfg_path);

        let mut app = Self {
            accounts:            Vec::new(),
            selected_source_idx: None,
            selected_target_idx: None,
            source_search:       String::new(),
            target_search:       String::new(),
            status_message:      "Ready".to_owned(),
            steam_userdata_path,
            dota_cfg_path,
            account_updates_rx:  None,
            loading:             true,
        };

        if let Some(path) = &app.steam_userdata_path.clone() {
            app.accounts = get_accounts(path, app.dota_cfg_path.as_deref());
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
            let Ok(rt) = rt else { return };

            rt.block_on(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(8))
                    .build();
                let Ok(client) = client else { return };

                let cache_dir = avatar_cache_dir();
                let _ = tokio::fs::create_dir_all(&cache_dir).await;

                let mut join_set = JoinSet::new();
                for account in accounts {
                    let client    = client.clone();
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
        let mut updated     = false;
        let mut disconnected = false;

        if let Some(rx) = self.account_updates_rx.as_ref() {
            loop {
                match rx.try_recv() {
                    Ok(update) => {
                        if let Some(account) =
                            self.accounts.iter_mut().find(|a| a.id == update.id)
                        {
                            if let Some(name) = update.persona_name {
                                account.persona_name = name;
                            }
                            if let Some(path) = update.avatar_path {
                                account.avatar_path = Some(path);
                            }
                            updated = true;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty)        => break,
                    Err(mpsc::TryRecvError::Disconnected) => { disconnected = true; break }
                }
            }
        }

        if disconnected { self.account_updates_rx = None; }
        if updated      { ctx.request_repaint(); }
        if self.account_updates_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

// ─── UI ───────────────────────────────────────────────────────────────────────

/// Draw the conduct stats card for a selected account.
fn render_conduct_card(ui: &mut egui::Ui, conduct: &ConductStats) {
    let bg = egui::Color32::from_rgba_premultiplied(30, 30, 40, 240);
    egui::Frame::none()
        .fill(bg)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(12.0))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 80)))
        .show(ui, |ui| {
            ui.set_min_width(220.0);

            // ── Behavior rating badge ──────────────────────────────────────
            let badge_color = conduct.rating_color();
            let label       = conduct.rating_label();

            ui.horizontal(|ui| {
                egui::Frame::none()
                    .fill(badge_color)
                    .rounding(egui::Rounding::same(4.0))
                    .inner_margin(egui::Margin::symmetric(8.0, 3.0))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(egui::Color32::BLACK)
                                .strong()
                                .size(13.0),
                        );
                    });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!("{}", conduct.behavior_score))
                        .size(13.0)
                        .color(egui::Color32::from_rgb(200, 200, 220)),
                );
            });

            ui.add_space(8.0);

            // ── Score progress bar ─────────────────────────────────────────
            let bar_w   = ui.available_width();
            let bar_h   = 6.0;
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(bar_w, bar_h),
                egui::Sense::hover(),
            );
            let painter = ui.painter();
            let bg_rect = rect;
            painter.rect_filled(
                bg_rect,
                egui::Rounding::same(3.0),
                egui::Color32::from_rgb(50, 50, 70),
            );
            let fill_w = bg_rect.width() * conduct.score_fraction();
            if fill_w > 0.0 {
                let fill_rect = egui::Rect::from_min_size(
                    bg_rect.min,
                    egui::vec2(fill_w, bar_h),
                );
                painter.rect_filled(
                    fill_rect,
                    egui::Rounding::same(3.0),
                    badge_color,
                );
            }

            ui.add_space(8.0);

            // ── Stats grid ────────────────────────────────────────────────
            let dim = egui::Color32::from_rgb(140, 140, 160);
            let val = egui::Color32::from_rgb(220, 220, 235);

            let stat = |ui: &mut egui::Ui, icon: &str, label: &str, n: i32, color: egui::Color32| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(icon).size(13.0));
                    ui.label(egui::RichText::new(label).size(12.0).color(dim));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(format!("{}", n)).size(12.0).color(color).strong());
                    });
                });
            };

            stat(ui, "★", "Commends",  conduct.commend_count,     egui::Color32::from_rgb(255, 210, 60));
            stat(ui, "⚑", "Reports",   conduct.reports_count,     if conduct.reports_count > 0 { egui::Color32::from_rgb(230,80,80) } else { val });
            stat(ui, "✓", "Clean",     conduct.matches_clean,     egui::Color32::from_rgb(72, 199, 116));
            stat(ui, "✗", "Abandoned", conduct.matches_abandoned, if conduct.matches_abandoned > 0 { egui::Color32::from_rgb(230,80,80) } else { val });
            stat(ui, "✉", "Comms rep.",conduct.comms_reports,     if conduct.comms_reports > 0 { egui::Color32::from_rgb(230,130,50) } else { val });
        });
}

/// Dropdown account selector. Returns whether selection changed.
fn render_account_selector(
    ui:           &mut egui::Ui,
    accounts:     &[SteamAccount],
    selected_idx: &mut Option<usize>,
    search_query: &mut String,
    id_salt:      &str,
) {
    ui.horizontal(|ui| {
        // Avatar
        if let Some(idx) = *selected_idx {
            if let Some(uri) = accounts[idx].avatar_uri() {
                ui.add(
                    egui::Image::new(uri)
                        .fit_to_exact_size(egui::vec2(36.0, 36.0))
                        .rounding(5.0),
                );
            } else {
                ui.allocate_ui(egui::vec2(36.0, 36.0), |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(egui::RichText::new("?").size(20.0));
                    });
                });
            }
        } else {
            ui.allocate_ui(egui::vec2(36.0, 36.0), |_| {});
        }

        let selected_text = selected_idx
            .map(|i| accounts[i].display_name())
            .unwrap_or_else(|| "Select account…".to_string());

        let popup_id     = ui.make_persistent_id(id_salt);
        let btn_response = ui.add(
            egui::Button::new(
                egui::RichText::new(&selected_text).size(13.0),
            )
            .min_size(egui::vec2(200.0, 32.0)),
        );

        if btn_response.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }

        if ui.memory(|m| m.is_popup_open(popup_id)) {
            let area = egui::Area::new(popup_id)
                .order(egui::Order::Foreground)
                .fixed_pos(btn_response.rect.left_bottom())
                .constrain(true);

            let area_resp = area.show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(260.0);
                    ui.add(
                        egui::TextEdit::singleline(search_query)
                            .hint_text("🔍  Search…")
                            .desired_width(f32::INFINITY),
                    );
                    ui.separator();

                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        let mut found = false;
                        for (i, account) in accounts.iter().enumerate() {
                            if !search_query.is_empty()
                                && !account
                                    .display_name()
                                    .to_lowercase()
                                    .contains(&search_query.to_lowercase())
                            {
                                continue;
                            }
                            found = true;

                            ui.horizontal(|ui| {
                                ui.set_height(28.0);
                                if let Some(uri) = account.avatar_uri() {
                                    ui.add(
                                        egui::Image::new(uri)
                                            .fit_to_exact_size(egui::vec2(22.0, 22.0))
                                            .rounding(3.0),
                                    );
                                }
                                // Conduct badge in dropdown
                                if let Some(c) = &account.conduct {
                                    egui::Frame::none()
                                        .fill(c.rating_color())
                                        .rounding(egui::Rounding::same(3.0))
                                        .inner_margin(egui::Margin::symmetric(5.0, 1.0))
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(c.rating_label())
                                                    .size(10.0)
                                                    .color(egui::Color32::BLACK),
                                            );
                                        });
                                }
                                if ui
                                    .selectable_value(selected_idx, Some(i), account.display_name())
                                    .clicked()
                                {
                                    ui.memory_mut(|m| m.close_popup());
                                }
                            });
                        }
                        if !found {
                            ui.label(
                                egui::RichText::new("No results")
                                    .color(egui::Color32::GRAY)
                                    .italics(),
                            );
                        }
                    });
                })
            });

            if ui.input(|i| i.pointer.any_pressed()) {
                if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                    if !btn_response.rect.contains(pos)
                        && !area_resp.inner.response.rect.contains(pos)
                    {
                        ui.memory_mut(|m| m.close_popup());
                    }
                }
            }
        }
    });
}

impl eframe::App for DotaToolsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_account_updates(ctx);

        // ── Top bar ───────────────────────────────────────────────────────
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(20, 22, 30))
                    .inner_margin(egui::Margin::symmetric(16.0, 12.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("⚙  Dota 2 Config Copier")
                            .size(18.0)
                            .strong()
                            .color(egui::Color32::from_rgb(200, 180, 255)),
                    );
                    if self.account_updates_rx.is_some() {
                        ui.add_space(12.0);
                        ui.spinner();
                        ui.label(
                            egui::RichText::new("Fetching profiles…")
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        );
                    }
                    if self.dota_cfg_path.is_none() {
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new("⚠ Dota 2 not found – conduct stats unavailable")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(220, 150, 50)),
                        );
                    }
                });
            });

        // ── Bottom panel: Copy button + status ───────────────────────────
        let status_msg = self.status_message.clone();
        egui::TopBottomPanel::bottom("bottombar")
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(20, 22, 30))
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    let btn = egui::Button::new(
                        egui::RichText::new("  Copy Config  →  ").size(14.0).strong(),
                    )
                    .min_size(egui::vec2(180.0, 36.0))
                    .fill(egui::Color32::from_rgb(80, 60, 180));

                    if ui.add(btn).clicked() {
                        match (self.selected_source_idx, self.selected_target_idx) {
                            (Some(si), Some(ti)) if si == ti => {
                                self.status_message = "⚠ Source and Target are the same account.".into();
                            }
                            (Some(si), Some(ti)) => {
                                let src_id   = self.accounts[si].id.clone();
                                let tgt_id   = self.accounts[ti].id.clone();
                                let src_name = self.accounts[si].persona_name.clone();
                                let tgt_name = self.accounts[ti].persona_name.clone();
                                if let Some(base) = &self.steam_userdata_path {
                                    match copy_dota_config(base, &src_id, &tgt_id) {
                                        Ok(_)  => self.status_message = format!("✓ Copied from {} → {}", src_name, tgt_name),
                                        Err(e) => self.status_message = format!("✗ Error: {}", e),
                                    }
                                }
                            }
                            _ => {
                                self.status_message = "Please select both Source and Target accounts.".into();
                            }
                        }
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(&status_msg)
                        .size(12.0)
                        .color(egui::Color32::from_rgb(160, 160, 180)),
                );
            });

        // ── Central panel ─────────────────────────────────────────────────
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(26, 28, 38))
                    .inner_margin(egui::Margin::same(16.0)),
            )
            .show(ctx, |ui| {
                if self.loading {
                    ui.centered_and_justified(|ui| {
                        ui.spinner();
                        ui.label("Loading accounts…");
                    });
                    return;
                }

                if self.accounts.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new("No Steam accounts found.")
                                .color(egui::Color32::GRAY),
                        );
                    });
                    return;
                }

                // ── Two-column layout: SOURCE | TARGET ────────────────────
                ui.columns(2, |cols| {
                    // ── LEFT: SOURCE ──────────────────────────────────────
                    {
                        let ui = &mut cols[0];
                        section_header(ui, "SOURCE", egui::Color32::from_rgb(100, 160, 255));
                        ui.add_space(8.0);

                        render_account_selector(
                            ui,
                            &self.accounts,
                            &mut self.selected_source_idx,
                            &mut self.source_search,
                            "source_combo",
                        );

                        ui.add_space(12.0);

                        if let Some(idx) = self.selected_source_idx {
                            if let Some(conduct) = &self.accounts[idx].conduct.clone() {
                                render_conduct_card(ui, conduct);
                            } else {
                                no_conduct_notice(ui);
                            }
                        } else {
                            placeholder_card(ui);
                        }
                    }

                    // ── RIGHT: TARGET ─────────────────────────────────────
                    {
                        let ui = &mut cols[1];
                        section_header(ui, "TARGET", egui::Color32::from_rgb(255, 140, 100));
                        ui.add_space(8.0);

                        render_account_selector(
                            ui,
                            &self.accounts,
                            &mut self.selected_target_idx,
                            &mut self.target_search,
                            "target_combo",
                        );

                        ui.add_space(12.0);

                        if let Some(idx) = self.selected_target_idx {
                            if let Some(conduct) = &self.accounts[idx].conduct.clone() {
                                render_conduct_card(ui, conduct);
                            } else {
                                no_conduct_notice(ui);
                            }
                        } else {
                            placeholder_card(ui);
                        }
                    }
                });

            });
    }
}

// ─── Small UI helpers ─────────────────────────────────────────────────────────

fn section_header(ui: &mut egui::Ui, label: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(label)
                .size(15.0)
                .strong()
                .color(color),
        );
    });
    ui.add(egui::Separator::default().spacing(4.0));
}

fn placeholder_card(ui: &mut egui::Ui) {
    egui::Frame::none()
        .fill(egui::Color32::from_rgba_premultiplied(30, 30, 40, 200))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(12.0))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 70)))
        .show(ui, |ui| {
            ui.set_min_width(220.0);
            ui.set_min_height(80.0);
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("No account selected")
                        .color(egui::Color32::from_rgb(80, 80, 100))
                        .italics(),
                );
            });
        });
}

fn no_conduct_notice(ui: &mut egui::Ui) {
    egui::Frame::none()
        .fill(egui::Color32::from_rgba_premultiplied(30, 30, 40, 200))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(12.0))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 50, 30)))
        .show(ui, |ui| {
            ui.set_min_width(220.0);
            ui.label(
                egui::RichText::new("⚠  Conduct file not found")
                    .color(egui::Color32::from_rgb(200, 150, 50))
                    .size(12.0),
            );
            ui.label(
                egui::RichText::new("Launch Dota 2 once to generate it.")
                    .color(egui::Color32::from_rgb(120, 110, 80))
                    .size(11.0)
                    .italics(),
            );
        });
}

// ─── Account / path helpers ───────────────────────────────────────────────────

fn get_accounts(userdata_path: &Path, dota_cfg: Option<&Path>) -> Vec<SteamAccount> {
    let mut accounts = Vec::new();
    let cache_dir = avatar_cache_dir();
    fs::create_dir_all(&cache_dir).ok();

    if let Ok(entries) = fs::read_dir(userdata_path) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    if let Ok(name) = entry.file_name().into_string() {
                        if name.chars().all(char::is_numeric) && name != "0" {
                            let id           = name;
                            let persona_name = get_account_info_local(userdata_path, &id);
                            let avatar_path  = cached_avatar_path(&cache_dir, &id);
                            let conduct      = dota_cfg.and_then(|p| read_conduct_stats(p, &id));

                            accounts.push(SteamAccount { id, persona_name, avatar_path, conduct });
                        }
                    }
                }
            }
        }
    }

    accounts.sort_by(|a, b| {
        let ak = if a.persona_name.is_empty() { &a.id } else { &a.persona_name };
        let bk = if b.persona_name.is_empty() { &b.id } else { &b.persona_name };
        ak.to_lowercase().cmp(&bk.to_lowercase())
    });
    accounts
}

fn get_account_info_local(userdata_path: &Path, account_id: &str) -> String {
    let config_path = userdata_path
        .join(account_id)
        .join("config/localconfig.vdf");
    if let Ok(content) = fs::read_to_string(&config_path) {
        let re = Regex::new(r#""PersonaName"\s+"([^"]+)""#).unwrap();
        if let Some(caps) = re.captures(&content) {
            if let Some(name) = caps.get(1) {
                return name.as_str().to_string();
            }
        }
    }
    String::new()
}

fn find_steam_userdata() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        if let Ok(steam) = hklm.open_subkey("SOFTWARE\\Wow6432Node\\Valve\\Steam") {
            if let Ok(path_str) = steam.get_value::<String, _>("InstallPath") {
                let path = PathBuf::from(path_str).join("userdata");
                if path.exists() {
                    return Some(path);
                }
            }
        }

        let possible = [
            PathBuf::from(r"C:\Program Files (x86)\Steam\userdata"),
            PathBuf::from(r"C:\Program Files\Steam\userdata"),
        ];
        for p in possible {
            if p.exists() { return Some(p); }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(home) = dirs::home_dir() {
            let possible = [
                home.join(".steam/steam/userdata"),
                home.join(".local/share/Steam/userdata"),
                home.join(".steam/debian-installation/userdata"),
            ];
            for p in possible {
                if p.exists() && p.is_dir() { return Some(p); }
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
    let p = cache_dir.join(format!("{}.jpg", account_id));
    if p.exists() { Some(p) } else { None }
}

// ─── Remote fetch helpers ─────────────────────────────────────────────────────

struct RemoteAccountInfo {
    persona_name: Option<String>,
    avatar_url:   Option<String>,
}

async fn fetch_remote_account_info(
    client:     &reqwest::Client,
    account_id: &str,
) -> Option<RemoteAccountInfo> {
    let id32 = account_id.parse::<u64>().ok()?;
    let id64 = id32 + 76_561_197_960_265_728;
    let url  = format!("https://steamcommunity.com/profiles/{}?xml=1", id64);

    let text = client.get(url).send().await.ok()?
        .error_for_status().ok()?
        .text().await.ok()?;

    let re_name   = Regex::new(r"<steamID><!\[CDATA\[(.*?)\]\]></steamID>").unwrap();
    let re_avatar = Regex::new(r"<avatarMedium><!\[CDATA\[(.*?)\]\]></avatarMedium>").unwrap();

    Some(RemoteAccountInfo {
        persona_name: re_name.captures(&text).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()),
        avatar_url:   re_avatar.captures(&text).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()),
    })
}

async fn download_avatar_async(
    client:     &reqwest::Client,
    url:        &str,
    account_id: &str,
    cache_dir:  &Path,
) -> Option<PathBuf> {
    let file_path = cache_dir.join(format!("{}.jpg", account_id));
    if file_path.exists() { return Some(file_path); }

    let bytes = client.get(url).send().await.ok()?
        .error_for_status().ok()?
        .bytes().await.ok()?;

    if let Ok(mut f) = tokio::fs::File::create(&file_path).await {
        if f.write_all(&bytes).await.is_ok() {
            return Some(file_path);
        }
    }
    None
}

async fn fetch_account_update(
    account:   SteamAccount,
    client:    reqwest::Client,
    cache_dir: PathBuf,
) -> Option<AccountUpdate> {
    let needs_name   = account.persona_name.is_empty();
    let needs_avatar = account.avatar_path.is_none();
    if !needs_name && !needs_avatar { return None; }

    let remote = fetch_remote_account_info(&client, &account.id).await?;
    let mut update = AccountUpdate { id: account.id.clone(), persona_name: None, avatar_path: None };

    if needs_name {
        if let Some(name) = remote.persona_name {
            if !name.is_empty() { update.persona_name = Some(name); }
        }
    }
    if needs_avatar {
        if let Some(url) = remote.avatar_url {
            update.avatar_path = download_avatar_async(&client, &url, &account.id, &cache_dir).await;
        }
    }

    if update.persona_name.is_none() && update.avatar_path.is_none() { None } else { Some(update) }
}

// ─── Config copy ──────────────────────────────────────────────────────────────

fn copy_dota_config(base_path: &Path, src_id: &str, target_id: &str) -> anyhow::Result<()> {
    let src    = base_path.join(src_id).join("570/remote/cfg");
    let target = base_path.join(target_id).join("570/remote/cfg");

    if !src.exists() {
        return Err(anyhow::anyhow!("Source config directory not found: {:?}", src));
    }
    if !target.exists() {
        fs::create_dir_all(&target)?;
    }

    for entry in fs::read_dir(&src)? {
        let entry = entry?;
        let path  = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name() {
                fs::copy(&path, target.join(name))?;
            }
        }
    }
    Ok(())
}

// ─── Entry point ─────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 560.0])
            .with_min_inner_size([600.0, 500.0])
            .with_app_id("dota_tools"),
        ..Default::default()
    };
    eframe::run_native(
        "Dota Tools",
        options,
        Box::new(|cc| Box::new(DotaToolsApp::new(cc))),
    )
}
