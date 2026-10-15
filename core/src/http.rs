//! Shared HTTP client. Every Ankama request carries `User-Agent: Zaap <version>`.
use std::sync::OnceLock;
use std::time::Duration;

pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(format!("Zaap {}", crate::ZAAP_VERSION))
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(16)
            .build()
            .expect("http client")
    })
}
