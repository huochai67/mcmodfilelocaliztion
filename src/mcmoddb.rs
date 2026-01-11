use anyhow::Result;
use flate2::read::GzDecoder;
use sqlx::{FromRow, SqlitePool};
use std::fs::File;
use std::io::{self};
use std::path::Path;

#[derive(Debug, FromRow)]
pub struct ModTranslation {
    pub ChineseName: String,
}

pub struct ModTranslationDb {
    pool: SqlitePool,
}

impl ModTranslationDb {
    pub async fn init(url: &str, db_path: &str) -> Result<Self> {
        let path = Path::new(db_path);
        if !path.exists() {
            let response = reqwest::get(url).await?.bytes().await?;
            let mut decoder = GzDecoder::new(&response[..]);
            let mut output_file = File::create(db_path)?;
            io::copy(&mut decoder, &mut output_file)?;
        }
        let connection_str = format!("sqlite:{}", db_path);
        let pool = SqlitePool::connect(&connection_str).await?;
        Ok(Self { pool })
    }

    pub async fn get_chinese_name(&self, modid: &str) -> Option<String> {
        // 尝试查询对应 modid 的中文名
        sqlx::query_as::<_, ModTranslation>(
            "SELECT ChineseName FROM ModTranslation WHERE CurseForgeSlug = ? OR ModrinthSlug = ? LIMIT 1",
        )
        .bind(modid)
        .bind(modid)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .map(|r| r.ChineseName)
    }
}
