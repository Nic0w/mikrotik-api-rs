#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use mikrotik_api::{Authenticated, Error as ApiError, MikrotikAPI};
    use rmcp::handler::server::wrapper::Parameters;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    use crate::{
        mcp::{ExecArgs, InspectArgs, MkMcpServer},
        router::{BoxFuture, RouterConnector, RouterDriver},
        safety::{PolicyMode, RuleClassifier, SafetyGate},
    };

    fn encode_word(w: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let bytes = w.as_bytes();
        if bytes.len() <= 0x7f {
            out.push(bytes.len() as u8);
        } else {
            out.push(((bytes.len() >> 8) | 0x80) as u8);
            out.push((bytes.len() & 0xff) as u8);
        }
        out.extend_from_slice(bytes);
        out
    }

    fn encode_sentence(words: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for w in words {
            out.extend(encode_word(w));
        }
        out.push(0x00);
        out
    }

    async fn read_sentence<R: AsyncReadExt + Unpin>(
        reader: &mut R,
    ) -> Result<Vec<String>, std::io::Error> {
        let mut words = Vec::new();
        loop {
            let mut b = [0u8; 1];
            reader.read_exact(&mut b).await?;
            if b[0] == 0 {
                break;
            }
            let len = if b[0] & 0x80 == 0 {
                b[0] as usize
            } else {
                let mut b2 = [0u8; 1];
                reader.read_exact(&mut b2).await?;
                (((b[0] & 0x3f) as usize) << 8) | (b2[0] as usize)
            };
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).await?;
            words.push(String::from_utf8_lossy(&buf).to_string());
        }
        Ok(words)
    }

    fn get_tag(words: &[String]) -> String {
        for w in words {
            if let Some(tag) = w.strip_prefix(".tag=") {
                return tag.to_string();
            }
        }
        "1".to_string()
    }

    struct MockConnector {
        client_stream: tokio::sync::Mutex<Option<DuplexStream>>,
    }

    impl MockConnector {
        fn new(client_stream: DuplexStream) -> Self {
            Self {
                client_stream: tokio::sync::Mutex::new(Some(client_stream)),
            }
        }
    }

    impl RouterConnector for MockConnector {
        fn connect(&self) -> BoxFuture<'_, Result<MikrotikAPI<Authenticated>, ApiError>> {
            Box::pin(async move {
                let mut guard = self.client_stream.lock().await;
                let stream = guard
                    .take()
                    .ok_or_else(|| std::io::Error::other("Mock stream already consumed"))?;
                let disconnected = MikrotikAPI::from_stream(stream);
                disconnected.authenticate("admin", "password").await
            })
        }
    }

    /// Spawns a mock router responding to standard RouterOS commands.
    fn spawn_mock_router(mut server_io: DuplexStream) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let words = match read_sentence(&mut server_io).await {
                    Ok(w) if w.is_empty() => continue,
                    Ok(w) => w,
                    Err(_) => break, // Connection closed
                };

                let cmd = &words[0];
                let tag = get_tag(&words);
                let tag_word = format!(".tag={tag}");

                match cmd.as_str() {
                    "/login" => {
                        let resp = encode_sentence(&["!done", &tag_word]);
                        let _ = server_io.write_all(&resp).await;
                    }
                    "/system/identity/print" => {
                        let re = encode_sentence(&["!re", &tag_word, "=name=Test-Router"]);
                        let done = encode_sentence(&["!done", &tag_word]);
                        let _ = server_io.write_all(&re).await;
                        let _ = server_io.write_all(&done).await;
                    }
                    "/system/resource/print" => {
                        let re = encode_sentence(&[
                            "!re",
                            &tag_word,
                            "=uptime=5d2h",
                            "=version=7.16",
                            "=cpu-load=3",
                            "=free-memory=104857600",
                            "=total-memory=268435456",
                            "=board-name=RB5009UG+S+IN",
                        ]);
                        let done = encode_sentence(&["!done", &tag_word]);
                        let _ = server_io.write_all(&re).await;
                        let _ = server_io.write_all(&done).await;
                    }
                    "/interface/print" => {
                        let re1 = encode_sentence(&[
                            "!re",
                            &tag_word,
                            "=name=ether1",
                            "=type=ether",
                            "=running=true",
                            "=disabled=false",
                            "=mtu=1500",
                            "=mac-address=00:11:22:33:44:55",
                        ]);
                        let re2 = encode_sentence(&[
                            "!re",
                            &tag_word,
                            "=name=bridge",
                            "=type=bridge",
                            "=running=true",
                            "=disabled=false",
                            "=mtu=1500",
                            "=mac-address=00:11:22:33:44:00",
                        ]);
                        let done = encode_sentence(&["!done", &tag_word]);
                        let _ = server_io.write_all(&re1).await;
                        let _ = server_io.write_all(&re2).await;
                        let _ = server_io.write_all(&done).await;
                    }
                    "/ip/address/print" => {
                        let re = encode_sentence(&[
                            "!re",
                            &tag_word,
                            "=address=192.168.88.1/24",
                            "=network=192.168.88.0",
                            "=interface=bridge",
                            "=disabled=false",
                            "=dynamic=false",
                        ]);
                        let done = encode_sentence(&["!done", &tag_word]);
                        let _ = server_io.write_all(&re).await;
                        let _ = server_io.write_all(&done).await;
                    }
                    "/console/inspect" => {
                        // Inspect request=child
                        let is_syntax = words.iter().any(|w| w == "=request=syntax");
                        if is_syntax {
                            let re = encode_sentence(&[
                                "!re",
                                &tag_word,
                                "=type=syntax",
                                "=symbol=address",
                                "=text=IP address and subnet mask",
                            ]);
                            let done = encode_sentence(&["!done", &tag_word]);
                            let _ = server_io.write_all(&re).await;
                            let _ = server_io.write_all(&done).await;
                        } else {
                            let re1 = encode_sentence(&[
                                "!re",
                                &tag_word,
                                "=type=child",
                                "=name=address",
                                "=node-type=cmd",
                            ]);
                            let re2 = encode_sentence(&[
                                "!re",
                                &tag_word,
                                "=type=child",
                                "=name=firewall",
                                "=node-type=dir",
                            ]);
                            let done = encode_sentence(&["!done", &tag_word]);
                            let _ = server_io.write_all(&re1).await;
                            let _ = server_io.write_all(&re2).await;
                            let _ = server_io.write_all(&done).await;
                        }
                    }
                    "/log/listen" => {
                        // Stream 3 events then pause
                        for i in 1..=3 {
                            let msg = format!("=message=system test log event {i}");
                            let re = encode_sentence(&["!re", &tag_word, &msg, "=topics=system"]);
                            let _ = server_io.write_all(&re).await;
                        }
                    }
                    "/cancel" => {
                        let done = encode_sentence(&["!done", &tag_word]);
                        let _ = server_io.write_all(&done).await;
                    }
                    "/ip/firewall/filter/add" => {
                        // Echo back received attributes inside !re so the test can assert them
                        let mut reply_words: Vec<&str> = vec!["!re", &tag_word, "=ret=*1337"];
                        for w in &words[1..] {
                            if !w.starts_with(".tag=") {
                                reply_words.push(w.as_str());
                            }
                        }
                        let re = encode_sentence(&reply_words);
                        let done = encode_sentence(&["!done", &tag_word]);
                        let _ = server_io.write_all(&re).await;
                        let _ = server_io.write_all(&done).await;
                    }
                    other => {
                        let trap = encode_sentence(&[
                            "!trap",
                            &tag_word,
                            &format!("=message=unknown command {other}"),
                        ]);
                        let done = encode_sentence(&["!done", &tag_word]);
                        let _ = server_io.write_all(&trap).await;
                        let _ = server_io.write_all(&done).await;
                    }
                }
            }
        })
    }

    #[tokio::test]
    async fn test_mcp_get_overview_end_to_end() {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let result = server.get_overview().await.expect("overview succeeds");
        assert_ne!(result.is_error, Some(true));
        assert!(!result.content.is_empty());

        let json_text = serde_json::to_string(&result.content).unwrap();
        assert!(json_text.contains("Test-Router"));
        assert!(json_text.contains("RB5009UG+S+IN"));
        assert!(json_text.contains("192.168.88.1/24"));
        assert!(json_text.contains("ether1"));
    }

    #[tokio::test]
    async fn test_mcp_routeros_inspect() {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let args = InspectArgs {
            path: Some("ip".into()),
            include_docs: true,
        };
        let result = server
            .routeros_inspect(Parameters(args))
            .await
            .expect("inspect succeeds");
        assert_ne!(result.is_error, Some(true));

        let json_text = serde_json::to_string(&result.content).unwrap();
        assert!(json_text.contains("firewall"));
        assert!(json_text.contains("address"));
    }

    #[tokio::test]
    async fn test_mcp_routeros_exec_safe_read() {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let args = ExecArgs {
            command: Some("/interface/print".into()),
            attributes: Default::default(),
            query: Default::default(),
            proplist: vec!["name".into(), "type".into()],
            as_task: false,
            task_id: None,
            task_action: None,
        };

        let result = server
            .routeros_exec(Parameters(args))
            .await
            .expect("exec succeeds");
        assert_ne!(result.is_error, Some(true));

        let json_text = serde_json::to_string(&result.content).unwrap();
        assert!(json_text.contains("ether1"));
        assert!(json_text.contains("bridge"));
    }

    #[tokio::test]
    async fn test_mcp_routeros_exec_dangerous_write_blocked_zero_io() {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadWrite, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let args = ExecArgs {
            command: Some("/system/reset-configuration".into()),
            attributes: Default::default(),
            query: Default::default(),
            proplist: Default::default(),
            as_task: false,
            task_id: None,
            task_action: None,
        };

        let result = server
            .routeros_exec(Parameters(args))
            .await
            .expect("exec call returns tool error");
        assert_eq!(result.is_error, Some(true));

        let json_text = serde_json::to_string(&result.content).unwrap();
        assert!(json_text.contains("Safety Policy Denied"));
        assert!(json_text.contains("DangerousWrite"));
    }

    #[tokio::test]
    async fn test_mcp_streaming_task_start_poll_cancel() {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        // 1. Start streaming task
        let start_args = ExecArgs {
            command: Some("/log/listen".into()),
            attributes: Default::default(),
            query: Default::default(),
            proplist: Default::default(),
            as_task: true,
            task_id: None,
            task_action: None,
        };

        let start_res = server
            .routeros_exec(Parameters(start_args))
            .await
            .expect("stream starts");
        assert_ne!(start_res.is_error, Some(true));
        let start_json = serde_json::to_string(&start_res.content).unwrap();
        assert!(start_json.contains("task-1"));

        // Allow background worker to buffer events
        tokio::time::sleep(Duration::from_millis(50)).await;

        // 2. Poll task
        let poll_args = ExecArgs {
            command: None,
            attributes: Default::default(),
            query: Default::default(),
            proplist: Default::default(),
            as_task: false,
            task_id: Some("task-1".into()),
            task_action: Some(crate::mcp::TaskAction::Poll),
        };

        let poll_res = server
            .routeros_exec(Parameters(poll_args))
            .await
            .expect("poll succeeds");
        assert_ne!(poll_res.is_error, Some(true));
        let poll_json = serde_json::to_string(&poll_res.content).unwrap();
        assert!(poll_json.contains("system test log event"));

        // 3. Cancel task
        let cancel_args = ExecArgs {
            command: None,
            attributes: Default::default(),
            query: Default::default(),
            proplist: Default::default(),
            as_task: false,
            task_id: Some("task-1".into()),
            task_action: Some(crate::mcp::TaskAction::Cancel),
        };

        let cancel_res = server
            .routeros_exec(Parameters(cancel_args))
            .await
            .expect("cancel succeeds");
        assert_ne!(cancel_res.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_mcp_routeros_exec_cli_spaces_and_modifiers() {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        // Test CLI-style invocation with spaces and "detail" modifier
        let args = ExecArgs {
            command: Some("/ip/address print detail".into()),
            attributes: Default::default(),
            query: Default::default(),
            proplist: Default::default(),
            as_task: false,
            task_id: None,
            task_action: None,
        };

        let result = server
            .routeros_exec(Parameters(args))
            .await
            .expect("exec succeeds");
        assert_ne!(result.is_error, Some(true));

        let json_text = serde_json::to_string(&result.content).unwrap();
        assert!(json_text.contains("192.168.88.1/24"));
    }

    #[tokio::test]
    async fn test_mcp_get_overview_propagates_connection_error() {
        struct FailingConnector;
        impl RouterConnector for FailingConnector {
            fn connect(&self) -> BoxFuture<'_, Result<MikrotikAPI<Authenticated>, ApiError>> {
                Box::pin(async {
                    Err(ApiError::Io(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "Connection refused (test)",
                    )))
                })
            }
        }

        let connector = Arc::new(FailingConnector);
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let result = server
            .get_overview()
            .await
            .expect("tool call returns result");
        assert_eq!(result.is_error, Some(true));

        let json_text = serde_json::to_string(&result.content).unwrap();
        assert!(json_text.contains("Failed to connect or query router identity"));
        assert!(json_text.contains("Connection refused"));
    }

    #[tokio::test]
    async fn test_mcp_routeros_exec_firewall_add_with_quoted_comment() {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadWrite, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let args = ExecArgs {
            command: Some(
                r#"/ip/firewall/filter/add action=accept chain=input dst-port=1337 protocol=tcp connection-state=new comment="Allow incoming IPv4 traffic on port 1337" log=yes priority=1"#
                    .into(),
            ),
            attributes: Default::default(),
            query: Default::default(),
            proplist: Default::default(),
            as_task: false,
            task_id: None,
            task_action: None,
        };

        let result = server
            .routeros_exec(Parameters(args))
            .await
            .expect("exec succeeds");
        assert_ne!(result.is_error, Some(true));

        let json_text = serde_json::to_string(&result.content).unwrap();
        assert!(json_text.contains("Allow incoming IPv4 traffic on port 1337"));
        assert!(json_text.contains("1337"));
        assert!(json_text.contains("accept"));
    }
}
