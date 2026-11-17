//! `NetArt`: the `ArtSource` of the network adapter. Refuses by host first (no I/O), then the disk
//! cache, then one capped GET, then a decode under limits.

use std::path::PathBuf;
use std::sync::Arc;

use whipspot_core::art::{ArtError, ArtImage, ArtSource};
use whipspot_core::provider::{BoxFuture, ImageRef};

use crate::allow::HostList;
use crate::cache::DiskCache;
use crate::decode::decode;
use crate::transport::{FetchError, ReqwestTransport, Transport};

/// The most bytes an art body may have.
pub(crate) const ART_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Fetches cover art from the declared hosts only. Construct only in the composition root.
pub struct NetArt {
    hosts: HostList,
    transport: Option<Arc<dyn Transport>>,
    cache: Option<Arc<DiskCache>>,
}

impl NetArt {
    /// `cache_dir` is the cache root; art is kept in `<cache_dir>/art`. When the HTTP client
    /// cannot be built every fetch answers `Unavailable`.
    pub fn new(cache_dir: PathBuf, declared_hosts: &[&str]) -> NetArt {
        let transport = ReqwestTransport::new().map(|t| Arc::new(t) as Arc<dyn Transport>);
        NetArt::build(transport, Some(cache_dir), declared_hosts)
    }

    /// Test constructor: any transport, optionally a cache root (art in `<root>/art`).
    #[doc(hidden)]
    pub fn with_transport(
        transport: Arc<dyn Transport>,
        cache_dir: Option<PathBuf>,
        declared_hosts: &[&str],
    ) -> NetArt {
        NetArt::build(Some(transport), cache_dir, declared_hosts)
    }

    fn build(
        transport: Option<Arc<dyn Transport>>,
        cache_dir: Option<PathBuf>,
        declared_hosts: &[&str],
    ) -> NetArt {
        NetArt {
            hosts: HostList::new(declared_hosts),
            transport,
            cache: cache_dir.map(|d| Arc::new(DiskCache::new(d.join("art")))),
        }
    }

    async fn cached(&self, url: &reqwest::Url) -> Option<ArtImage> {
        let cache = self.cache.clone()?;
        let u = url.clone();
        let hit = tokio::task::spawn_blocking(move || cache.read(&u, ART_MAX_BYTES))
            .await
            .ok()??;
        match decode(Arc::new(hit)).await {
            Ok(image) => Some(image),
            Err(_) => {
                // No longer decodes under today's caps: delete it and go to the network.
                if let Some(cache) = self.cache.clone() {
                    let u = url.clone();
                    let _ = tokio::task::spawn_blocking(move || cache.remove(&u)).await;
                }
                None
            }
        }
    }

    async fn get(&self, image: &ImageRef) -> Result<ArtImage, ArtError> {
        let url = self
            .hosts
            .check_str(&image.url)
            .map_err(|_| ArtError::Refused)?;
        if let Some(image) = self.cached(&url).await {
            return Ok(image);
        }
        let transport = self.transport.as_ref().ok_or(ArtError::Unavailable)?;
        let fetched = transport
            .get(&url, ART_MAX_BYTES)
            .await
            .map_err(|e| match e {
                FetchError::TooLarge => ArtError::TooLarge,
                FetchError::Network => ArtError::Unavailable,
            })?;
        if fetched.status != 200 {
            return Err(ArtError::Unavailable);
        }
        let body = Arc::new(fetched.body);
        let image = decode(body.clone()).await?;
        if let Some(cache) = self.cache.clone() {
            let _ = tokio::task::spawn_blocking(move || cache.write(&url, &body)).await;
        }
        Ok(image)
    }
}

impl ArtSource for NetArt {
    fn fetch<'a>(&'a self, image: &'a ImageRef) -> BoxFuture<'a, Result<ArtImage, ArtError>> {
        Box::pin(self.get(image))
    }
}
