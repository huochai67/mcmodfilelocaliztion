use dashmap::DashMap;
use serde::Deserialize;

// --- Modrinth API 数据清洗 ---
#[derive(Debug, Deserialize, Clone)]
pub struct ModrinthProject {
    pub client_side: String,
    pub server_side: String,
    pub categories: Vec<String>,
}

#[derive(Debug)]
pub struct ModrinthApi {
    endpoint: String,
    http_client: reqwest::Client,
    // 缓存 API 结果，避免同个 mod 多次请求
    api_cache: DashMap<String, Option<ModrinthProject>>,
}

impl ModrinthApi {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            http_client: reqwest::Client::new(),
            api_cache: DashMap::new(),
        }
    }

    pub async fn get_modrinth_data(&self, mod_id: &str) -> Option<ModrinthProject> {
        if let Some(cached) = self.api_cache.get(mod_id) {
            return cached.clone();
        }
        let url = format!("{}/project/{}", self.endpoint, mod_id);
        let res = self.http_client.get(url).send().await.ok()?;

        let data = if res.status().is_success() {
            res.json::<ModrinthProject>().await.ok()
        } else {
            None
        };

        self.api_cache.insert(mod_id.to_string(), data.clone());
        data
    }
}
