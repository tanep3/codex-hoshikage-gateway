use super::{CodexTransport, LaunchConfig, TransportError};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Owns at most two independent App Server children. A lease holds its permit
/// for the whole Run; approvals and interrupts use its transport directly and
/// never wait on the new-Run semaphore.
#[derive(Clone)]
pub struct CodexRuntimePool {
    config: LaunchConfig,
    permits: Arc<Semaphore>,
}

pub struct RuntimeLease {
    transport: CodexTransport,
    _permit: OwnedSemaphorePermit,
}

impl CodexRuntimePool {
    pub fn new(config: LaunchConfig) -> Self {
        Self {
            config,
            permits: Arc::new(Semaphore::new(2)),
        }
    }

    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }

    pub async fn acquire(&self) -> Result<RuntimeLease, TransportError> {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| TransportError::NotSent("runtime pool closed".into()))?;
        let transport = CodexTransport::launch(&self.config).await?;
        Ok(RuntimeLease {
            transport,
            _permit: permit,
        })
    }

    pub fn close(&self) {
        self.permits.close();
    }
}

impl RuntimeLease {
    pub fn transport(&self) -> &CodexTransport {
        &self.transport
    }
    pub async fn shutdown(self) -> Result<(), TransportError> {
        self.transport.shutdown().await
    }
}
