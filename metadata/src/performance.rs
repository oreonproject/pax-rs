use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use crate::processed::ProcessedMetaData;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry<T> {
    pub data: T,
    pub created_at: u64,
    pub expires_at: Option<u64>,
}

impl<T> CacheEntry<T> {
    pub fn new(data: T, ttl: Option<Duration>) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            data,
            created_at: now,
            expires_at: ttl.map(|ttl| now + ttl.as_secs()),
        }
    }

    pub fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() > expires_at
        } else {
            false
        }
    }
}

pub struct Cache<T> {
    entries: Arc<RwLock<HashMap<String, CacheEntry<T>>>>,
    default_ttl: Option<Duration>,
}

impl<T: Clone> Cache<T> {
    pub fn new(default_ttl: Option<Duration>) -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            default_ttl,
        }
    }

    pub fn get(&self, key: &str) -> Option<T> {
        let entries = self.entries.read().ok()?;
        let entry = entries.get(key)?;
        
        if entry.is_expired() {
            drop(entries);
            self.remove(key);
            return None;
        }
        
        Some(entry.data.clone())
    }

    pub fn set(&self, key: String, value: T) {
        self.set_with_ttl(key, value, self.default_ttl);
    }

    pub fn set_with_ttl(&self, key: String, value: T, ttl: Option<Duration>) {
        if let Ok(mut entries) = self.entries.write() {
            let entry = CacheEntry::new(value, ttl);
            entries.insert(key, entry);
        }
    }

    pub fn remove(&self, key: &str) -> Option<T> {
        if let Ok(mut entries) = self.entries.write() {
            entries.remove(key).map(|entry| entry.data)
        } else {
            None
        }
    }

    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.clear();
        }
    }

    pub fn cleanup_expired(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.retain(|_, entry| !entry.is_expired());
        }
    }

    pub fn size(&self) -> usize {
        self.entries.read().map(|entries| entries.len()).unwrap_or(0)
    }
}

pub struct RepositoryCache {
    package_cache: Cache<crate::processed::ProcessedMetaData>,
    version_cache: Cache<Vec<utils::Version>>,
    dependency_cache: Cache<Vec<crate::processed::ProcessedMetaData>>,
}

impl RepositoryCache {
    pub fn new() -> Self {
        Self {
            package_cache: Cache::new(Some(Duration::from_secs(3600))), // 1 hour
            version_cache: Cache::new(Some(Duration::from_secs(1800))), // 30 minutes
            dependency_cache: Cache::new(Some(Duration::from_secs(1800))), // 30 minutes
        }
    }

    pub fn get_package(&self, key: &str) -> Option<crate::processed::ProcessedMetaData> {
        self.package_cache.get(key)
    }

    pub fn set_package(&self, key: String, package: crate::processed::ProcessedMetaData) {
        self.package_cache.set(key, package);
    }

    pub fn get_versions(&self, key: &str) -> Option<Vec<utils::Version>> {
        self.version_cache.get(key)
    }

    pub fn set_versions(&self, key: String, versions: Vec<utils::Version>) {
        self.version_cache.set(key, versions);
    }

    pub fn get_dependencies(&self, key: &str) -> Option<Vec<crate::processed::ProcessedMetaData>> {
        self.dependency_cache.get(key)
    }

    pub fn set_dependencies(&self, key: String, dependencies: Vec<crate::processed::ProcessedMetaData>) {
        self.dependency_cache.set(key, dependencies);
    }

    pub fn cleanup(&self) {
        self.package_cache.cleanup_expired();
        self.version_cache.cleanup_expired();
        self.dependency_cache.cleanup_expired();
    }
}

pub struct DownloadCache {
    file_cache: Cache<Vec<u8>>,
    metadata_cache: Cache<String>,
}

impl DownloadCache {
    pub fn new() -> Self {
        Self {
            file_cache: Cache::new(Some(Duration::from_secs(86400))), // 24 hours
            metadata_cache: Cache::new(Some(Duration::from_secs(3600))), // 1 hour
        }
    }

    pub fn get_file(&self, key: &str) -> Option<Vec<u8>> {
        self.file_cache.get(key)
    }

    pub fn set_file(&self, key: String, data: Vec<u8>) {
        self.file_cache.set(key, data);
    }

    pub fn get_metadata(&self, key: &str) -> Option<String> {
        self.metadata_cache.get(key)
    }

    pub fn set_metadata(&self, key: String, metadata: String) {
        self.metadata_cache.set(key, metadata);
    }

    pub fn cleanup(&self) {
        self.file_cache.cleanup_expired();
        self.metadata_cache.cleanup_expired();
    }
}

#[derive(Debug, Clone)]
pub struct PerformanceMetrics {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub download_time: Duration,
    pub parse_time: Duration,
    pub install_time: Duration,
}

impl PerformanceMetrics {
    pub fn new() -> Self {
        Self {
            cache_hits: 0,
            cache_misses: 0,
            download_time: Duration::ZERO,
            parse_time: Duration::ZERO,
            install_time: Duration::ZERO,
        }
    }

    pub fn hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            0.0
        } else {
            self.cache_hits as f64 / total as f64
        }
    }
}

pub struct PerformanceTracker {
    metrics: Arc<RwLock<PerformanceMetrics>>,
}

impl PerformanceTracker {
    pub fn new() -> Self {
        Self {
            metrics: Arc::new(RwLock::new(PerformanceMetrics::new())),
        }
    }

    pub fn record_cache_hit(&self) {
        if let Ok(mut metrics) = self.metrics.write() {
            metrics.cache_hits += 1;
        }
    }

    pub fn record_cache_miss(&self) {
        if let Ok(mut metrics) = self.metrics.write() {
            metrics.cache_misses += 1;
        }
    }

    pub fn record_download_time(&self, duration: Duration) {
        if let Ok(mut metrics) = self.metrics.write() {
            metrics.download_time += duration;
        }
    }

    pub fn record_parse_time(&self, duration: Duration) {
        if let Ok(mut metrics) = self.metrics.write() {
            metrics.parse_time += duration;
        }
    }

    pub fn record_install_time(&self, duration: Duration) {
        if let Ok(mut metrics) = self.metrics.write() {
            metrics.install_time += duration;
        }
    }

    pub fn get_metrics(&self) -> PerformanceMetrics {
        self.metrics.read().unwrap().clone()
    }

    pub fn reset(&self) {
        if let Ok(mut metrics) = self.metrics.write() {
            *metrics = PerformanceMetrics::new();
        }
    }
}

pub struct ParallelDownloader {
    max_concurrent: usize,
    download_cache: DownloadCache,
    performance_tracker: PerformanceTracker,
}

impl ParallelDownloader {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            max_concurrent,
            download_cache: DownloadCache::new(),
            performance_tracker: PerformanceTracker::new(),
        }
    }

    pub async fn download_multiple(&self, urls: Vec<String>) -> Result<Vec<Vec<u8>>, String> {
        use futures::stream::StreamExt;
        use futures::stream::FuturesUnordered;

        let start_time = std::time::Instant::now();
        let mut results = Vec::new();
        let mut futures = FuturesUnordered::new();

        for url in urls {
            let future = self.download_single(url);
            futures.push(future);
        }

        while let Some(result) = futures.next().await {
            match result {
                Ok(data) => results.push(data),
                Err(e) => return Err(format!("Download failed: {}", e)),
            }
        }

        let duration = start_time.elapsed();
        self.performance_tracker.record_download_time(duration);

        Ok(results)
    }

    async fn download_single(&self, url: String) -> Result<Vec<u8>, String> {
        // Check cache first
        if let Some(cached_data) = self.download_cache.get_file(&url) {
            self.performance_tracker.record_cache_hit();
            return Ok(cached_data);
        }

        self.performance_tracker.record_cache_miss();

        // Download the file
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .connect_timeout(std::time::Duration::from_secs(2))
            .read_timeout(std::time::Duration::from_secs(3))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        let response = client.get(&url).send().await
            .map_err(|e| format!("Failed to download {}: {}", url, e))?;
        let data = response.bytes().await
            .map_err(|e| format!("Failed to read response bytes: {}", e))?
            .to_vec();

        // Cache the result
        self.download_cache.set_file(url.clone(), data.clone());

        Ok(data)
    }
}
