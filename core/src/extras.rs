//! News (Haapi CMS) and server status — both public endpoints, no APIKEY.
//! Results are cached for 5 minutes so the UI can call freely without polling Ankama.
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(300);
const NEWS_URL: &str = "https://haapi.ankama.com/json/Ankama/v5/Cms/Items/Get";
const STATUS_URL: &str = "https://status.cdn.ankama.com/export.json";
/// Status tag of the Dofus game servers (Ankama kept the legacy "dofus2" tag for Dofus 3 servers).
const DOFUS_SERVER_TAG: &str = "dofus2";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewsItem {
    pub id: i64,
    pub title: String,
    /// ISO 8601 UTC date derived from the CMS timestamp.
    pub date: String,
    pub url: String,
    pub image: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub name: String,
    /// "Up", "Down", "Maintenance"… as reported by Ankama.
    pub status: String,
    pub tags: Vec<String>,
}

#[derive(Deserialize)]
struct RawNews {
    id: i64,
    name: String,
    #[serde(default)]
    timestamp: i64,
    canonical_url: Option<String>,
    url: Option<String>,
    image_url: Option<String>,
}

#[derive(Deserialize, Clone)]
struct RawServer {
    #[serde(default)]
    tags: Vec<String>,
    names: std::collections::HashMap<String, String>,
    status: String,
}

struct Cached<T> {
    key: String,
    at: Instant,
    value: T,
}

static NEWS_CACHE: Mutex<Option<Cached<Vec<NewsItem>>>> = Mutex::new(None);
static STATUS_CACHE: Mutex<Option<Cached<Vec<RawServer>>>> = Mutex::new(None);


fn cached<T: Clone>(cache: &Mutex<Option<Cached<T>>>, key: &str) -> Option<T> {
    let guard = cache.lock().unwrap();
    guard
        .as_ref()
        .filter(|c| c.key == key && c.at.elapsed() < CACHE_TTL)
        .map(|c| c.value.clone())
}

fn store<T>(cache: &Mutex<Option<Cached<T>>>, key: &str, value: T) {
    *cache.lock().unwrap() = Some(Cached { key: key.to_string(), at: Instant::now(), value });
}

/// Latest Dofus news in `lang` (fr, en, es, de, pt).
pub async fn news(lang: &str, count: u32) -> anyhow::Result<Vec<NewsItem>> {
    let key = format!("{lang}:{count}");
    if let Some(v) = cached(&NEWS_CACHE, &key) {
        return Ok(v);
    }
    let count = count.to_string();
    let raw: Vec<RawNews> = crate::http::client()
        .get(NEWS_URL)
        .query(&[("template_key", "NEWS"), ("site", "DOFUS"), ("lang", lang), ("page", "1"), ("count", &count)])
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let items: Vec<NewsItem> = raw
        .into_iter()
        .map(|n| NewsItem {
            id: n.id,
            title: n.name,
            date: iso_utc(n.timestamp),
            url: n.canonical_url.or(n.url).unwrap_or_default(),
            image: n.image_url,
        })
        .collect();
    store(&NEWS_CACHE, &key, items.clone());
    Ok(items)
}

/// Dofus game servers status, names localized in `lang` (fallback fr).
pub async fn server_status(lang: &str) -> anyhow::Result<Vec<ServerStatus>> {
    let servers = match cached(&STATUS_CACHE, "all") {
        Some(v) => v,
        None => {
            let raw: Vec<RawServer> = crate::http::client()
                .get(STATUS_URL)
                .timeout(Duration::from_secs(10))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            store(&STATUS_CACHE, "all", raw.clone());
            raw
        }
    };
    Ok(servers
        .into_iter()
        .filter(|s| s.tags.iter().any(|t| t == DOFUS_SERVER_TAG))
        .map(|s| ServerStatus {
            name: s.names.get(lang).or_else(|| s.names.get("fr")).cloned().unwrap_or_default(),
            status: s.status,
            tags: s.tags,
        })
        .collect())
}

/// Unix seconds → `YYYY-MM-DDTHH:MM:SSZ` (civil-from-days, Howard Hinnant).
fn iso_utc(ts: i64) -> String {
    let (days, secs) = (ts.div_euclid(86_400), ts.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_791_968_400), "2026-10-14T09:00:00Z");
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn live_endpoints() {
        let n = news("fr", 3).await.unwrap();
        assert!(!n.is_empty());
        let s = server_status("fr").await.unwrap();
        assert!(s.iter().any(|s| s.name == "Draconiros"));
    }
}
