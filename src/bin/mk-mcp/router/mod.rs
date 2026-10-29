pub mod tasks;

use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use mikrotik_api::{Authenticated, Error as ApiError, MikrotikAPI};
use tokio::sync::Mutex;

pub use self::tasks::{TaskRegistry, TaskSnapshot, TaskStatus};
use crate::safety::{AuthorizedCommand, SafetyClass};

/// Generic row type returned by RouterOS sentences.
pub type Row = HashMap<String, String>;

/// Boxed future for dyn-compatible asynchronous connector calls.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Factory trait responsible for establishing authenticated RouterOS sessions.
pub trait RouterConnector: Send + Sync {
    /// Opens and authenticates a new RouterOS API connection.
    fn connect(&self) -> BoxFuture<'_, Result<MikrotikAPI<Authenticated>, ApiError>>;
}

/// Live connector managing TCP/TLS socket connections to real hardware or CHR routers.
pub struct LiveConnector {
    address: String,
    login: String,
    password: String,
    tls: bool,
    tls_insecure: bool,
    tls_ca: Option<std::path::PathBuf>,
    tls_domain: Option<String>,
}

impl LiveConnector {
    /// Creates a new `LiveConnector`.
    pub fn new(
        address: String,
        login: String,
        password: String,
        tls: bool,
        tls_insecure: bool,
        tls_ca: Option<std::path::PathBuf>,
        tls_domain: Option<String>,
    ) -> Self {
        Self {
            address,
            login,
            password,
            tls,
            tls_insecure,
            tls_ca,
            tls_domain,
        }
    }
}

impl RouterConnector for LiveConnector {
    fn connect(&self) -> BoxFuture<'_, Result<MikrotikAPI<Authenticated>, ApiError>> {
        Box::pin(async move {
            #[cfg(feature = "tls")]
            if self.tls {
                let domain = self.tls_domain.clone().unwrap_or_else(|| {
                    let host_part = self.address.split(':').next().unwrap_or(&self.address);
                    host_part
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .to_string()
                });

                let mut builder = mikrotik_api::TlsConfig::builder()
                    .danger_accept_invalid_certs(self.tls_insecure);
                if let Some(ca) = &self.tls_ca {
                    builder = builder
                        .add_ca_cert_file(ca)
                        .map_err(|e| std::io::Error::other(format!("failed to load CA: {e}")))?;
                }

                let config = builder.build();
                let disconnected =
                    mikrotik_api::connect_tls_with_config(&self.address, &domain, config).await?;
                return disconnected.authenticate(&self.login, &self.password).await;
            }

            let disconnected = mikrotik_api::connect(&self.address).await?;
            disconnected.authenticate(&self.login, &self.password).await
        })
    }
}

/// RouterOS execution driver strictly enforcing safety capabilities.
#[derive(Clone)]
pub struct RouterDriver {
    conn: Arc<Mutex<Option<MikrotikAPI<Authenticated>>>>,
    connector: Arc<dyn RouterConnector>,
    tasks: TaskRegistry,
}

impl RouterDriver {
    /// Creates a new `RouterDriver` backed by the specified connector.
    pub fn new(connector: Arc<dyn RouterConnector>) -> Self {
        Self {
            conn: Arc::new(Mutex::new(None)),
            connector,
            tasks: TaskRegistry::new(),
        }
    }

    /// Accesses the streaming task registry.
    pub fn tasks(&self) -> &TaskRegistry {
        &self.tasks
    }

    /// Executes an `AuthorizedCommand` returning a list of data rows.
    /// Notice: Cannot be invoked with an unverified command!
    pub async fn execute(&self, cmd: AuthorizedCommand) -> Result<Vec<Row>, ApiError> {
        let wire_owned = cmd.to_wire_attributes();
        let wire_refs: Vec<(&str, &str)> = wire_owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let attrs = (!wire_refs.is_empty()).then_some(wire_refs.as_slice());

        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.connector.connect().await?);
        }

        let api = guard.as_mut().expect("connected");
        match api.generic_array_call::<Row>(cmd.path(), attrs).await {
            Ok(rows) => Ok(rows),
            Err(ApiError::EndOfStream | ApiError::Io(_)) => {
                // Connection dropped; clear stale session
                *guard = None;
                // Auto-reconnect once if the command is idempotent (SafeRead)
                if cmd.class() == SafetyClass::SafeRead {
                    let fresh_api = self.connector.connect().await?;
                    let res = fresh_api.generic_array_call::<Row>(cmd.path(), attrs).await;
                    *guard = Some(fresh_api);
                    res
                } else {
                    Err(ApiError::EndOfStream)
                }
            }
            Err(other) => Err(other),
        }
    }

    /// Starts a background streaming command (`StreamingCall`) and registers an MCP Task.
    pub async fn start_stream(&self, cmd: AuthorizedCommand) -> Result<String, ApiError> {
        let wire_owned = cmd.to_wire_attributes();
        let wire_refs: Vec<(&str, &str)> = wire_owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let attrs = (!wire_refs.is_empty()).then_some(wire_refs.as_slice());

        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.connector.connect().await?);
        }

        let api = guard.as_mut().expect("connected");
        let mut tag: u16 = 0;
        let stream = api
            .generic_streaming_call::<Row>(cmd.path(), attrs, &mut tag)
            .await;

        if tag == 0 {
            return Err(ApiError::EndOfStream);
        }

        let task_id = self
            .tasks
            .spawn_stream_task(cmd.path().to_string(), tag, stream);
        Ok(task_id)
    }

    /// Cancels an active streaming task by sending `/cancel =tag=<u16>` to the router.
    pub async fn cancel_stream(&self, task_id: &str) -> Result<bool, ApiError> {
        if let Some(tag) = self.tasks.mark_cancelling(task_id) {
            let mut guard = self.conn.lock().await;
            if let Some(api) = guard.as_mut() {
                let _ = api.cancel(tag).await;
            }
            self.tasks.finish_cancel(task_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
