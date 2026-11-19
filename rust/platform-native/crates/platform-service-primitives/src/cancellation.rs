//! Shared cooperative signal; no admission/quota policy.
use tokio::sync::watch;
/// Cooperative owner/caller cancellation; clones address the same signal.
#[derive(Clone, Debug)]
pub struct AdmissionCancellation(watch::Sender<bool>);
impl Default for AdmissionCancellation {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}
impl AdmissionCancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}
