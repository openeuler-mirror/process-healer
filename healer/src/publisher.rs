use crate::event_bus::ProcessEvent;
use tokio::sync::broadcast;

pub trait Publisher {
    fn publish(
        &self,
        event: ProcessEvent,
    ) -> Result<usize, broadcast::error::SendError<ProcessEvent>>;
}
