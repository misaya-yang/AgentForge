use super::ContentFuture;
use std::time::Duration;
pub struct FetchedImage {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub status: u16,
}
pub trait ImageReferenceFetch: Send + Sync {
    /// Must implement canonical safe_fetch DNS pinning, private/loopback rejection,
    /// redirect revalidation and streaming byte cap. It must not use an unrestricted client.
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        max_bytes: usize,
        timeout: Duration,
    ) -> ContentFuture<'a, FetchedImage>;
}
