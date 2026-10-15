//! Live Python service-object identity. DTOs stay owned wire values.
//! Lock scopes contain synchronous source segments only; never await in callbacks.
//! Callbacks must not recursively read/edit their captured handles.
use crate::protocol::{ServiceConfig, ServiceDefinition};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ConnectorHandle(Arc<Mutex<Value>>);
impl ConnectorHandle {
    pub fn new(value: Value) -> Self {
        Self(Arc::new(Mutex::new(value)))
    }
    pub fn snapshot(&self) -> Value {
        self.0.lock().expect("connector poisoned").clone()
    }
    pub fn edit<R>(&self, edit: impl FnOnce(&mut Value) -> R) -> R {
        edit(&mut self.0.lock().expect("connector poisoned"))
    }
    /// Execute one synchronous source segment over target and previous dictionaries.
    /// Locks are address-ordered when distinct; an alias takes just one lock.
    /// The callback must not access any handle, await, or retain references.
    pub fn edit_with_previous<R>(
        &self,
        previous: Option<&Self>,
        edit: impl FnOnce(&mut Value, Option<&Value>) -> R,
    ) -> R {
        let Some(previous) = previous else {
            let mut target = self.0.lock().expect("connector poisoned");
            return edit(&mut target, None);
        };
        if self.same_object(previous) {
            let mut target = self.0.lock().expect("connector poisoned");
            // A value copy permits safe simultaneous read/write access while the
            // original object's lock remains held through the entire callback.
            let before = target.clone();
            return edit(&mut target, Some(&before));
        }
        if Arc::as_ptr(&self.0) < Arc::as_ptr(&previous.0) {
            let mut target = self.0.lock().expect("connector poisoned");
            let previous = previous.0.lock().expect("connector poisoned");
            edit(&mut target, Some(&previous))
        } else {
            let previous = previous.0.lock().expect("connector poisoned");
            let mut target = self.0.lock().expect("connector poisoned");
            edit(&mut target, Some(&previous))
        }
    }
    pub fn same_object(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// `_service_to_dict`: an empty dict falls through `or {}`.
    pub fn dictionary_or_empty(&self) -> Self {
        if self
            .0
            .lock()
            .expect("connector poisoned")
            .as_object()
            .is_some_and(|v| !v.is_empty())
        {
            self.clone()
        } else {
            Self::new(json!({}))
        }
    }
}
#[derive(Clone)]
pub struct ConfigHandle(Arc<Mutex<ServiceConfig>>);
impl ConfigHandle {
    pub fn new(value: ServiceConfig) -> Self {
        Self(Arc::new(Mutex::new(value)))
    }
    pub fn snapshot(&self) -> ServiceConfig {
        self.0.lock().expect("service config poisoned").clone()
    }
    pub fn edit<R>(&self, edit: impl FnOnce(&mut ServiceConfig) -> R) -> R {
        edit(&mut self.0.lock().expect("service config poisoned"))
    }
    pub fn same_object(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
struct LiveService {
    // Alias-bearing DTO slots are always vacant here. There is no second cache.
    fields: ServiceDefinition,
    connector: ConnectorHandle,
    config: Option<ConfigHandle>,
}
#[derive(Clone)]
pub struct ServiceHandle(Arc<Mutex<LiveService>>);
impl std::fmt::Debug for ServiceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceHandle")
            .field("service_id", &self.service_id())
            .finish_non_exhaustive()
    }
}
impl ServiceHandle {
    pub fn new(mut fields: ServiceDefinition) -> Self {
        let connector = ConnectorHandle::new(std::mem::take(&mut fields.connector_config));
        Self::with_connector(fields, connector)
    }
    /// New outer object and detached typed config, possibly inherited connector.
    pub fn with_connector(mut fields: ServiceDefinition, connector: ConnectorHandle) -> Self {
        fields.connector_config = Value::Null;
        let config = fields.service_config.take().map(ConfigHandle::new);
        Self(Arc::new(Mutex::new(LiveService {
            fields,
            connector,
            config,
        })))
    }
    pub fn same_object(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn service_id(&self) -> String {
        self.0
            .lock()
            .expect("service poisoned")
            .fields
            .service_id
            .clone()
    }
    pub fn connector(&self) -> ConnectorHandle {
        self.0.lock().expect("service poisoned").connector.clone()
    }
    pub fn config(&self) -> Option<ConfigHandle> {
        self.0.lock().expect("service poisoned").config.clone()
    }
    pub fn materialize_config(&self) -> ConfigHandle {
        self.0
            .lock()
            .expect("service poisoned")
            .config
            .get_or_insert_with(|| ConfigHandle::new(ServiceConfig::default()))
            .clone()
    }
    pub fn snapshot(&self) -> ServiceDefinition {
        let live = self.0.lock().expect("service poisoned");
        let mut fields = live.fields.clone();
        fields.connector_config = live.connector.snapshot();
        fields.service_config = live.config.as_ref().map(ConfigHandle::snapshot);
        fields
    }
    /// Config route mutation and its legacy rate-limit assignment are one
    /// synchronous segment. Already-applied changes survive errors/drop.
    pub fn edit_config<R>(
        &self,
        edit: impl FnOnce(&mut ServiceConfig, &mut Option<Value>) -> R,
    ) -> R {
        let mut live = self.0.lock().expect("service poisoned");
        let config = live
            .config
            .get_or_insert_with(|| ConfigHandle::new(ServiceConfig::default()))
            .clone();
        config.edit(|config| edit(config, &mut live.fields.rate_limit))
    }
}
