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

        let api = {
            let mut guard = self.conn.lock().await;
            if guard.is_none() {
                *guard = Some(self.connector.connect().await?);
            }
            guard.as_ref().expect("connected").clone()
        };

        match api.generic_array_call::<Row>(cmd.path(), attrs).await {
            Ok(rows) => Ok(rows),
            Err(ApiError::EndOfStream | ApiError::Io(_)) => {
                let fresh_api = {
                    let mut guard = self.conn.lock().await;
                    *guard = None;
                    if cmd.class() != SafetyClass::SafeRead {
                        return Err(ApiError::EndOfStream);
                    }
                    let fresh_api = self.connector.connect().await?;
                    *guard = Some(fresh_api.clone());
                    fresh_api
                };
                fresh_api.generic_array_call::<Row>(cmd.path(), attrs).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::{PolicyMode, RuleClassifier, SafetyGate, UncheckedCommand};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct SeqConnector {
        streams: std::sync::Mutex<std::collections::VecDeque<tokio::io::DuplexStream>>,
        connects: std::sync::atomic::AtomicUsize,
    }

    impl SeqConnector {
        fn new(streams: impl IntoIterator<Item = tokio::io::DuplexStream>) -> Self {
            Self {
                streams: std::sync::Mutex::new(streams.into_iter().collect()),
                connects: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl RouterConnector for SeqConnector {
        fn connect(&self) -> BoxFuture<'_, Result<MikrotikAPI<Authenticated>, ApiError>> {
            self.connects
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let stream = self.streams.lock().unwrap().pop_front();
            Box::pin(async move {
                let s = stream.ok_or(ApiError::EndOfStream)?;
                MikrotikAPI::from_stream(s).authenticate("admin", "").await
            })
        }
    }

    fn encode_word(word: &str) -> Vec<u8> {
        let len = word.len();
        let mut buf = Vec::new();
        if len < 0x80 {
            buf.push(len as u8);
        } else {
            buf.extend_from_slice(&((len as u16) | 0x8000).to_be_bytes());
        }
        buf.extend_from_slice(word.as_bytes());
        buf
    }

    fn encode_sentence(words: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        for w in words {
            buf.extend_from_slice(&encode_word(w));
        }
        buf.push(0);
        buf
    }

    async fn read_word<R: AsyncReadExt + Unpin>(r: &mut R) -> Option<String> {
        let b0 = r.read_u8().await.ok()?;
        let len = if b0 & 0x80 == 0 {
            b0 as usize
        } else {
            let b1 = r.read_u8().await.ok()?;
            (((b0 & 0x3F) as usize) << 8) | (b1 as usize)
        };
        if len == 0 {
            return Some(String::new());
        }
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).await.ok()?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    }

    async fn read_sentence<R: AsyncReadExt + Unpin>(r: &mut R) -> Option<Vec<String>> {
        let mut words = Vec::new();
        loop {
            let w = read_word(r).await?;
            if w.is_empty() {
                return Some(words);
            }
            words.push(w);
        }
    }

    fn tag_of(words: &[String]) -> String {
        words
            .iter()
            .find(|w| w.starts_with(".tag="))
            .cloned()
            .unwrap_or_else(|| ".tag=1".to_string())
    }

    #[tokio::test]
    async fn test_router_driver_reconnect_safe_read_vs_safe_write() {
        let gate = SafetyGate::new(PolicyMode::ReadWrite, RuleClassifier);

        // Stream 1: responds to /login, then drops EOF on the actual command!
        let (c1, mut s1) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            if let Some(words) = read_sentence(&mut s1).await {
                let tag = tag_of(&words);
                let _ = s1.write_all(&encode_sentence(&["!done", &tag])).await;
            }
            let _ = read_sentence(&mut s1).await;
            drop(s1);
        });

        // Stream 2: responds to /login and succeeds on retried SafeRead
        let (c2, mut s2) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            while let Some(words) = read_sentence(&mut s2).await {
                let tag = tag_of(&words);
                if words.first().map(String::as_str) == Some("/login") {
                    let _ = s2.write_all(&encode_sentence(&["!done", &tag])).await;
                } else {
                    let _ = s2
                        .write_all(&encode_sentence(&["!re", &tag, "=name=Recovered"]))
                        .await;
                    let _ = s2.write_all(&encode_sentence(&["!done", &tag])).await;
                }
            }
        });

        let connector = Arc::new(SeqConnector::new([c1, c2]));
        let driver = RouterDriver::new(connector.clone());

        let safe_read = gate
            .authorize(UncheckedCommand::new("/system/identity/print"))
            .unwrap();
        let rows = driver
            .execute(safe_read)
            .await
            .expect("SafeRead should reconnect and succeed");
        assert_eq!(rows[0].get("name").map(String::as_str), Some("Recovered"));
        assert_eq!(
            connector.connects.load(std::sync::atomic::Ordering::SeqCst),
            2
        );

        // Stream 3: responds to /login, then drops EOF on SafeWrite (should NOT retry on Stream 4)
        let (c3b, mut s3b) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            if let Some(words) = read_sentence(&mut s3b).await {
                let tag = tag_of(&words);
                let _ = s3b.write_all(&encode_sentence(&["!done", &tag])).await;
            }
            let _ = read_sentence(&mut s3b).await;
            drop(s3b);
        });
        let (c4b, _s4b) = tokio::io::duplex(4096);
        let write_connector = Arc::new(SeqConnector::new([c3b, c4b]));
        let write_driver = RouterDriver::new(write_connector.clone());

        let safe_write = gate
            .authorize(UncheckedCommand::new("/system/identity/set name=X"))
            .unwrap();
        let err = write_driver
            .execute(safe_write)
            .await
            .expect_err("SafeWrite must NOT auto-retry on EndOfStream");
        assert!(matches!(err, ApiError::EndOfStream));
        assert_eq!(
            write_connector
                .connects
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn test_router_driver_stream_disconnected_and_cancel_unknown() {
        let gate = SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier);
        let connector = Arc::new(SeqConnector::new(std::iter::empty()));
        let driver = RouterDriver::new(connector);

        let stream_cmd = gate
            .authorize(UncheckedCommand::new("/interface/listen"))
            .unwrap();
        assert!(driver.start_stream(stream_cmd).await.is_err());

        let cancelled = driver
            .cancel_stream("unknown-task-id")
            .await
            .expect("cancel unknown should return Ok(false)");
        assert!(!cancelled);
    }

    #[tokio::test]
    async fn test_live_connector_tcp_login_success_and_trap() {
        // 1. Success over TCP
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                if let Some(words) = read_sentence(&mut sock).await {
                    let tag = tag_of(&words);
                    let _ = sock.write_all(&encode_sentence(&["!done", &tag])).await;
                }
            }
        });

        let connector = LiveConnector::new(
            addr.to_string(),
            "admin".into(),
            "pass".into(),
            false,
            false,
            None,
            None,
        );
        let api = connector
            .connect()
            .await
            .expect("LiveConnector plaintext login should succeed");
        drop(api);

        // 2. Trap failure over TCP
        let listener_fail = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr_fail = listener_fail.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener_fail.accept().await {
                if let Some(words) = read_sentence(&mut sock).await {
                    let tag = tag_of(&words);
                    let _ = sock
                        .write_all(&encode_sentence(&[
                            "!trap",
                            &tag,
                            "=message=invalid user name or password",
                        ]))
                        .await;
                    let _ = sock.write_all(&encode_sentence(&["!done", &tag])).await;
                }
            }
        });

        let connector_fail = LiveConnector::new(
            addr_fail.to_string(),
            "admin".into(),
            "wrong".into(),
            false,
            false,
            None,
            None,
        );
        assert!(connector_fail.connect().await.is_err());
    }

    #[cfg(feature = "tls")]
    #[tokio::test]
    async fn test_live_connector_tls_missing_ca_errors() {
        let connector = LiveConnector::new(
            "127.0.0.1:8729".into(),
            "admin".into(),
            "pass".into(),
            true,
            false,
            Some(std::path::PathBuf::from("/nonexistent/ca.pem")),
            None,
        );
        assert!(connector.connect().await.is_err());
    }
}
