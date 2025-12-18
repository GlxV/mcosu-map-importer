mod app_state;
mod cache;
mod concurrency;
mod audio;
mod preview;
mod importer;
mod osu_parser;
mod osz_reader;
mod path_utils;
mod watcher;

use arboard::Clipboard;
use audio::AudioPlayer;
use anyhow::Context;
use std::collections::HashMap;
use std::fs::{self, create_dir_all, OpenOptions};
use std::env;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use urlencoding::encode;
use serde::{de, Deserialize, Deserializer};

use app_state::{AppConfig, AudioPreviewStatus, BeatmapEntry, ImportStatus};
use cache::{CacheStore, load_config, save_config};
use concurrency::ImportGuards;
use path_utils::{
    can_delete_source, downloads_songs_conflict, is_within_dir, validate_songs_choice,
};
use slint::{Color, SharedString};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

slint::include_modules!();

#[derive(Debug)]
enum CommandMsg {
    AddFile(PathBuf),
    ManualImport(u64, bool),
    ImportAll,
    ClearCompleted,
    UpdateConfig(AppConfig),
    AddFileDialog,
    OpenSource(u64),
    OpenDestination(u64),
    OpenBrowser(u64),
    SearchBeatmaps(String),
    DownloadBeatmap(u64),
    CopyLogs,
    DeleteSource(u64),
    Ignore(u64),
    ToggleAutoDelete(bool),
    ConfirmAutoDelete(bool),
    CancelAutoDeletePrompt,
    ShowErrorDetail(u64),
    PreviewAudio(u64),
    PreviewMap(u64),
}

#[derive(Debug, Clone, Copy)]
enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug)]
enum UiMsg {
    Upsert(BeatmapEntry),
    Log(LogLevel, String),
    ConfigChanged(AppConfig, Option<String>),
    ReplaceAll(Vec<BeatmapEntry>),
    BeatmapSearchState { loading: bool, message: Option<String> },
    BeatmapResults(Vec<BeatmapSearchResult>),
    BeatmapDownloadStatus { active: bool, text: Option<String> },
    ShowAutoDeletePrompt,
    HideAutoDeletePrompt,
    BulkRunning(bool),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BeatmapSource {
    Catboy,
    Nerinyan,
}

#[derive(Clone, Debug)]
struct BeatmapSearchResult {
    id: u64,
    title: String,
    artist: String,
    creator: String,
    source: BeatmapSource,
    download_url: String,
}

#[derive(Clone, Debug)]
struct BeatmapFound {
    title: String,
    artist: String,
    creator: String,
    source: BeatmapSource,
    download_url: String,
}

#[derive(Deserialize, Debug)]
struct CatboyApiResponse {
    #[serde(default)]
    results: Vec<CatboyBeatmap>,
}

#[derive(Deserialize, Debug)]
struct CatboyBeatmap {
    #[serde(rename = "SetID")]
    set_id: u64,
    #[serde(rename = "Title")]
    title: String,
    #[serde(rename = "Artist")]
    artist: String,
    #[serde(rename = "Creator")]
    creator: String,
}

#[derive(Deserialize, Debug)]
struct NerinyanBeatmap {
    #[serde(rename = "id", deserialize_with = "deserialize_flexible_id")]
    set_id: u64,
    #[serde(rename = "artist")]
    artist: String,
    #[serde(rename = "title")]
    title: String,
    #[serde(rename = "creator")]
    creator: String,
    #[serde(rename = "mode")]
    mode: Option<u8>,
}

fn deserialize_flexible_id<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    struct FlexibleIdVisitor;

    impl<'de> de::Visitor<'de> for FlexibleIdVisitor {
        type Value = u64;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("um número ou uma string que possa ser um número")
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value.parse().unwrap_or(0))
        }
    }

    deserializer.deserialize_any(FlexibleIdVisitor)
}

fn load_startup_config() -> AppConfig {
    let mut cfg = load_config();
    if cfg.auto_import {
        cfg.auto_import = false;
        let _ = save_config(&cfg);
    }
    enforce_path_safety(&mut cfg);
    cfg
}

fn enforce_path_safety(cfg: &mut AppConfig) -> Option<String> {
    let warning = downloads_songs_conflict(&cfg.downloads_dir, &cfg.songs_dir);
    if warning.is_some() {
        cfg.auto_import = false;
        cfg.auto_delete_source = false;
    }
    warning
}

fn main() -> anyhow::Result<()> {
    app_state::ensure_dir(&cache::base_dir())?;
    app_state::ensure_dir(&cache::cache_dir())?;
    app_state::ensure_dir(&cache::logs_dir())?;
    app_state::ensure_dir(&cache::thumbnails_dir())?;
    app_state::ensure_dir(&cache::audio_cache_dir())?;
    app_state::ensure_dir(&cache::preview_dir())?;

    let log_dir = Path::new("logs");
    if !log_dir.exists() {
        create_dir_all(log_dir)?;
    }

    let file_appender = tracing_appender::rolling::never(cache::logs_dir(), "app.log");
    let (nb_writer, _log_guard) = tracing_appender::non_blocking(file_appender);
    let console_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stdout);
    let file_layer = tracing_subscriber::fmt::layer().with_writer(nb_writer);

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(console_layer)
        .with(file_layer)
        .init();

    let app = AppWindow::new()?;
    let mut config = load_startup_config();
    let cache_store = Arc::new(CacheStore::load());
    let guards = Arc::new(ImportGuards::default());
    let initial_warning = enforce_path_safety(&mut config);
    let _ = save_config(&config);
    let shared_config: Arc<Mutex<AppConfig>> = Arc::new(Mutex::new(config.clone()));

    let beatmap_entries: Arc<Mutex<HashMap<u64, BeatmapEntry>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let ui_state_entries = Arc::new(Mutex::new(Vec::<BeatmapEntry>::new()));
    let log_state = Arc::new(Mutex::new(Vec::<(LogLevel, String)>::new()));
    let search_results_state: Arc<Mutex<HashMap<u64, BeatmapSearchResult>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let (cmd_tx, cmd_rx) = mpsc::channel::<CommandMsg>();
    let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();

    seed_existing_osz(&config.downloads_dir, &cmd_tx)?;

    // Start watcher
    {
        let tx = cmd_tx.clone();
        let dir = config.downloads_dir.clone();
        watcher::start_watcher(dir, move |path| {
            let _ = tx.send(CommandMsg::AddFile(path));
        })?;
    }

    // UI wiring
    app.set_download_path(SharedString::from(
        config.downloads_dir.display().to_string(),
    ));
    app.set_songs_path(SharedString::from(config.songs_dir.display().to_string()));
    app.set_auto_import(config.auto_import);
    app.set_auto_delete_after_import(config.auto_delete_source);
    app.set_show_completed(true);
    app.set_paths_blocked(initial_warning.is_some());
    app.set_bulk_import_running(false);
    app.set_auto_delete_prompt_visible(false);
    app.set_auto_delete_prompt_skip(false);
    app.set_active_tab(0);
    app.set_beatmap_query(SharedString::default());
    app.set_beatmap_loading(false);
    app.set_beatmap_downloading(false);
    app.set_beatmap_status(SharedString::default());
    app.set_beatmap_message(SharedString::default());
    app.set_beatmap_results(slint::ModelRc::new(Rc::new(slint::VecModel::default())));
    app.set_path_warning(SharedString::from(
        initial_warning.clone().unwrap_or_default(),
    ));
    {
        let _ = ui_tx.send(UiMsg::ConfigChanged(
            config.clone(),
            initial_warning.clone(),
        ));
        if let Some(warn) = initial_warning.clone() {
            let _ = ui_tx.send(UiMsg::Log(
                LogLevel::Warn,
                format!(
                    "Aviso de configuracao: {warn} Importar ja, auto-import e exclusao da fonte estao bloqueados."
                ),
            ));
        } else {
            let _ = ui_tx.send(UiMsg::Log(
                LogLevel::Info,
                "Auto-import inicia desligado; clique em Importar ja ou ligue o toggle".into(),
            ));
        }
    }

    app.on_pick_download({
        let tx = cmd_tx.clone();
        move || {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                let mut cfg = load_config();
                if let Err(msg) = validate_songs_choice(&path, &cfg.songs_dir) {
                    rfd::MessageDialog::new()
                        .set_title("Caminho inseguro")
                        .set_description(&msg)
                        .set_buttons(rfd::MessageButtons::Ok)
                        .show();
                    return;
                }
                cfg.downloads_dir = path.clone();
                let _ = save_config(&cfg);
                let _ = tx.send(CommandMsg::UpdateConfig(cfg));
            }
        }
    });
    app.on_pick_songs({
        let tx = cmd_tx.clone();
        move || {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                let mut cfg = load_config();
                if let Err(msg) = validate_songs_choice(&cfg.downloads_dir, &path) {
                    rfd::MessageDialog::new()
                        .set_title("Caminho inseguro")
                        .set_description(&msg)
                        .set_buttons(rfd::MessageButtons::Ok)
                        .show();
                    return;
                }
                cfg.songs_dir = path.clone();
                let _ = save_config(&cfg);
                let _ = tx.send(CommandMsg::UpdateConfig(cfg));
            }
        }
    });
    app.on_toggle_auto({
        let tx = cmd_tx.clone();
        move |state| {
            let mut cfg = load_config();
            cfg.auto_import = state;
            let _ = save_config(&cfg);
            let _ = tx.send(CommandMsg::UpdateConfig(cfg));
        }
    });
    app.on_import_all({
        let tx = cmd_tx.clone();
        move || {
            let _ = tx.send(CommandMsg::ImportAll);
        }
    });
    app.on_clear_completed({
        let tx = cmd_tx.clone();
        move || {
            let _ = tx.send(CommandMsg::ClearCompleted);
        }
    });
    app.on_import_now({
        let tx = cmd_tx.clone();
        move |id| {
            let _ = tx.send(CommandMsg::ManualImport(id as u64, false));
        }
    });
    app.on_reimport_now({
        let tx = cmd_tx.clone();
        move |id| {
            let _ = tx.send(CommandMsg::ManualImport(id as u64, true));
        }
    });
    app.on_ignore_now({
        let tx = cmd_tx.clone();
        move |id| {
            let _ = tx.send(CommandMsg::Ignore(id as u64));
        }
    });
    app.on_delete_source({
        let tx = cmd_tx.clone();
        move |id| {
            let confirm = rfd::MessageDialog::new()
                .set_title("Confirmar exclusao")
                .set_description(
                    "Tem certeza que quer apagar o arquivo .osz de Downloads? Isso nao remove o beatmap importado.",
                )
                .set_buttons(rfd::MessageButtons::OkCancel)
                .show();
            if confirm == rfd::MessageDialogResult::Ok {
                let _ = tx.send(CommandMsg::DeleteSource(id as u64));
            }
        }
    });
    app.on_open_source({
        let tx = cmd_tx.clone();
        move |id| {
            let _ = tx.send(CommandMsg::OpenSource(id as u64));
        }
    });
    app.on_open_destination({
        let tx = cmd_tx.clone();
        move |id| {
            let _ = tx.send(CommandMsg::OpenDestination(id as u64));
        }
    });
    app.on_open_browser({
        let tx = cmd_tx.clone();
        move |id| {
            let _ = tx.send(CommandMsg::OpenBrowser(id as u64));
        }
    });
    app.on_search_beatmaps({
        let tx = cmd_tx.clone();
        move |query| {
            let _ = tx.send(CommandMsg::SearchBeatmaps(query.to_string()));
        }
    });
    app.on_download_beatmap({
        let tx = cmd_tx.clone();
        move |id| {
            let _ = tx.send(CommandMsg::DownloadBeatmap(id as u64));
        }
    });
    app.on_add_file({
        let tx = cmd_tx.clone();
        move || {
            let _ = tx.send(CommandMsg::AddFileDialog);
        }
    });
    app.on_toggle_show_completed({
        let entries_state = ui_state_entries.clone();
        let cfg_state = shared_config.clone();
        let app_ref = app.as_weak();
        move |_state| {
            let entries_state = entries_state.clone();
            let cfg_state = cfg_state.clone();
            let app_ref = app_ref.clone();
            slint::invoke_from_event_loop(move || {
                if let Some(app) = app_ref.upgrade() {
                    let cfg = cfg_state
                        .lock()
                        .ok()
                        .map(|g| g.clone())
                        .unwrap_or_default();
                    refresh_entries_model(&app, &entries_state, &cfg);
                }
            })
            .ok();
        }
    });
    app.on_toggle_auto_delete({
        let tx = cmd_tx.clone();
        move |state| {
            let _ = tx.send(CommandMsg::ToggleAutoDelete(state));
        }
    });
    app.on_confirm_auto_delete({
        let tx = cmd_tx.clone();
        move |skip| {
            let _ = tx.send(CommandMsg::ConfirmAutoDelete(skip));
        }
    });
    app.on_cancel_auto_delete_prompt({
        let tx = cmd_tx.clone();
        move || {
            let _ = tx.send(CommandMsg::CancelAutoDeletePrompt);
        }
    });
    app.on_copy_logs({
        let tx = cmd_tx.clone();
        move || {
            let _ = tx.send(CommandMsg::CopyLogs);
        }
    });
    app.on_show_error_detail({
        let tx = cmd_tx.clone();
        move |id| {
            let _ = tx.send(CommandMsg::ShowErrorDetail(id as u64));
        }
    });
    app.on_preview_audio({
        let tx = cmd_tx.clone();
        move |id| {
            let _ = tx.send(CommandMsg::PreviewAudio(id as u64));
        }
    });
    app.on_preview_map({
        let tx = cmd_tx.clone();
        move |id| {
            let _ = tx.send(CommandMsg::PreviewMap(id as u64));
        }
    });

    // Worker thread
    {
        let entries = beatmap_entries.clone();
        let cache_store = cache_store.clone();
        let ui_sender = ui_tx.clone();
        let logs_arc = log_state.clone();
        let shared_cfg_thread = shared_config.clone();
        let cfg_start = config.clone();
        let guards_thread = guards.clone();
        let search_map = search_results_state.clone();
        thread::spawn(move || {
            let mut next_id: u64 = 1;
            let mut next_search_id: u64 = 1;
            let mut cfg = cfg_start;
            let audio_player = AudioPlayer::new();
            loop {
                if let Ok(msg) = cmd_rx.recv() {
                    match msg {
                        CommandMsg::AddFile(path) => {
                            let id = next_id;
                            next_id += 1;
                            let entry = BeatmapEntry {
                                id,
                                osz_path: path.clone(),
                                status: ImportStatus::Detected,
                                message: None,
                                error_detail: None,
                                error_short: None,
                                metadata: None,
                                thumbnail_path: None,
                                detected_at: std::time::SystemTime::now(),
                                destination: None,
                                osz_hash: None,
                                audio: app_state::AudioPreview::default(),
                            };
                            if let Ok(mut guard) = entries.lock() {
                                guard.insert(id, entry.clone());
                            }
                            let _ = ui_sender.send(UiMsg::Upsert(entry.clone()));
                            spawn_processing(
                                entry,
                                entries.clone(),
                                ui_sender.clone(),
                                cache_store.clone(),
                                cfg.clone(),
                                guards_thread.clone(),
                            );
                        }
                        CommandMsg::ManualImport(id, force) => {
                            if let Some(entry) =
                                entries.lock().ok().and_then(|m| m.get(&id).cloned())
                            {
                                spawn_import_only(
                                    entry,
                                    entries.clone(),
                                    ui_sender.clone(),
                                    cfg.clone(),
                                    cache_store.clone(),
                                    guards_thread.clone(),
                                    force,
                                );
                            }
                        }
                        CommandMsg::ImportAll => {
                            spawn_bulk_import(
                                entries.clone(),
                                ui_sender.clone(),
                                cfg.clone(),
                                cache_store.clone(),
                                guards_thread.clone(),
                            );
                        }
                        CommandMsg::ClearCompleted => {
                            let mut removed = 0usize;
                            let mut remaining = Vec::new();
                            if let Ok(mut guard) = entries.lock() {
                                guard.retain(|_, e| {
                                    let done = matches!(
                                        e.status,
                                        ImportStatus::Completed | ImportStatus::DuplicateSkipped
                                    );
                                    if done {
                                        removed += 1;
                                    }
                                    !done
                                });
                                remaining = guard.values().cloned().collect();
                                remaining.sort_by_key(|e| e.id);
                            }
                            let _ = ui_sender.send(UiMsg::ReplaceAll(remaining));
                            let _ = ui_sender.send(UiMsg::Log(
                                LogLevel::Info,
                                format!(
                                    "{} item(ns) concluidos removidos da fila (UI only)",
                                    removed
                                ),
                            ));
                        }
                        CommandMsg::UpdateConfig(new_cfg) => {
                            let requested_auto_import = new_cfg.auto_import;
                            let requested_auto_delete = new_cfg.auto_delete_source;
                            cfg = new_cfg;
                            let warning = enforce_path_safety(&mut cfg);
                            let _ = save_config(&cfg);
                            if let Ok(mut guard) = shared_cfg_thread.lock() {
                                *guard = cfg.clone();
                            }
                            let _ = ui_sender.send(UiMsg::ConfigChanged(
                                cfg.clone(),
                                warning.clone(),
                            ));
                            let auto_import_blocked =
                                requested_auto_import && !cfg.auto_import && warning.is_some();
                            let auto_delete_blocked =
                                requested_auto_delete && !cfg.auto_delete_source && warning.is_some();
                            if let Some(warn) = warning {
                                if auto_import_blocked || auto_delete_blocked {
                                    let mut blocked = Vec::new();
                                    if auto_import_blocked {
                                        blocked.push("auto-import");
                                    }
                                    if auto_delete_blocked {
                                        blocked.push("exclusao da fonte");
                                    }
                                    let _ = ui_sender.send(UiMsg::Log(
                                        LogLevel::Warn,
                                        format!(
                                            "{} desabilitado(s) por configuracao insegura: {warn}",
                                            blocked.join(" e ")
                                        ),
                                    ));
                                }
                                let _ = ui_sender.send(UiMsg::Log(
                                    LogLevel::Warn,
                                    format!("Aviso de configuracao: {warn}"),
                                ));
                            } else {
                                let _ = ui_sender.send(UiMsg::Log(
                                    LogLevel::Info,
                                    "Config atualizada".into(),
                                ));
                            }
                        }
                        CommandMsg::ToggleAutoDelete(state) => {
                            let warning =
                                downloads_songs_conflict(&cfg.downloads_dir, &cfg.songs_dir);
                            if state && warning.is_some() {
                                let warn_text =
                                    warning.clone().unwrap_or_else(|| "Caminho inseguro".into());
                                let _ = ui_sender.send(UiMsg::Log(
                                    LogLevel::Warn,
                                    format!("Exclusao automatica bloqueada: {warn_text}"),
                                ));
                                let _ = ui_sender.send(UiMsg::ConfigChanged(
                                    cfg.clone(),
                                    warning.clone(),
                                ));
                                continue;
                            }
                            if state && !cfg.suppress_delete_prompt {
                                let _ = ui_sender.send(UiMsg::ShowAutoDeletePrompt);
                                let _ = ui_sender.send(UiMsg::ConfigChanged(
                                    cfg.clone(),
                                    warning.clone(),
                                ));
                                continue;
                            }
                            cfg.auto_delete_source = state;
                            let warning = enforce_path_safety(&mut cfg);
                            let _ = save_config(&cfg);
                            if let Ok(mut guard) = shared_cfg_thread.lock() {
                                *guard = cfg.clone();
                            }
                            let _ = ui_sender.send(UiMsg::ConfigChanged(
                                cfg.clone(),
                                warning.clone(),
                            ));
                            let text = if state {
                                "Excluir fonte apos importar ligado"
                            } else {
                                "Excluir fonte apos importar desligado"
                            };
                            let _ = ui_sender.send(UiMsg::Log(LogLevel::Info, text.into()));
                        }
                        CommandMsg::ConfirmAutoDelete(skip_prompt) => {
                            cfg.auto_delete_source = true;
                            if skip_prompt {
                                cfg.suppress_delete_prompt = true;
                            }
                            let warning = enforce_path_safety(&mut cfg);
                            let _ = save_config(&cfg);
                            if let Ok(mut guard) = shared_cfg_thread.lock() {
                                *guard = cfg.clone();
                            }
                            let _ = ui_sender.send(UiMsg::HideAutoDeletePrompt);
                            let _ = ui_sender.send(UiMsg::ConfigChanged(
                                cfg.clone(),
                                warning.clone(),
                            ));
                            let _ = ui_sender.send(UiMsg::Log(
                                LogLevel::Info,
                                "Exclusao automatica ativada".into(),
                            ));
                        }
                        CommandMsg::CancelAutoDeletePrompt => {
                            let warning =
                                downloads_songs_conflict(&cfg.downloads_dir, &cfg.songs_dir);
                            let _ = ui_sender.send(UiMsg::HideAutoDeletePrompt);
                            let _ = ui_sender.send(UiMsg::ConfigChanged(
                                cfg.clone(),
                                warning.clone(),
                            ));
                        }
                        CommandMsg::AddFileDialog => {
                            if let Some(file) = rfd::FileDialog::new()
                                .add_filter("Beatmap", &["osz"])
                                .pick_file()
                            {
                                let _ = ui_sender.send(UiMsg::Log(
                                    LogLevel::Info,
                                    format!("Arquivo escolhido: {}", file.to_string_lossy()),
                                ));
                                let _ = cmd_tx.send(CommandMsg::AddFile(file));
                            }
                        }
                        CommandMsg::OpenSource(id) => {
                            if let Some(entry) =
                                entries.lock().ok().and_then(|m| m.get(&id).cloned())
                            {
                                open_in_explorer(&entry.osz_path);
                            }
                        }
                        CommandMsg::OpenDestination(id) => {
                            if let Some(entry) =
                                entries.lock().ok().and_then(|m| m.get(&id).cloned())
                            {
                                if let Some(dest) = entry.destination.clone() {
                                    open_in_explorer(&dest);
                                }
                            }
                        }
                        CommandMsg::OpenBrowser(id) => {
                            if let Some(entry) =
                                entries.lock().ok().and_then(|m| m.get(&id).cloned())
                            {
                                if let Some(meta) = entry.metadata {
                                    if let Some(set_id) = meta.beatmap_set_id {
                                        let _ = open_in_browser(set_id);
                                    }
                                }
                            }
                        }
                        CommandMsg::SearchBeatmaps(query) => {
                            // --- CONFIGURAÇÃO DO LOG EM ARQUIVO ---
                            let log_path = "logs/search_log.txt";
                            let mut log_file = OpenOptions::new()
                                .create(true) // Cria o arquivo se não existir
                                .append(true) // Adiciona ao final do arquivo em vez de apagar
                                .open(log_path)
                                .expect("Não foi possível abrir o arquivo de log.");
                            // --- FIM DA CONFIGURAÇÃO ---

                            // Agora, todas as chamadas `println!` e `eprintln!` são substituídas por `writeln!`
                            writeln!(log_file, "\n--- INICIANDO NOVO CICLO DE BUSCA ---").unwrap();
                            
                            let trimmed = query.trim().to_string();
                            if trimmed.is_empty() {
                                let _ = ui_sender.send(UiMsg::BeatmapSearchState {
                                    loading: false,
                                    message: Some("Digite um termo para buscar beatmaps.".into()),
                                });
                                continue;
                            }
                            let _ = ui_sender.send(UiMsg::BeatmapSearchState {
                                loading: true,
                                message: None,
                            });

                            writeln!(log_file, "[DIAGNÓSTICO] Buscando pelo termo: '{}'", trimmed).unwrap();

                            let mut fetch_error = false;
                            let found: Vec<BeatmapFound> = match fetch_nerinyan(&trimmed) {
                                Ok(list) => {
                                    writeln!(log_file, "[DIAGNÓSTICO] fetch_nerinyan retornou Ok. Número de beatmaps encontrados: {}", list.len()).unwrap();
                                    list
                                },
                                Err(err) => {
                                    // --- MUDANÇA CRÍTICA ---
                                    // Agora, em vez de uma mensagem genérica, vamos imprimir a causa raiz detalhada do erro.
                                    writeln!(log_file, "--- ERRO FATAL NA FUNÇÃO fetch_nerinyan ---").unwrap();
                                    writeln!(log_file, "A causa raiz do erro foi:").unwrap();
                                    writeln!(log_file, "{:#?}", err).unwrap(); // Imprime o erro detalhado com formatação.
                                    // --- FIM DA MUDANÇA ---
                                    
                                    fetch_error = true;
                                    let _ = ui_sender.send(UiMsg::Log(
                                        LogLevel::Warn,
                                        format!("Falha na busca Nerinyan: {:?}", err),
                                    ));
                                    Vec::new()
                                }
                            };

                            writeln!(log_file, "[DIAGNÓSTICO] Vetor 'found' tem {} itens antes do processamento do Mutex.", found.len()).unwrap();

                            let mut items: Vec<BeatmapSearchResult> = Vec::new();
                            match search_map.lock() {
                                Ok(mut map) => {
                                    writeln!(log_file, "[DIAGNÓSTICO] Mutex lock adquirido com sucesso.").unwrap();
                                    map.clear();
                                    for entry in found {
                                        let id = next_search_id;
                                        next_search_id += 1;
                                        let result = BeatmapSearchResult { id, title: entry.title, artist: entry.artist, creator: entry.creator, source: entry.source, download_url: entry.download_url };
                                        map.insert(id, result.clone());
                                        items.push(result);
                                    }
                                },
                                Err(poisoned) => {
                                    writeln!(log_file, "[DIAGNÓSTICO] Mutex estava envenenado! Tentando recuperar.").unwrap();
                                    let mut map = poisoned.into_inner();
                                    map.clear();
                                    for entry in found {
                                        let id = next_search_id;
                                        next_search_id += 1;
                                        let result = BeatmapSearchResult { id, title: entry.title, artist: entry.artist, creator: entry.creator, source: entry.source, download_url: entry.download_url };
                                        map.insert(id, result.clone())
;                    items.push(result);
                                    }
                                }
                            }

                            writeln!(log_file, "[DIAGNÓSTICO] Vetor 'items' tem {} itens após o processamento do Mutex.", items.len()).unwrap();

                            if items.is_empty() {
                                writeln!(log_file, "[DIAGNÓSTICO] 'items' está vazio. Preparando mensagem de 'sem resultados' ou 'falha'.").unwrap();
                                let message = if fetch_error { "Falha ao buscar beatmaps na Nerinyan.".into() } else { "Nenhum beatmap encontrado.".into() };
                                writeln!(log_file, "[DIAGNÓSTICO] Enviando para UI a mensagem: '{}'", message).unwrap();
                                let _ = ui_sender.send(UiMsg::BeatmapResults(Vec::new()));
                                let _ = ui_sender.send(UiMsg::BeatmapSearchState { loading: false, message: Some(message) });
                            } else {
                                writeln!(log_file, "[DIAGNÓSTICO] 'items' tem resultados. Enviando {} itens para a UI.", items.len()).unwrap();
                                let _ = ui_sender.send(UiMsg::BeatmapResults(items));
                                let _ = ui_sender.send(UiMsg::BeatmapSearchState { loading: false, message: None });
                            }
                            writeln!(log_file, "--- FIM DO CICLO DE BUSCA ---\n").unwrap();
                        }
                        CommandMsg::DownloadBeatmap(search_id) => {
                            let result_opt = search_map
                                .lock()
                                .ok()
                                .and_then(|m| m.get(&search_id).cloned());
                            let Some(result) = result_opt else {
                                let _ = ui_sender.send(UiMsg::BeatmapDownloadStatus {
                                    active: false,
                                    text: Some("Beatmap nao encontrado nos resultados.".into()),
                                });
                                continue;
                            };
                            let status_label = format!(
                                "Baixando {} - {} ({}) via {}...",
                                result.artist,
                                result.title,
                                result.creator,
                                beatmap_source_label(&result.source),
                            );
                            let _ = ui_sender.send(UiMsg::BeatmapDownloadStatus {
                                active: true,
                                text: Some(status_label),
                            });

                            let downloads_dir = cfg.downloads_dir.clone();
                            let ui_sender_clone = ui_sender.clone();
                            let cmd_tx_clone = cmd_tx.clone();
                            thread::spawn(move || {
                                let download_name = build_osz_name(&result);
                                let target = ensure_unique_path(&downloads_dir, &download_name);
                                let part_path = target.with_extension("osz.part");
                                let client = reqwest::blocking::Client::builder()
                                    .user_agent("McOsuImporter/beatmap-search")
                                    .build();
                                let client = match client {
                                    Ok(c) => c,
                                    Err(err) => {
                                        let _ = ui_sender_clone.send(UiMsg::BeatmapDownloadStatus {
                                            active: false,
                                            text: Some(format!(
                                                "Falha ao inicializar download: {err}"
                                            )),
                                        });
                                        return;
                                    }
                                };
                                let res = download_with_progress(
                                    &client,
                                    &result.download_url,
                                    &part_path,
                                    &target,
                                    |done, total| {
                                        if let Some(total) = total {
                                            let pct = ((done as f64 / total as f64) * 100.0)
                                                .clamp(0.0, 100.0);
                                            let _ = ui_sender_clone.send(
                                                UiMsg::BeatmapDownloadStatus {
                                                    active: true,
                                                    text: Some(format!(
                                                        "Baixando... {:.0}% ({:.1} / {:.1} MB)",
                                                        pct,
                                                        done as f64 / 1_048_576.0,
                                                        total as f64 / 1_048_576.0
                                                    )),
                                                },
                                            );
                                        } else {
                                            let _ = ui_sender_clone.send(
                                                UiMsg::BeatmapDownloadStatus {
                                                    active: true,
                                                    text: Some(format!(
                                                        "Baixando... {:.1} MB",
                                                        done as f64 / 1_048_576.0
                                                    )),
                                                },
                                            );
                                        }
                                    },
                                );
                                match res {
                                    Ok(_) => {
                                        let _ = ui_sender_clone.send(
                                            UiMsg::BeatmapDownloadStatus {
                                                active: false,
                                                text: Some("Download concluido!".into()),
                                            },
                                        );
                                        let _ = ui_sender_clone.send(UiMsg::Log(
                                            LogLevel::Info,
                                            format!(
                                                "Download concluido: {}",
                                                target
                                                    .file_name()
                                                    .and_then(|s| s.to_str())
                                                    .unwrap_or_default()
                                            ),
                                        ));
                                        let _ = cmd_tx_clone.send(CommandMsg::AddFile(target));
                                    }
                                    Err(err) => {
                                        eprintln!("Falha no download: {:?}", err);
                                        let _ = ui_sender_clone.send(
                                            UiMsg::BeatmapDownloadStatus {
                                                active: false,
                                                text: Some(format!(
                                                    "Falha no download: {:?}",
                                                    err
                                                )),
                                            },
                                        );
                                        let _ = ui_sender_clone.send(UiMsg::Log(
                                            LogLevel::Error,
                                            format!(
                                                "Erro ao baixar {}: {:?}",
                                                result.download_url,
                                                err
                                            ),
                                        ));
                                    }
                                }
                            });
                        }
                        CommandMsg::DeleteSource(id) => {
                            if let Some(mut entry) =
                                entries.lock().ok().and_then(|m| m.get(&id).cloned())
                            {
                                handle_delete_source(&mut entry, &entries, &ui_sender, &cfg);
                            }
                        }
                        CommandMsg::Ignore(id) => {
                            if let Some(mut entry) =
                                entries.lock().ok().and_then(|m| m.get(&id).cloned())
                            {
                                update_entry(
                                    &mut entry,
                                    &entries,
                                    &ui_sender,
                                    ImportStatus::DuplicateSkipped,
                                    Some("Ignorado pelo usuario".into()),
                                    None,
                                );
                            }
                        }
                        CommandMsg::CopyLogs => {
                            if let Ok(logs) = logs_arc.lock() {
                                let text = logs
                                    .iter()
                                    .map(|(_, msg)| msg.clone())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                if let Ok(mut cb) = Clipboard::new() {
                                    let _ = cb.set_text(text);
                                    let _ = ui_sender
                                        .send(UiMsg::Log(LogLevel::Info, "Logs copiados".into()));
                                }
                            }
                        }
                        CommandMsg::ShowErrorDetail(id) => {
                            if let Some(entry) =
                                entries.lock().ok().and_then(|m| m.get(&id).cloned())
                            {
                                let detail = entry
                                    .error_detail
                                    .clone()
                                    .or(entry.message.clone())
                                    .unwrap_or_else(|| "Sem detalhes adicionais".into());
                                rfd::MessageDialog::new()
                                    .set_title("Detalhes do erro")
                                    .set_description(&detail)
                                    .set_buttons(rfd::MessageButtons::Ok)
                                    .show();
                            }
                        }
                        CommandMsg::PreviewAudio(id) => {
                            if let Some(entry) =
                                entries.lock().ok().and_then(|m| m.get(&id).cloned())
                            {
                                let entries_clone = entries.clone();
                                let ui_clone = ui_sender.clone();
                                let cache_clone = cache_store.clone();
                                handle_audio_preview(
                                    entry,
                                    entries_clone,
                                    ui_clone,
                                    cache_clone,
                                    audio_player.clone(),
                                );
                            }
                        }
                        CommandMsg::PreviewMap(id) => {
                            if let Some(entry) =
                                entries.lock().ok().and_then(|m| m.get(&id).cloned())
                            {
                                let entries_clone = entries.clone();
                                let ui_clone = ui_sender.clone();
                                let cache_clone = cache_store.clone();
                                let cfg_clone = cfg.clone();
                                thread::spawn(move || {
                                    handle_preview_map(
                                        entry,
                                        entries_clone,
                                        ui_clone,
                                        cache_clone,
                                        cfg_clone,
                                    );
                                });
                            }
                        }
                    }
                }
            }
        });
    }

    // UI update thread (receives UiMsg and applies in UI thread)
    {
        let entries_state = ui_state_entries.clone();
        let logs_state = log_state.clone();
        let app_weak = app.as_weak();
        let config_state = shared_config.clone();
        thread::spawn(move || {
            while let Ok(msg) = ui_rx.recv() {
                match msg {
                    UiMsg::Upsert(entry) => {
                        let entries_state = entries_state.clone();
                        let app_ref = app_weak.clone();
                        let cfg_state = config_state.clone();
                        slint::invoke_from_event_loop(move || {
                            {
                                if let Ok(mut vec) = entries_state.lock() {
                                    if let Some(pos) = vec.iter().position(|e| e.id == entry.id) {
                                        vec[pos] = entry.clone();
                                    } else {
                                        vec.push(entry.clone());
                                    }
                                }
                            }
                            if let Some(app) = app_ref.upgrade() {
                                let cfg = cfg_state
                                    .lock()
                                    .ok()
                                    .map(|g| g.clone())
                                    .unwrap_or_default();
                                refresh_entries_model(&app, &entries_state, &cfg);
                            }
                        })
                        .ok();
                    }
                    UiMsg::ReplaceAll(list) => {
                        let entries_state = entries_state.clone();
                        let app_ref = app_weak.clone();
                        let cfg_state = config_state.clone();
                        slint::invoke_from_event_loop(move || {
                            {
                                if let Ok(mut vec) = entries_state.lock() {
                                    *vec = list.clone();
                                }
                            }
                            if let Some(app) = app_ref.upgrade() {
                                let cfg = cfg_state
                                    .lock()
                                    .ok()
                                    .map(|g| g.clone())
                                    .unwrap_or_default();
                                refresh_entries_model(&app, &entries_state, &cfg);
                            }
                        })
                        .ok();
                    }
                    UiMsg::Log(level, line) => {
                        let logs_state = logs_state.clone();
                        let app_ref = app_weak.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Ok(mut logs) = logs_state.lock() {
                                logs.push((level, line.clone()));
                                if logs.len() > 200 {
                                    logs.remove(0);
                                }
                                if let Some(app) = app_ref.upgrade() {
                                    let log_items = logs
                                        .iter()
                                        .cloned()
                                        .map(|(lvl, msg)| to_log_item(lvl, &msg))
                                        .collect::<Vec<_>>();
                                    let model = Rc::new(slint::VecModel::from(log_items));
                                    app.set_logs(model.into());
                                }
                            }
                        })
                        .ok();
                    }
                    UiMsg::BeatmapSearchState { loading, message } => {
                        let app_ref = app_weak.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = app_ref.upgrade() {
                                app.set_beatmap_loading(loading);
                                app.set_beatmap_message(SharedString::from(
                                    message.unwrap_or_default(),
                                ));
                            }
                        })
                        .ok();
                    }
                    UiMsg::BeatmapResults(list) => {
                        let app_ref = app_weak.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = app_ref.upgrade() {
                                let items = list
                                    .iter()
                                    .map(to_search_item)
                                    .collect::<Vec<_>>();
                                let model = Rc::new(slint::VecModel::from(items));
                                app.set_beatmap_results(model.into());
                            }
                        })
                        .ok();
                    }
                    UiMsg::BeatmapDownloadStatus { active, text } => {
                        let app_ref = app_weak.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = app_ref.upgrade() {
                                app.set_beatmap_downloading(active);
                                app.set_beatmap_status(SharedString::from(
                                    text.unwrap_or_default(),
                                ));
                            }
                        })
                        .ok();
                    }
                    UiMsg::ConfigChanged(cfg, warning) => {
                        let entries_state = entries_state.clone();
                        let app_ref = app_weak.clone();
                        let cfg_state = config_state.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Ok(mut guard) = cfg_state.lock() {
                                *guard = cfg.clone();
                            }
                            if let Some(app) = app_ref.upgrade() {
                                app.set_download_path(SharedString::from(
                                    cfg.downloads_dir.display().to_string(),
                                ));
                                app.set_songs_path(SharedString::from(
                                    cfg.songs_dir.display().to_string(),
                                ));
                                app.set_auto_import(cfg.auto_import);
                                app.set_auto_delete_after_import(cfg.auto_delete_source);
                                app.set_paths_blocked(warning.is_some());
                                app.set_path_warning(SharedString::from(
                                    warning.clone().unwrap_or_default(),
                                ));
                                if let Ok(vec) = entries_state.lock() {
                                    drop(vec);
                                }
                                refresh_entries_model(&app, &entries_state, &cfg);
                            }
                        })
                        .ok();
                    }
                    UiMsg::ShowAutoDeletePrompt => {
                        let app_ref = app_weak.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = app_ref.upgrade() {
                                app.set_auto_delete_prompt_skip(false);
                                app.set_auto_delete_prompt_visible(true);
                            }
                        })
                        .ok();
                    }
                    UiMsg::HideAutoDeletePrompt => {
                        let app_ref = app_weak.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = app_ref.upgrade() {
                                app.set_auto_delete_prompt_visible(false);
                            }
                        })
                        .ok();
                    }
                    UiMsg::BulkRunning(state) => {
                        let app_ref = app_weak.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = app_ref.upgrade() {
                                app.set_bulk_import_running(state);
                            }
                        })
                        .ok();
                    }
                }
            }
        });
    }

    app.run()?;
    Ok(())
}

fn spawn_processing(
    mut entry: BeatmapEntry,
    entries: Arc<Mutex<HashMap<u64, BeatmapEntry>>>,
    ui_sender: mpsc::Sender<UiMsg>,
    cache_store: Arc<CacheStore>,
    cfg: AppConfig,
    guards: Arc<ImportGuards>,
) {
    thread::spawn(move || {
        update_entry(
            &mut entry,
            &entries,
            &ui_sender,
            ImportStatus::WaitingStable,
            None,
            None,
        );
        if !watcher::is_file_stable(&entry.osz_path, &cfg.stability) {
            update_entry(
                &mut entry,
                &entries,
                &ui_sender,
                ImportStatus::Failed,
                Some("Arquivo nao estabilizou".into()),
                Some("O arquivo nao ficou estavel dentro do tempo limite".into()),
            );
            return;
        }
        update_entry(
            &mut entry,
            &entries,
            &ui_sender,
            ImportStatus::ReadingMetadata,
            None,
            None,
        );

        match osz_reader::read_osz_metadata(&entry.osz_path, &cache_store) {
            Ok(meta) => {
                entry.metadata = Some(meta.metadata.clone());
                entry.thumbnail_path = meta.thumbnail_path.clone();
                entry.osz_hash = Some(meta.hash.clone());
                // duplicate detection
                if let Some(set_id) = meta.metadata.beatmap_set_id {
                    if let Some(dest) = cache_store.find_set(set_id) {
                        entry.destination = Some(dest.clone());
                        update_entry(
                            &mut entry,
                            &entries,
                            &ui_sender,
                            ImportStatus::DuplicateSkipped,
                            Some("Duplicado (BeatmapSetID)".into()),
                            None,
                        );
                        return;
                    }
                }
                if let Some(dest) = cache_store.find_hash(&meta.hash) {
                    entry.destination = Some(dest.clone());
                    update_entry(
                        &mut entry,
                        &entries,
                        &ui_sender,
                        ImportStatus::DuplicateSkipped,
                        Some("Duplicado (hash)".into()),
                        None,
                    );
                    return;
                }
                let hash_short: String = meta.hash.chars().take(8).collect();
                update_entry(
                    &mut entry,
                    &entries,
                    &ui_sender,
                    ImportStatus::ReadingMetadata,
                    Some(format!("Metadados lidos ({hash_short})")),
                    None,
                );
            }
            Err(err) => {
                update_entry(
                    &mut entry,
                    &entries,
                    &ui_sender,
                    ImportStatus::Failed,
                    Some("Erro ao ler metadados".into()),
                    Some(format!("{err:#}")),
                );
                return;
            }
        }

        if cfg.auto_import
            && downloads_songs_conflict(&cfg.downloads_dir, &cfg.songs_dir).is_none()
        {
            perform_import(
                &mut entry,
                &entries,
                &ui_sender,
                &cfg,
                &cache_store,
                &guards,
                false,
            );
        } else if cfg.auto_import {
            let _ = ui_sender.send(UiMsg::Log(
                LogLevel::Warn,
                "Auto-import bloqueado ate corrigir caminhos".into(),
            ));
        }
    });
}

fn spawn_import_only(
    mut entry: BeatmapEntry,
    entries: Arc<Mutex<HashMap<u64, BeatmapEntry>>>,
    ui_sender: mpsc::Sender<UiMsg>,
    cfg: AppConfig,
    cache_store: Arc<CacheStore>,
    guards: Arc<ImportGuards>,
    force: bool,
) {
    thread::spawn(move || {
        perform_import(
            &mut entry,
            &entries,
            &ui_sender,
            &cfg,
            &cache_store,
            &guards,
            force,
        );
    });
}

fn is_ready_for_import(entry: &BeatmapEntry) -> bool {
    if entry.metadata.is_none() {
        return false;
    }
    !matches!(
        entry.status,
        ImportStatus::Completed
            | ImportStatus::Importing
            | ImportStatus::DuplicateSkipped
            | ImportStatus::Failed
    )
}

fn spawn_bulk_import(
    entries: Arc<Mutex<HashMap<u64, BeatmapEntry>>>,
    ui_sender: mpsc::Sender<UiMsg>,
    cfg: AppConfig,
    cache_store: Arc<CacheStore>,
    guards: Arc<ImportGuards>,
) {
    thread::spawn(move || {
        if downloads_songs_conflict(&cfg.downloads_dir, &cfg.songs_dir).is_some() {
            let _ = ui_sender.send(UiMsg::Log(
                LogLevel::Warn,
                "Importar ja bloqueado por configuracao insegura de caminhos.".into(),
            ));
            return;
        }
        if !guards.try_start_bulk() {
            let _ = ui_sender.send(UiMsg::Log(
                LogLevel::Warn,
                "Importar ja ja esta em andamento; clique ignorado.".into(),
            ));
            return;
        }
        let _ = ui_sender.send(UiMsg::BulkRunning(true));
        struct BulkRelease<'a> {
            guards: &'a ImportGuards,
            sender: mpsc::Sender<UiMsg>,
        }
        impl Drop for BulkRelease<'_> {
            fn drop(&mut self) {
                self.guards.finish_bulk();
                let _ = self.sender.send(UiMsg::BulkRunning(false));
            }
        }
        let _bulk_guard = BulkRelease {
            guards: &guards,
            sender: ui_sender.clone(),
        };

        let ready = entries
            .lock()
            .map(|m| {
                m.values()
                    .filter(|e| is_ready_for_import(e))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if ready.is_empty() {
            let _ = ui_sender.send(UiMsg::Log(
                LogLevel::Info,
                "Nenhum item pronto para importar.".into(),
            ));
            return;
        }
        let _ = ui_sender.send(UiMsg::Log(
            LogLevel::Info,
            format!("Importando {} item(ns) da fila", ready.len()),
        ));
        for mut entry in ready {
            perform_import(
                &mut entry,
                &entries,
                &ui_sender,
                &cfg,
                &cache_store,
                &guards,
                false,
            );
        }
    });
}

fn perform_import(
    entry: &mut BeatmapEntry,
    entries: &Arc<Mutex<HashMap<u64, BeatmapEntry>>>,
    ui_sender: &mpsc::Sender<UiMsg>,
    cfg: &AppConfig,
    cache_store: &Arc<CacheStore>,
    guards: &Arc<ImportGuards>,
    force: bool,
) {
    if !guards.try_lock_entry(entry.id) {
        let _ = ui_sender.send(UiMsg::Log(
            LogLevel::Warn,
            format!(
                "{}: Importacao em andamento; clique repetido ignorado",
                entry.source_file_name()
            ),
        ));
        return;
    }
    struct EntryRelease<'a> {
        guards: &'a Arc<ImportGuards>,
        id: u64,
    }
    impl Drop for EntryRelease<'_> {
        fn drop(&mut self) {
            self.guards.release_entry(self.id);
        }
    }
    let _entry_release = EntryRelease {
        guards,
        id: entry.id,
    };

    update_entry(
        entry,
        entries,
        ui_sender,
        ImportStatus::Importing,
        None,
        None,
    );
    if let Some(meta) = entry.metadata.clone() {
        match importer::import_osz(entry, &meta, &cfg.songs_dir, force) {
            Ok(res) => {
                entry.destination = Some(res.destination.clone());
                let status = if res.duplicated {
                    ImportStatus::DuplicateSkipped
                } else {
                    ImportStatus::Completed
                };
                let msg = if res.duplicated {
                    Some("Duplicado - pasta ja existia".into())
                } else {
                    Some("Importado".into())
                };
                if let Some(set_id) = meta.beatmap_set_id {
                    cache_store.register_beatmap_set(set_id, res.destination.clone());
                }
                if let Some(hash) = entry.osz_hash.clone() {
                    cache_store.register_hash(hash, res.destination.clone());
                }
                let _ = cache_store.save();
                update_entry(entry, entries, ui_sender, status, msg, None);
                if matches!(status, ImportStatus::Completed)
                    && cfg.auto_delete_source
                    && downloads_songs_conflict(&cfg.downloads_dir, &cfg.songs_dir).is_none()
                {
                    maybe_delete_source_after_import(entry, entries, ui_sender, cfg);
                }
            }
            Err(err) => {
                let (short, detail) = classify_import_error(&err);
                update_entry(
                    entry,
                    entries,
                    ui_sender,
                    ImportStatus::Failed,
                    Some(short),
                    Some(detail),
                );
            }
        }
    } else {
        update_entry(
            entry,
            entries,
            ui_sender,
            ImportStatus::Failed,
            Some("Sem metadados".into()),
            Some("Nao foi possivel ler os metadados do arquivo .osz".into()),
        );
    }
}

fn handle_delete_source(
    entry: &mut BeatmapEntry,
    entries: &Arc<Mutex<HashMap<u64, BeatmapEntry>>>,
    ui_sender: &mpsc::Sender<UiMsg>,
    cfg: &AppConfig,
) {
    if entry.status != ImportStatus::Completed {
        let _ = ui_sender.send(UiMsg::Log(
            LogLevel::Warn,
            format!(
                "{}: Apenas itens concluidos podem apagar a fonte",
                entry.source_file_name()
            ),
        ));
        return;
    }
    attempt_delete_source(
        entry,
        entries,
        ui_sender,
        cfg,
        "Fonte apagada de Downloads",
        "Exclusao manual",
    );
}

fn attempt_delete_source(
    entry: &mut BeatmapEntry,
    entries: &Arc<Mutex<HashMap<u64, BeatmapEntry>>>,
    ui_sender: &mpsc::Sender<UiMsg>,
    cfg: &AppConfig,
    success_msg: &str,
    failure_context: &str,
) {
    if let Some(warn) = downloads_songs_conflict(&cfg.downloads_dir, &cfg.songs_dir) {
        let _ = ui_sender.send(UiMsg::Log(
            LogLevel::Warn,
            format!("Protecao ativa: {warn}"),
        ));
        return;
    }
    if !can_delete_source(&cfg.downloads_dir, &cfg.songs_dir, &entry.osz_path) {
        let _ = ui_sender.send(UiMsg::Log(
            LogLevel::Warn,
            format!(
                "{}: Fonte fora da pasta de Downloads configurada; nada apagado",
                entry.source_file_name()
            ),
        ));
        return;
    }
    if !entry.osz_path.exists() {
        update_entry(
            entry,
            entries,
            ui_sender,
            entry.status,
            Some("Fonte nao encontrada em Downloads".into()),
            None,
        );
        return;
    }

    let deletion = trash::delete(&entry.osz_path).or_else(|err| {
        let _ = ui_sender.send(UiMsg::Log(
            LogLevel::Warn,
            format!(
                "{}: Falha ao mover para lixeira ({err}); tentando apagar definitivamente",
                entry.source_file_name()
            ),
        ));
        fs::remove_file(&entry.osz_path)
    });
    match deletion {
        Ok(_) => {
            update_entry(
                entry,
                entries,
                ui_sender,
                entry.status,
                Some(success_msg.into()),
                None,
            );
        }
        Err(err) => {
            update_entry(
                entry,
                entries,
                ui_sender,
                entry.status,
                Some(format!("{failure_context}: fonte nao removida")),
                Some(err.to_string()),
            );
        }
    }
}

fn maybe_delete_source_after_import(
    entry: &mut BeatmapEntry,
    entries: &Arc<Mutex<HashMap<u64, BeatmapEntry>>>,
    ui_sender: &mpsc::Sender<UiMsg>,
    cfg: &AppConfig,
) {
    attempt_delete_source(
        entry,
        entries,
        ui_sender,
        cfg,
        "Fonte apagada apos importar",
        "Auto-delete",
    );
}

fn classify_import_error(err: &anyhow::Error) -> (String, String) {
    let detail = format!("{err:#}");
    let err_txt = err.to_string().to_lowercase();
    let short = if err_txt.contains("zip")
        || err_txt.contains("archive")
        || err_txt.contains("unzip")
    {
        "Falha ao extrair o .osz"
    } else if err_txt.contains("create")
        || err_txt.contains("permiss")
        || err_txt.contains("acesso")
        || err_txt.contains("denied")
    {
        "Falha ao criar/gravar na pasta destino"
    } else if err_txt.contains("metadata") || err_txt.contains("metadado") {
        "Falha ao ler metadados"
    } else {
        "Erro ao importar .osz"
    };
    (short.into(), detail)
}

fn update_entry(
    entry: &mut BeatmapEntry,
    entries: &Arc<Mutex<HashMap<u64, BeatmapEntry>>>,
    ui_sender: &mpsc::Sender<UiMsg>,
    status: ImportStatus,
    message: Option<String>,
    error_detail: Option<String>,
) {
    entry.status = status;
    entry.message = message.clone();
    entry.error_detail = error_detail.clone();
    if status == ImportStatus::Failed || entry.error_detail.is_some() {
        entry.error_short = message.clone();
    } else {
        entry.error_short = None;
    }
    if let Ok(mut guard) = entries.lock() {
        if let Some(stored) = guard.get_mut(&entry.id) {
            *stored = entry.clone();
        }
    }
    let _ = ui_sender.send(UiMsg::Upsert(entry.clone()));
    let _ = ui_sender.send(UiMsg::Upsert(entry.clone()));
    if let Some(msg) = message {
        let level = match status {
            ImportStatus::Failed => LogLevel::Error,
            ImportStatus::DuplicateSkipped => LogLevel::Warn,
            _ if entry.error_detail.is_some() => LogLevel::Warn,
            _ => LogLevel::Info,
        };
        let mut line = format!("{}: {}", entry.source_file_name(), msg);
        if let Some(detail) = error_detail {
            line.push_str(&format!(" ({detail})"));
        }
        let _ = ui_sender.send(UiMsg::Log(level, line));
    }
}

fn refresh_entries_model(
    app: &AppWindow,
    entries_state: &Arc<Mutex<Vec<BeatmapEntry>>>,
    cfg: &AppConfig,
) {
    let path_warning = downloads_songs_conflict(&cfg.downloads_dir, &cfg.songs_dir);
    if let Ok(vec) = entries_state.lock() {
        let show_completed = app.get_show_completed();
        let ui_items = vec
            .iter()
            .filter(|e| {
                show_completed
                    || !matches!(
                        e.status,
                        ImportStatus::Completed | ImportStatus::DuplicateSkipped
                    )
            })
            .map(|e| to_ui_item(e, cfg, path_warning.as_deref()))
            .collect::<Vec<_>>();
        let model = Rc::new(slint::VecModel::from(ui_items));
        app.set_beatmaps(model.into());
    }
    app.set_path_warning(SharedString::from(
        path_warning.clone().unwrap_or_default(),
    ));
    app.set_paths_blocked(path_warning.is_some());
}

fn to_ui_item(entry: &BeatmapEntry, cfg: &AppConfig, path_warning: Option<&str>) -> BeatmapItem {
    let placeholder = {
        let buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(1, 1);
        slint::Image::from_rgb8(buffer)
    };
    let image = entry
        .thumbnail_path
        .as_ref()
        .and_then(|p| slint::Image::load_from_path(p).ok())
        .unwrap_or(placeholder);
    let show_delete = matches!(entry.status, ImportStatus::Completed);
    let warning_owned = path_warning.map(|s| s.to_string());
    let in_downloads = is_within_dir(&cfg.downloads_dir, &entry.osz_path);
    let source_exists = entry.osz_path.exists();
    let can_delete = show_delete
        && can_delete_source(&cfg.downloads_dir, &cfg.songs_dir, &entry.osz_path)
        && source_exists;
    let can_import = matches!(
        entry.status,
        ImportStatus::Detected
            | ImportStatus::WaitingStable
            | ImportStatus::ReadingMetadata
            | ImportStatus::Failed
    );
    let can_reimport = matches!(
        entry.status,
        ImportStatus::DuplicateSkipped | ImportStatus::Completed | ImportStatus::Failed
    );
    let can_ignore = !matches!(entry.status, ImportStatus::Importing);
    let mut info_message = entry.message.clone().unwrap_or_default();
    let mut error_short = entry.error_short.clone().unwrap_or_default();
    let mut error_detail = entry.error_detail.clone().unwrap_or_default();
    if matches!(entry.status, ImportStatus::Failed) {
        if error_short.is_empty() {
            error_short = if !info_message.is_empty() {
                info_message.clone()
            } else {
                "Falha ao importar".into()
            };
        }
        if error_detail.is_empty() {
            error_detail = error_short.clone();
        }
        info_message.clear();
    } else if !error_detail.is_empty() && error_short.is_empty() {
        error_short = if !info_message.is_empty() {
            info_message.clone()
        } else {
            "Aviso ao remover fonte".into()
        };
        info_message.clear();
    }
    let mut delete_hint = String::new();
    if show_delete {
        if !source_exists {
            delete_hint = "Fonte nao encontrada".into();
        } else if let Some(warn) = warning_owned.clone() {
            delete_hint = warn;
        } else if !in_downloads {
            delete_hint = "Fonte fora de Downloads".into();
        }
    }
    let title = entry
        .metadata
        .as_ref()
        .map(|m| m.display_title())
        .unwrap_or_else(|| "Desconhecido".into());
    let artist = entry
        .metadata
        .as_ref()
        .map(|m| m.artist.clone())
        .unwrap_or_default();
    let source_full = entry.source_file_name();
    let destination_full = entry
        .destination
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "-".into());
    let (audio_status, audio_available, audio_playing, audio_enabled) = audio_status_ui(entry);
    let preview_enabled = entry.metadata.is_some()
        && (entry.osz_path.exists() || entry.destination.as_ref().map(|d| d.exists()).unwrap_or(false));
    BeatmapItem {
        id: entry.id as i32,
        title: SharedString::from(title),
        artist: SharedString::from(artist),
        creator: SharedString::from(
            entry
                .metadata
                .as_ref()
                .map(|m| m.creator.clone())
                .unwrap_or_default(),
        ),
        source: SharedString::from(source_full.clone()),
        source_short: SharedString::from(shorten_middle(&source_full, 32)),
        destination: SharedString::from(destination_full.clone()),
        destination_short: SharedString::from(shorten_middle(&destination_full, 42)),
        difficulties: SharedString::from(
            entry
                .metadata
                .as_ref()
                .map(|m| m.difficulties.join(", "))
                .unwrap_or_default(),
        ),
        status: SharedString::from(entry.status.as_display()),
        status_badge_color: status_badge_color(&entry.status).into(),
        message: SharedString::from(info_message),
        error_short: SharedString::from(error_short),
        error_detail: SharedString::from(error_detail),
        thumbnail: image,
        show_delete,
        can_delete_source: can_delete,
        delete_hint: SharedString::from(delete_hint),
        can_import,
        can_reimport,
        can_ignore,
        audio_available,
        audio_playing,
        audio_status: SharedString::from(audio_status),
        audio_enabled,
        preview_enabled,
    }
}

fn status_badge_color(status: &ImportStatus) -> Color {
    match status {
        ImportStatus::Importing => Color::from_rgb_u8(93, 139, 255),
        ImportStatus::Completed => Color::from_rgb_u8(92, 193, 146),
        ImportStatus::DuplicateSkipped => Color::from_rgb_u8(245, 192, 107),
        ImportStatus::Failed => Color::from_rgb_u8(228, 123, 123),
        ImportStatus::ReadingMetadata | ImportStatus::WaitingStable => Color::from_rgb_u8(126, 138, 168),
        ImportStatus::Detected => Color::from_rgb_u8(110, 120, 140),
    }
}

fn shorten_middle(text: &str, max_len: usize) -> String {
    let len = text.chars().count();
    if len <= max_len {
        return text.to_string();
    }
    let head = (max_len.saturating_sub(3)) / 2;
    let tail = max_len.saturating_sub(3) - head;
    let start = text.chars().take(head).collect::<String>();
    let end = text.chars().rev().take(tail).collect::<String>();
    format!("{}...{}", start, end.chars().rev().collect::<String>())
}

fn audio_status_ui(entry: &BeatmapEntry) -> (String, bool, bool, bool) {
    let has_audio_meta = entry
        .metadata
        .as_ref()
        .and_then(|m| m.audio_file.as_ref())
        .is_some();
    let playing = matches!(entry.audio.status, AudioPreviewStatus::Playing);
    let enabled = entry.metadata.is_some() && !matches!(entry.audio.status, AudioPreviewStatus::Unavailable);
    let available = has_audio_meta && !matches!(entry.audio.status, AudioPreviewStatus::Unavailable);
    let status = match entry.audio.status {
        AudioPreviewStatus::Playing => "Tocando".to_string(),
        AudioPreviewStatus::Paused => "Pausado".to_string(),
        AudioPreviewStatus::Ready => "Pronto para tocar".to_string(),
        AudioPreviewStatus::Loading => "Carregando preview...".to_string(),
        AudioPreviewStatus::Unavailable => entry
            .audio
            .last_error
            .clone()
            .unwrap_or_else(|| "Sem audio".into()),
        AudioPreviewStatus::Unknown => {
            if has_audio_meta {
                "Aguardando metadados".into()
            } else {
                "Sem audio".into()
            }
        }
    };
    (status, available, playing, enabled)
}

fn update_audio_state(
    entry: &mut BeatmapEntry,
    entries: &Arc<Mutex<HashMap<u64, BeatmapEntry>>>,
    ui_sender: &mpsc::Sender<UiMsg>,
    status: AudioPreviewStatus,
    cached_path: Option<PathBuf>,
    last_error: Option<String>,
) {
    if let Some(path) = cached_path.clone() {
        entry.audio.cached_path = Some(path);
    }
    entry.audio.status = status;
    entry.audio.last_error = last_error;
    if let Ok(mut guard) = entries.lock() {
        if let Some(stored) = guard.get_mut(&entry.id) {
            *stored = entry.clone();
        }
    }
    let _ = ui_sender.send(UiMsg::Upsert(entry.clone()));
}

fn ensure_osz_hash(entry: &mut BeatmapEntry) -> Option<String> {
    if let Some(h) = entry.osz_hash.clone() {
        return Some(h);
    }
    let mut file = std::fs::File::open(&entry.osz_path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 8 * 1024];
    loop {
        let read = file.read(&mut buf).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let hash = hasher.finalize().to_hex().to_string();
    entry.osz_hash = Some(hash.clone());
    Some(hash)
}

fn extract_audio_to_cache(entry: &BeatmapEntry, hash: &str, audio_name: &str) -> anyhow::Result<PathBuf> {
    let target_dir = cache::audio_cache_dir().join(hash);
    app_state::ensure_dir(&target_dir)?;
    let file_name = Path::new(audio_name)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| audio_name.to_string());
    let target_path = target_dir.join(file_name);
    if target_path.exists() {
        return Ok(target_path);
    }
    let file = fs::File::open(&entry.osz_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let lower_name = audio_name.to_lowercase();
    for i in 0..archive.len() {
        let mut item = archive.by_index(i)?;
        let name_in_zip = item.name().to_lowercase();
        let filename_only = Path::new(item.name())
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default()
            .to_lowercase();
        if name_in_zip.ends_with(&lower_name) || filename_only == lower_name {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&target_path)?;
            std::io::copy(&mut item, &mut out)?;
            return Ok(target_path);
        }
    }
    Err(anyhow::anyhow!("Audio nao encontrado dentro do .osz"))
}

fn resolve_audio_path(
    entry: &mut BeatmapEntry,
    cache_store: &CacheStore,
) -> anyhow::Result<PathBuf> {
    let audio_file = entry
        .metadata
        .as_ref()
        .and_then(|m| m.audio_file.as_ref().cloned())
        .ok_or_else(|| anyhow::anyhow!("Sem audio no beatmap"))?;
    if let Some(cached) = entry
        .audio
        .cached_path
        .as_ref()
        .filter(|p| p.exists())
    {
        return Ok(cached.clone());
    }
    if let Some(dest) = entry.destination.as_ref() {
        let candidate = dest.join(&audio_file);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    let hash = ensure_osz_hash(entry).ok_or_else(|| anyhow::anyhow!("Nao foi possivel calcular hash do .osz"))?;
    if let Some(cached) = cache_store.find_audio(&hash).filter(|p| p.exists()) {
        return Ok(cached);
    }
    let extracted = extract_audio_to_cache(entry, &hash, &audio_file)?;
    cache_store.register_audio(hash.clone(), extracted.clone());
    let _ = cache_store.save();
    Ok(extracted)
}

fn handle_audio_preview(
    mut entry: BeatmapEntry,
    entries: Arc<Mutex<HashMap<u64, BeatmapEntry>>>,
    ui_sender: mpsc::Sender<UiMsg>,
    cache_store: Arc<CacheStore>,
    player: AudioPlayer,
) {
    if entry.metadata.is_none() {
        update_audio_state(
            &mut entry,
            &entries,
            &ui_sender,
            AudioPreviewStatus::Unavailable,
            None,
            Some("Metadados pendentes".into()),
        );
        return;
    }
    update_audio_state(
        &mut entry,
        &entries,
        &ui_sender,
        AudioPreviewStatus::Loading,
        None,
        None,
    );
    match resolve_audio_path(&mut entry, &cache_store) {
        Ok(path) => {
            if entry.audio.cached_path.is_none() {
                entry.audio.cached_path = Some(path.clone());
            }
            match player.toggle(entry.id, &path) {
                Ok(status) => {
                    update_audio_state(&mut entry, &entries, &ui_sender, status, Some(path.clone()), None);
                }
                Err(err) => {
                    let _ = ui_sender.send(UiMsg::Log(
                        LogLevel::Error,
                        format!("{}: falha ao tocar preview ({err:#})", entry.source_file_name()),
                    ));
                    update_audio_state(
                        &mut entry,
                        &entries,
                        &ui_sender,
                        AudioPreviewStatus::Unavailable,
                        None,
                        Some("Falha ao tocar audio".into()),
                    );
                }
            }
        }
        Err(err) => {
            let _ = ui_sender.send(UiMsg::Log(
                LogLevel::Warn,
                format!("{}: {}", entry.source_file_name(), err),
            ));
            update_audio_state(
                &mut entry,
                &entries,
                &ui_sender,
                AudioPreviewStatus::Unavailable,
                None,
                Some("Sem audio".into()),
            );
        }
    }
}

fn handle_preview_map(
    mut entry: BeatmapEntry,
    entries: Arc<Mutex<HashMap<u64, BeatmapEntry>>>,
    ui_sender: mpsc::Sender<UiMsg>,
    _cache_store: Arc<CacheStore>,
    _cfg: AppConfig,
) {
    if entry.metadata.is_none() {
        let _ = ui_sender.send(UiMsg::Log(
            LogLevel::Warn,
            format!(
                "{}: metadados ainda nao carregados para preview",
                entry.source_file_name()
            ),
        ));
        return;
    }
    let viewer_root = match locate_viewer_assets() {
        Ok(path) => path,
        Err(err) => {
            let _ = ui_sender.send(UiMsg::Log(
                LogLevel::Error,
                format!("Assets do viewer ausentes: {err}"),
            ));
            return;
        }
    };
    let prep = match prepare_preview_files(&mut entry, &ui_sender) {
        Ok(ok) => ok,
        Err(err) => {
            let _ = ui_sender.send(UiMsg::Log(
                LogLevel::Error,
                format!("{}: falha ao preparar preview ({err:#})", entry.source_file_name()),
            ));
            return;
        }
    };
    if let Ok(mut guard) = entries.lock() {
        if let Some(stored) = guard.get_mut(&entry.id) {
            *stored = entry.clone();
        }
    }
    let server = match preview::ensure_server(viewer_root, cache::preview_dir()) {
        Ok(s) => s,
        Err(err) => {
            let _ = ui_sender.send(UiMsg::Log(
                LogLevel::Error,
                format!("Nao foi possivel iniciar servidor de preview: {err:#}"),
            ));
            return;
        }
    };
    let url = format!(
        "http://127.0.0.1:{}/viewer/index.html?map=/beatmaps/{}/beatmap.osz&title={}",
        server.port,
        prep.hash,
        encode(&prep.title)
    );
    let _ = ui_sender.send(UiMsg::Log(
        LogLevel::Info,
        format!(
            "Preview: porta {}, cache {}, origem {}, url {}",
            server.port,
            prep.folder.display(),
            format_preview_origin(&prep.origin),
            url
        ),
    ));
    match open_preview_url(&url) {
        Ok(_) => {
            let _ = ui_sender.send(UiMsg::Log(
                LogLevel::Info,
                format!("Preview do beatmap aberto no navegador ({})", prep.title),
            ));
        }
        Err(err) => {
            let _ = ui_sender.send(UiMsg::Log(
                LogLevel::Warn,
                format!("Nao foi possivel abrir o navegador para preview: {err}"),
            ));
        }
    }
}

struct PreviewReady {
    hash: String,
    title: String,
    folder: PathBuf,
    origin: PreviewOrigin,
}

#[derive(Debug, Clone)]
enum PreviewOrigin {
    Cached(PathBuf),
    CopiedSource(PathBuf),
    ZippedDestination(PathBuf),
}

fn format_preview_origin(origin: &PreviewOrigin) -> String {
    match origin {
        PreviewOrigin::Cached(p) => format!("cache ({})", p.display()),
        PreviewOrigin::CopiedSource(p) => format!("fonte ({})", p.display()),
        PreviewOrigin::ZippedDestination(p) => format!("destino ({})", p.display()),
    }
}

fn prepare_preview_files(
    entry: &mut BeatmapEntry,
    ui_sender: &mpsc::Sender<UiMsg>,
) -> anyhow::Result<PreviewReady> {
    let hash = ensure_osz_hash(entry).ok_or_else(|| anyhow::anyhow!("hash do .osz ausente"))?;
    let base = cache::preview_dir().join(&hash);
    app_state::ensure_dir(&base)?;
    let osz_target = base.join("beatmap.osz");
    let origin = if !osz_target.exists() {
        if entry.osz_path.exists() {
            fs::copy(&entry.osz_path, &osz_target).with_context(|| {
                format!(
                    "copiando {} para cache de preview",
                    entry.source_file_name()
                )
            })?;
            PreviewOrigin::CopiedSource(entry.osz_path.clone())
        } else if let Some(dest) = entry.destination.as_ref().filter(|p| p.exists()) {
            zip_destination_for_preview(dest, &osz_target).with_context(|| {
                format!("compactando destino {:?} para preview", dest)
            })?;
            PreviewOrigin::ZippedDestination(dest.clone())
        } else {
            return Err(anyhow::anyhow!(
                "Arquivo de origem e destino indisponiveis para preview"
            ));
        }
    } else {
        PreviewOrigin::Cached(osz_target.clone())
    };
    let extract_target = base.join("extracted");
    if !extract_target.exists() {
        if let Err(err) = extract_osz_for_preview(&osz_target, &extract_target) {
            let _ = ui_sender.send(UiMsg::Log(
                LogLevel::Warn,
                format!(
                    "{}: extracao parcial para preview ({err:#})",
                    entry.source_file_name()
                ),
            ));
        }
    }
    let title = entry
        .metadata
        .as_ref()
        .map(|m| m.display_title())
        .unwrap_or_else(|| entry.source_file_name());
    Ok(PreviewReady {
        hash,
        title,
        folder: base,
        origin,
    })
}

fn extract_osz_for_preview(osz_path: &Path, target_dir: &Path) -> anyhow::Result<()> {
    app_state::ensure_dir(target_dir)?;
    let file = fs::File::open(osz_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut item = archive.by_index(i)?;
        let safe = safe_preview_path(target_dir, item.name())?;
        if item.is_dir() {
            fs::create_dir_all(&safe)?;
            continue;
        }
        if let Some(parent) = safe.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&safe)?;
        std::io::copy(&mut item, &mut out)?;
    }
    Ok(())
}

fn zip_destination_for_preview(source_dir: &Path, target_file: &Path) -> anyhow::Result<()> {
    if !source_dir.is_dir() {
        return Err(anyhow::anyhow!("Destino para preview nao existe"));
    }
    if let Some(parent) = target_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(target_file)?;
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut writer = zip::ZipWriter::new(file);
    let mut dirs = vec![source_dir.to_path_buf()];
    let mut buffer = Vec::new();
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let rel = path
                .strip_prefix(source_dir)
                .unwrap_or(path.as_path())
                .to_path_buf();
            if path.is_dir() {
                dirs.push(path);
                let name = format!("{}/", rel.to_string_lossy().replace('\\', "/"));
                writer.add_directory(name, options)?;
                continue;
            }
            let name = rel.to_string_lossy().replace('\\', "/");
            writer.start_file(name, options)?;
            let mut f = fs::File::open(&path)?;
            f.read_to_end(&mut buffer)?;
            writer.write_all(&buffer)?;
            buffer.clear();
        }
    }
    writer.finish()?;
    Ok(())
}

fn safe_preview_path(base: &Path, inside_zip: &str) -> anyhow::Result<PathBuf> {
    let mut clean = PathBuf::new();
    for comp in Path::new(inside_zip).components() {
        match comp {
            Component::Normal(s) => {
                clean.push(app_state::sanitize_path_component(&s.to_string_lossy()))
            }
            Component::CurDir => {}
            _ => return Err(anyhow::anyhow!("Entrada de ZIP com caminho invalido")),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(anyhow::anyhow!("Entrada de ZIP vazia"));
    }
    Ok(base.join(clean))
}

fn locate_viewer_assets() -> anyhow::Result<PathBuf> {
    let candidate = PathBuf::from("assets/viewer");
    if candidate.join("index.html").exists() {
        return Ok(candidate);
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(parent) = exe.parent() {
            let alt = parent.join("assets/viewer");
            if alt.join("index.html").exists() {
                return Ok(alt);
            }
        }
    }
    Err(anyhow::anyhow!(
        "assets/viewer/index.html nao encontrado"
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewLaunch {
    pub program: Option<String>,
    pub args: Vec<String>,
}

fn build_preview_launches(url: &str) -> Vec<PreviewLaunch> {
    let mut plans = Vec::new();
    #[cfg(target_os = "windows")]
    {
        for browser in ["msedge", "chrome"] {
            plans.push(PreviewLaunch {
                program: Some(browser.to_string()),
                args: vec![format!("--app={url}")],
            });
        }
    }
    plans.push(PreviewLaunch {
        program: None,
        args: vec![url.to_string()],
    });
    plans
}

fn open_preview_url(url: &str) -> std::io::Result<()> {
    let mut last_err: Option<std::io::Error> = None;
    for plan in build_preview_launches(url) {
        if let Some(program) = plan.program.as_ref() {
            match Command::new(program).args(&plan.args).spawn() {
                Ok(_) => return Ok(()),
                Err(err) => last_err = Some(err),
            }
        } else {
            return open::that(url);
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::Other, "Nenhum navegador encontrado")
    }))
}

fn to_log_item(level: LogLevel, msg: &str) -> LogItem {
    let lvl_str = match level {
        LogLevel::Info => "INFO",
        LogLevel::Warn => "WARN",
        LogLevel::Error => "ERROR",
    };
    LogItem {
        level: SharedString::from(lvl_str),
        text: SharedString::from(msg),
    }
}

#[cfg(test)]
static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod safety_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn enforce_safety_disables_flags_when_conflict() {
        let mut cfg = AppConfig {
            downloads_dir: PathBuf::from("C:/dl"),
            songs_dir: PathBuf::from("C:/dl/Songs"),
            auto_import: true,
            auto_delete_source: true,
            suppress_delete_prompt: false,
            stability: app_state::StabilityConfig::default(),
        };
        let warning = enforce_path_safety(&mut cfg);
        assert!(warning.is_some());
        assert!(!cfg.auto_import);
        assert!(!cfg.auto_delete_source);
    }
}

#[cfg(test)]
mod audio_resolution_tests {
    use super::*;
    use std::env;
    use std::ffi::OsString;
    use std::io::Write;
    use std::time::SystemTime;
    use tempfile::tempdir;
    use zip::write::FileOptions;

    fn build_osz_with_audio(path: &Path, audio_name: &str) {
        let file = fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let osu = format!(
            "[Metadata]\nTitle:Test\nArtist:Artist\nCreator:Mapper\nVersion:Hard\nBeatmapSetID:1\n\n[General]\nAudioFilename:{}\n",
            audio_name
        );
        writer.start_file("test.osu", opts).unwrap();
        writer.write_all(osu.as_bytes()).unwrap();
        writer.start_file(audio_name, opts).unwrap();
        writer.write_all(b"audio-bytes").unwrap();
        writer.finish().unwrap();
    }

    fn restore_env(name: &str, val: Option<OsString>) {
        unsafe {
            if let Some(v) = val {
                env::set_var(name, v);
            } else {
                env::remove_var(name);
            }
        }
    }

    #[test]
    fn resolve_audio_prefers_destination_and_falls_back_to_cache() {
        let _lock = super::ENV_GUARD.lock().unwrap();
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let old_home = env::var_os("HOME");
        let old_local = env::var_os("LOCALAPPDATA");
        unsafe {
            env::set_var("HOME", &home);
            env::set_var("LOCALAPPDATA", &home);
        }

        let osz_path = tmp.path().join("map.osz");
        build_osz_with_audio(&osz_path, "song.mp3");
        let dest_dir = tmp.path().join("Songs").join("Map");
        fs::create_dir_all(&dest_dir).unwrap();
        let dest_audio = dest_dir.join("song.mp3");
        fs::write(&dest_audio, b"dest audio").unwrap();

        let metadata = app_state::BeatmapMetadata {
            title: "Title".into(),
            artist: "Artist".into(),
            creator: "Creator".into(),
            difficulties: vec!["Easy".into()],
            beatmap_set_id: Some(1),
            beatmap_ids: vec![11],
            background_file: None,
            audio_file: Some("song.mp3".into()),
        };

        let mut entry = BeatmapEntry {
            id: 1,
            osz_path: osz_path.clone(),
            status: ImportStatus::Detected,
            message: None,
            error_detail: None,
            error_short: None,
            metadata: Some(metadata),
            thumbnail_path: None,
            detected_at: SystemTime::now(),
            destination: Some(dest_dir.clone()),
            osz_hash: None,
            audio: app_state::AudioPreview::default(),
        };

        let cache_store = CacheStore::load();
        let from_dest = resolve_audio_path(&mut entry, &cache_store).unwrap();
        assert_eq!(from_dest, dest_audio);

        fs::remove_file(&dest_audio).unwrap();
        entry.audio.cached_path = None;
        let from_cache = resolve_audio_path(&mut entry, &cache_store).unwrap();
        assert!(from_cache.exists());
        assert!(from_cache.starts_with(cache::audio_cache_dir()));

        restore_env("HOME", old_home);
        restore_env("LOCALAPPDATA", old_local);
    }
}

#[cfg(test)]
mod preview_launch_tests {
    use super::*;

    #[test]
    fn build_preview_launches_orders_app_mode_first() {
        let url = "http://localhost:1234/test";
        let plans = build_preview_launches(url);
        #[cfg(target_os = "windows")]
        {
            assert!(plans.len() >= 3);
            assert_eq!(plans[0].program.as_deref(), Some("msedge"));
            assert_eq!(plans[0].args, vec![format!("--app={url}")]);
            assert_eq!(plans[1].program.as_deref(), Some("chrome"));
            assert_eq!(plans.last().unwrap().program, None);
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(plans.len(), 1);
            assert_eq!(plans[0].program, None);
            assert_eq!(plans[0].args, vec![url.to_string()]);
        }
    }
}

#[cfg(test)]
mod preview_prepare_tests {
    use super::*;
    use std::env;
    use std::ffi::OsString;
    use std::time::SystemTime;
    use tempfile::tempdir;
    use zip::ZipArchive;

    fn restore_env(name: &str, val: Option<OsString>) {
        unsafe {
            if let Some(v) = val {
                env::set_var(name, v);
            } else {
                env::remove_var(name);
            }
        }
    }

    #[test]
    fn prepare_preview_uses_destination_when_source_missing() {
        let _lock = super::ENV_GUARD.lock().unwrap();
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let old_home = env::var_os("HOME");
        let old_local = env::var_os("LOCALAPPDATA");
        unsafe {
            env::set_var("HOME", &home);
            env::set_var("LOCALAPPDATA", &home);
        }

        let dest_dir = tmp.path().join("Songs").join("Imported");
        std::fs::create_dir_all(&dest_dir).unwrap();
        std::fs::write(dest_dir.join("map.osu"), "osu data").unwrap();
        std::fs::write(dest_dir.join("audio.mp3"), "audio").unwrap();

        let metadata = app_state::BeatmapMetadata {
            title: "Title".into(),
            artist: "Artist".into(),
            creator: "Creator".into(),
            difficulties: vec!["Easy".into()],
            beatmap_set_id: Some(1),
            beatmap_ids: vec![11],
            background_file: None,
            audio_file: Some("audio.mp3".into()),
        };

        let mut entry = BeatmapEntry {
            id: 1,
            osz_path: tmp.path().join("missing.osz"),
            status: ImportStatus::Detected,
            message: None,
            error_detail: None,
            error_short: None,
            metadata: Some(metadata),
            thumbnail_path: None,
            detected_at: SystemTime::now(),
            destination: Some(dest_dir.clone()),
            osz_hash: Some("deadbeef".into()),
            audio: app_state::AudioPreview::default(),
        };

        let (tx, _rx) = mpsc::channel();
        let prep = prepare_preview_files(&mut entry, &tx).unwrap();
        let osz_file = prep.folder.join("beatmap.osz");
        assert_eq!(prep.hash, "deadbeef");
        assert!(osz_file.exists());
        assert!(prep.folder.join("extracted").exists());

        let mut archive = ZipArchive::new(std::fs::File::open(&osz_file).unwrap()).unwrap();
        let mut names = Vec::new();
        for i in 0..archive.len() {
            names.push(archive.by_index(i).unwrap().name().to_string());
        }
        assert!(names.iter().any(|n| n.contains("map.osu")));
        assert!(names.iter().any(|n| n.contains("audio.mp3")));

        restore_env("HOME", old_home);
        restore_env("LOCALAPPDATA", old_local);
    }
}

fn seed_existing_osz(dir: &Path, tx: &mpsc::Sender<CommandMsg>) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .map(|e| e.to_string_lossy().eq_ignore_ascii_case("osz"))
            .unwrap_or(false)
        {
            let _ = tx.send(CommandMsg::AddFile(path));
        }
    }
    Ok(())
}

fn open_in_explorer(path: &PathBuf) {
    #[cfg(target_os = "windows")]
    let _ = Command::new("explorer").arg(path).spawn();
    #[cfg(not(target_os = "windows"))]
    let _ = Command::new("xdg-open").arg(path).spawn();
}

fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        Command::new("rundll32.exe")
            .arg("url.dll,FileProtocolHandler")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new("xdg-open").arg(url).spawn().map(|_| ())
    }
}

fn open_in_browser(set_id: i32) -> std::io::Result<()> {
    let url = format!("https://osu.ppy.sh/beatmapsets/{set_id}");
    open_url(&url)
}

fn fetch_nerinyan(query: &str) -> anyhow::Result<Vec<BeatmapFound>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("McOsuImporter/beatmap-search")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let encoded_query = encode(query);
    let url = format!("https://api.nerinyan.moe/search?q={}", encoded_query);
    let resp = client.get(&url).send()?;

    if !resp.status().is_success() {
        anyhow::bail!("Falha na busca Nerinyan: Status HTTP {}", resp.status());
    }

    // Lê o corpo da resposta como texto primeiro
    let body_text = resp.text()?;

    // Tenta converter o texto para a nossa struct
    match serde_json::from_str::<Vec<NerinyanBeatmap>>(&body_text) {
        Ok(beatmaps) => {
            // Se funcionar, continua normalmente
            { // Abre um novo escopo para o log
                if let Ok(mut log_file) = OpenOptions::new().append(true).open("logs/search_log.txt") {
                    writeln!(log_file, "--- DIAGNÓSTICO PRÉ-FILTRO ---").unwrap();
                    writeln!(log_file, "Inspecionando {} beatmaps recebidos da API:", beatmaps.len()).unwrap();
                    for b in &beatmaps {
                        writeln!(log_file, "  - ID: {}, Título: '{}', Modo: {:?}", b.set_id, b.title, b.mode).unwrap();
                    }
                    writeln!(log_file, "--- FIM DO DIAGNÓSTICO PRÉ-FILTRO ---").unwrap();
                }
            }
            let items = beatmaps
                .into_iter()
                // O filtro foi removido. Agora apenas descartamos mapas com ID inválido.
                .filter(|b| b.set_id > 0)
                .map(|b| BeatmapFound {
                    title: b.title,
                    artist: b.artist,
                    creator: b.creator,
                    source: BeatmapSource::Nerinyan,
                    download_url: format!("https://api.nerinyan.moe/d/{}", b.set_id),
                })
                .collect();
            return Ok(items); // Retorna o sucesso imediatamente
        },
        Err(e) => {
            // Se a conversão falhar, IMPRIME o erro e o corpo que causou a falha
            eprintln!("--- ERRO FATAL DE DESSERIALIZAÇÃO (Nerinyan) ---");
            eprintln!("O erro do Serde foi: {:?}", e);
            eprintln!("\nO corpo da resposta que causou o erro foi:\n---\n{}\n---", body_text);

            // --- ADIÇÃO CRÍTICA PARA LOGGING ---
            if let Ok(mut log_file) = OpenOptions::new().append(true).open("logs/search_log.txt") {
                writeln!(log_file, "--- ERRO FATAL DE DESSERIALIZAÇÃO (Nerinyan) ---").ok();
                writeln!(log_file, "O erro do Serde foi: {:?}", e).ok();
                writeln!(
                    log_file,
                    "\nO corpo da resposta que causou o erro foi:\n---\n{}\n---",
                    body_text
                )
                .ok();
            }
            // --- FIM DA ADIÇÃO ---

            anyhow::bail!("O formato da resposta da API Nerinyan era inválido.")
        }
    }
}

fn fetch_catboy(query: &str) -> anyhow::Result<Vec<BeatmapFound>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("McOsuImporter/beatmap-search")
        .build()?;
    let encoded_query = urlencoding::encode(query);
    let url = format!("https://catboy.best/api/v2/search?q={}", encoded_query);
    println!("--- URL SENDO CHAMADA: {} ---", url);
    // Envia a requisição e trata erros de conexão (DNS, etc.)
    let resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Erro de conexão ao tentar buscar em Catboy.best: {:?}", e);
            anyhow::bail!("Falha ao enviar requisição para Catboy.best");
        }
    };

    // Verifica se o status da resposta HTTP é um sucesso (ex: 200 OK)
    if !resp.status().is_success() {
        let status = resp.status();
        let error_text = resp.text().unwrap_or_else(|_| "Falha ao ler o corpo do erro.".to_string());
        anyhow::bail!("Falha na busca Catboy.best: Status HTTP {} - {}", status, error_text);
    }

    // --- MUDANÇA CRÍTICA: SEPARAÇÃO DAS ETAPAS ---
    // Etapa 1: Ler o corpo inteiro da resposta como uma String de texto.
    let body_text = match resp.text() {
        Ok(text) => text,
        Err(e) => {
            eprintln!("Erro ao ler o corpo da resposta como texto: {:?}", e);
            anyhow::bail!("Falha ao ler o corpo da resposta da API.");
        }
    };

    // Etapa 2: Tentar desserializar (converter) a String de texto para nossas structs.
    // Esta é a única fonte possível do erro "invalid type: map, expected a sequence".
    match serde_json::from_str::<CatboyApiResponse>(&body_text) {
        Ok(api_response) => {
            // Se a conversão foi um sucesso, mapeamos os resultados.
            let items = api_response
                .results
                .into_iter()
                .map(|b| {
                    let download_url = format!("https://catboy.best/d/{}", b.set_id);
                    BeatmapFound {
                        title: b.title,
                        artist: b.artist,
                        creator: b.creator,
                        source: BeatmapSource::Catboy,
                        download_url,
                    }
                })
                .collect();
            
            Ok(items)
        },
        Err(e) => {
            // Se a conversão falhou, imprimimos o erro E o corpo que causou a falha.
            eprintln!("--- ERRO FATAL DE DESSERIALIZAÇÃO ---");
            eprintln!("O erro do Serde foi: {:?}", e);
            eprintln!("\nO corpo da resposta que causou o erro foi:\n---\n{}\n---", body_text);
            anyhow::bail!("O formato da resposta da API Catboy era inválido.")
        }
    }
}
fn build_osz_name(result: &BeatmapSearchResult) -> String {
    let mut name = format!("{} - {} ({})", result.artist, result.title, result.creator);
    name = app_state::sanitize_path_component(&name);
    if !name.to_lowercase().ends_with(".osz") {
        name.push_str(".osz");
    }
    name
}

fn ensure_unique_path(base_dir: &Path, filename: &str) -> PathBuf {
    let mut candidate = base_dir.join(filename);
    let mut counter = 1;
    while candidate.exists() {
        let stem = candidate
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("beatmap");
        let ext = candidate.extension().and_then(|s| s.to_str()).unwrap_or("");
        let new_name = if ext.is_empty() {
            format!("{stem} ({counter})")
        } else {
            format!("{stem} ({counter}).{ext}")
        };
        candidate = base_dir.join(new_name);
        counter += 1;
    }
    candidate
}

fn download_with_progress<F>(
    client: &reqwest::blocking::Client,
    url: &str,
    temp_path: &Path,
    final_path: &Path,
    progress: F,
) -> anyhow::Result<()>
where
    F: Fn(u64, Option<u64>),
{
    let res = (|| {
        if let Some(parent) = temp_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut resp = client.get(url).send()?.error_for_status()?;
        let total = resp.content_length();
        let mut file = std::fs::File::create(temp_path)?;
        let mut buf = [0u8; 32 * 1024];
        let mut downloaded = 0u64;
        loop {
            let n = resp.read(&mut buf)?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            downloaded += n as u64;
            progress(downloaded, total);
        }
        file.flush()?;
        std::fs::rename(temp_path, final_path)?;
        Ok::<(), anyhow::Error>(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(temp_path);
    }
    res
}

fn beatmap_source_label(source: &BeatmapSource) -> &'static str {
    match source {
        BeatmapSource::Catboy => "Catboy.best",
        BeatmapSource::Nerinyan => "Nerinyan",
    }
}

fn to_search_item(result: &BeatmapSearchResult) -> BeatmapSearchItem {
    let source_label = beatmap_source_label(&result.source);
    BeatmapSearchItem {
        id: result.id as i32,
        title: SharedString::from(&result.title),
        artist_mapper: SharedString::from(format!("{} | {}", result.artist, result.creator)),
        source: SharedString::from(source_label),
    }
}
