//! Fan-out of camera frames to consumers.
//!
//! The camera loop publishes every frame once. The web server, recorder and
//! alarm engine each subscribe independently; a slow consumer cannot stall the
//! camera, it just misses intermediate frames.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, RwLock};

use crate::camera::{CameraFrame, StreamKind};

#[derive(Clone)]
pub struct FrameHub {
    tx: broadcast::Sender<Arc<CameraFrame>>,
    latest: Arc<RwLock<HashMap<StreamKind, Arc<CameraFrame>>>>,
}

impl FrameHub {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        Self {
            tx,
            latest: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Publish one frame. Publishing never blocks.
    pub async fn publish(&self, frame: CameraFrame) {
        let frame = Arc::new(frame);
        self.latest.write().await.insert(frame.stream, frame.clone());
        let _ = self.tx.send(frame);
    }

    /// Most recent frame of a given stream, if any has arrived.
    pub async fn latest(&self, kind: StreamKind) -> Option<Arc<CameraFrame>> {
        self.latest.read().await.get(&kind).cloned()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<CameraFrame>> {
        self.tx.subscribe()
    }

    pub fn subscribers(&self) -> usize {
        self.tx.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use bytes::Bytes;

    use super::*;
    use crate::camera::FramePayload;

    #[tokio::test]
    async fn keeps_latest_and_fans_out() {
        let hub = FrameHub::new(8);
        let mut rx = hub.subscribe();
        let frame = CameraFrame {
            stream: StreamKind::Visible,
            seq: 1,
            at: Instant::now(),
            payload: FramePayload::Video(Bytes::from_static(b"nal")),
        };
        hub.publish(frame).await;

        assert_eq!(rx.recv().await.unwrap().seq, 1);
        assert_eq!(hub.latest(StreamKind::Visible).await.unwrap().seq, 1);
        assert!(hub.latest(StreamKind::Thermal).await.is_none());
    }
}
