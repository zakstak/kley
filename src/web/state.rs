use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::runtime::RuntimeManager;
use crate::store::{SharedStore, Store};

#[derive(Clone)]
pub struct WebAppState {
    pub store: SharedStore,
    pub runtime_manager: Arc<RuntimeManager>,
}

impl WebAppState {
    pub fn new(store: SharedStore) -> Self {
        Self {
            store,
            runtime_manager: Arc::new(RuntimeManager::new()),
        }
    }

    pub fn from_store(store: Store) -> Self {
        Self::new(Arc::new(Mutex::new(store)))
    }

    pub fn for_web_mode() -> Result<Self> {
        Ok(Self::from_store(Store::open()?))
    }
}
