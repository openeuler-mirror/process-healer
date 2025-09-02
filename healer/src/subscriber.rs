use crate::event_bus;
use async_trait::async_trait;
pub mod process_healer;
#[async_trait]
pub trait Subscriber: Send + Sync {
    async fn handle_event(&mut self, event: event_bus::ProcessEvent);
}
