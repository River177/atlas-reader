use std::{
    collections::VecDeque,
    sync::{Mutex, PoisonError},
};

use async_trait::async_trait;
use atlas_domain::{ConnectionTestResult, ProviderKind};

use crate::{NormalizedEndpoint, Secret};

/// Everything an adapter needs to check one provider. The provider protocol
/// (paths, headers, and response shapes) belongs to the adapter, not here.
#[derive(Clone, Debug)]
pub struct ProbeRequest {
    pub kind: ProviderKind,
    pub endpoint: NormalizedEndpoint,
    pub api_key: Option<Secret>,
}

#[async_trait]
pub trait ConnectionProbe: Send + Sync {
    async fn probe(&self, request: ProbeRequest) -> ConnectionTestResult;
}

/// Test adapter. Replays queued results, then repeats the last one.
#[derive(Debug)]
pub struct ScriptedConnectionProbe {
    results: Mutex<VecDeque<ConnectionTestResult>>,
    last: Mutex<ConnectionTestResult>,
    requests: Mutex<Vec<ProbeRequest>>,
}

impl ScriptedConnectionProbe {
    #[must_use]
    pub fn new(results: impl IntoIterator<Item = ConnectionTestResult>) -> Self {
        let results: VecDeque<_> = results.into_iter().collect();
        let last = results
            .back()
            .cloned()
            .unwrap_or_else(|| ConnectionTestResult::passed("Endpoint reachable"));
        Self {
            results: Mutex::new(results),
            last: Mutex::new(last),
            requests: Mutex::new(Vec::new()),
        }
    }

    #[must_use]
    pub fn recorded_requests(&self) -> Vec<ProbeRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Default for ScriptedConnectionProbe {
    fn default() -> Self {
        Self::new([])
    }
}

#[async_trait]
impl ConnectionProbe for ScriptedConnectionProbe {
    async fn probe(&self, request: ProbeRequest) -> ConnectionTestResult {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        let mut results = self.results.lock().unwrap_or_else(PoisonError::into_inner);
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(result) = results.pop_front() {
            *last = result.clone();
            result
        } else {
            last.clone()
        }
    }
}
