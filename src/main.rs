use anyhow::{Context, Result};
use clap::Parser;
use mcmodfilelocaliztion::mcmoddb::ModTranslationDb;
use mcmodfilelocaliztion::modrinthapi::ModrinthApi;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct NeoForgeConfig {
    mods: Vec<NeoForgeModInfo>,
}

#[derive(Debug, Deserialize)]
struct NeoForgeModInfo {
    #[serde(rename = "modId")]
    mod_id: String,
    version: String,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

struct ModInfo {
    mod_id: String,
    display_name: Option<String>,
    version: String,
}

// --- 核心工具函数 ---
fn extract_manifest_version(archive: &mut zip::ZipArchive<File>) -> Result<String> {
    let manifest_file = archive.by_name("META-INF/MANIFEST.MF")?;
    let reader = BufReader::new(manifest_file);
    for line in reader.lines() {
        let line = line?;
        if line.starts_with("Implementation-Version:") {
            return Ok(line.splitn(2, ':').nth(1).unwrap_or("").trim().to_string());
        }
    }
    Err(anyhow::anyhow!("No version in manifest"))
}
async fn get_mod_info(path: PathBuf) -> Result<ModInfo> {
    let file = File::open(&path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    // 1. 解析 TOML
    let toml_str = {
        let mut f = archive.by_name("META-INF/neoforge.mods.toml")?;
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        s
    };
    let config: NeoForgeConfig = toml::from_str(&toml_str)?;
    let m = config.mods.first().context("No mod info")?;

    // 2. 版本处理
    let version = if m.version == "${file.jarVersion}" {
        extract_manifest_version(&mut archive).unwrap_or_else(|_| "unknown".to_string())
    } else {
        m.version.clone()
    };

    Ok(ModInfo {
        mod_id: m.mod_id.clone(),
        display_name: m.display_name.clone(),
        version,
    })
}

async fn process_file(path: PathBuf, state: Arc<AppState>) -> Result<()> {
    if state.verbose {
        println!("--- Processing file: {:?} ---", path.file_name().unwrap());
    }
    let modinfo = get_mod_info(path.clone()).await?;
    if state.verbose {
        println!(
            "DisplayName: {} (ModID: {}, Version: {})",
            modinfo
                .display_name
                .clone()
                .unwrap_or_else(|| modinfo.mod_id.clone()),
            modinfo.mod_id,
            modinfo.version
        );
    }

    // 3. 多源名称 (DB > DisplayName > ModId)
    let db_name = state.db_pool.get_chinese_name(&modinfo.mod_id).await;

    // if db_name.is_none() && state.verbose {
    //     println!(
    //         "\tNo DB name for {}, using display name or modId, it also mean thie mod may not found in Modrinth, so no side/category tag will be added",
    //         modinfo.mod_id
    //     );
    // }
    let final_name = db_name.clone()
        .or(modinfo.display_name.clone())
        .unwrap_or_else(|| modinfo.mod_id.clone());

    // 4. Modrinth 数据整合
    let mut side_tag = String::new();
    let mut category_tag = String::new();
    if let Some(info) = state.modrinth_api.get_modrinth_data(&modinfo.mod_id).await {
        // 构建端位标签
        let c = match info.client_side.as_str() {
            "unsupported" => -1,
            "optional" => 0,
            "required" => 1,
            _ => -99,
        };
        let s = match info.server_side.as_str() {
            "unsupported" => -1,
            "optional" => 0,
            "required" => 1,
            _ => -99,
        };
        side_tag = match (c, s) {
            (1, 1) => "[C&S]".to_string(),
            (0, 0) => "[C|S]".to_string(),
            (-1, -1) => "[Toxic]".to_string(),
            (1, 0) => "[C]".to_string(),
            (0, 1) => "[S]".to_string(),
            (1, -1) => "[!S]".to_string(),
            (-1, 1) => "[!C]".to_string(),
            _ => "".to_string(),
        };

        // 构建分类标签 (使用翻译 Map)
        let translated_cats: Vec<String> = info
            .categories
            .iter()
            .map(|cat| state.category_map.get(cat).cloned().unwrap_or(cat.clone()))
            .collect();
        if !translated_cats.is_empty() {
            category_tag = format!("[{}]", translated_cats.join("]["));
        }
    }

    // 5. 重命名
    let new_name = format!(
        "{}{}{}-{}.jar",
        side_tag, category_tag, final_name, modinfo.version
    );
    let safe_name = new_name.replace(
        |c: char| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'),
        "",
    );

    let mut new_path = path.clone();
    new_path.set_file_name(safe_name);

    if state.verbose {
        println!("Found in DB: {}, Modrinth: {}", !db_name.is_none(), !category_tag.is_empty());
        println!(
            "{:?} is renaming to: {:?}",
            path.file_name().unwrap(),
            new_path.file_name().unwrap()
        );
    }
    if path != new_path && !new_path.exists() {
        fs::rename(&path, &new_path)?;
        println!("Renamed: {:?}", new_path.file_name().unwrap());
    }

    Ok(())
}

struct AppState {
    db_pool: ModTranslationDb,
    category_map: HashMap<String, String>,
    modrinth_api: ModrinthApi,
    verbose: bool,
}

// --- 命令行配置 ---
#[derive(Parser, Debug)]
struct Args {
    /// 需要扫描的文件夹路径
    #[arg(short, long, default_value = "./mods")]
    path: String,

    /// 数据库下载链接
    #[arg(
        short,
        long,
        default_value = "https://raw.githubusercontent.com/PCL-Community/PCL2-CE/refs/heads/dev/Plain%20Craft%20Launcher%202/Resources/ModData.dbcp"
    )]
    url: String,

    /// Modrinth API 端点 (默认 v2)
    #[arg(short, long, default_value = "https://api.modrinth.com/v2")]
    api_endpoint: String,

    /// 数据库本地存储名称
    #[arg(short, long, default_value = "ModData.db")]
    db_name: String,

    /// Verbose 模式，输出更多调试信息
    #[arg(short, long)]
    verbose: bool,
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // --- 1. 加载分类翻译 Map ---
    let category_map_path = "categories.json";
    let category_map: HashMap<String, String> = if Path::new(category_map_path).exists() {
        let content = fs::read_to_string(category_map_path).context("读取 categories.json 失败")?;
        serde_json::from_str(&content).context("解析 categories.json 失败")?
    } else {
        println!("未发现 categories.json，将使用原始英文标签。");
        HashMap::new() // 如果文件不存在，返回空的 Map
    };
    // --- 2. 初始化数据库 ---
    let db = ModTranslationDb::init(&args.url, &args.db_name).await?;
    let modrinth_api = ModrinthApi::new(&args.api_endpoint);
    println!("数据库和 API 初始化完成，开始处理文件夹: {}", args.path);

    let state = Arc::new(AppState {
        db_pool: db,
        category_map,
        modrinth_api,
        verbose: args.verbose,
    });

    let folder = Path::new(&args.path);
    if !folder.is_dir() {
        return Err(anyhow::anyhow!("Path is not a dir"));
    }
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().map_or(false, |e| e == "jar") {
            let state_clone = Arc::clone(&state);
            // 这里为了简单使用了顺序处理，如果要极大提速，可以使用 tokio::spawn
            // 但考虑到 Modrinth API 的速率限制 (Rate Limit)，顺序处理其实更稳妥
            if let Err(e) = process_file(path, state_clone).await {
                eprintln!("Error processing: {}", e);
            }
        }
    }

    Ok(())
}
