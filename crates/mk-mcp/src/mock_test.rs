#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use mikrotik_api::{Authenticated, Error as ApiError, MikrotikAPI};
    use rmcp::handler::server::wrapper::Parameters;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    use crate::{
        mcp::{
            BatchArgs, BatchCommandItem, ExecArgs, InspectArgs, MkMcpServer, PaginatedRows,
            DEFAULT_EXEC_LIMIT,
        },
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
                    "/ip/route/print" => {
                        for i in 0..150 {
                            let id = format!("=.id=*{i}");
                            let dst = format!("=dst-address=10.0.{}.0/24", i % 256);
                            let re = encode_sentence(&["!re", &tag_word, &id, &dst]);
                            let _ = server_io.write_all(&re).await;
                        }
                        let done = encode_sentence(&["!done", &tag_word]);
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
            proplist: vec!["name".into(), "type".into()],
            ..Default::default()
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
            ..Default::default()
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
            as_task: true,
            ..Default::default()
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
            task_id: Some("task-1".into()),
            task_action: Some(crate::mcp::TaskAction::Poll),
            ..Default::default()
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
            task_id: Some("task-1".into()),
            task_action: Some(crate::mcp::TaskAction::Cancel),
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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

    #[tokio::test]
    async fn test_mcp_routeros_exec_default_truncation() {
        let (client_io, server_io) = tokio::io::duplex(65536);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let args = ExecArgs {
            command: Some("/ip/route/print".into()),
            ..Default::default()
        };

        let result = server
            .routeros_exec(Parameters(args))
            .await
            .expect("exec succeeds");
        assert_ne!(result.is_error, Some(true));

        let text = result.content[0]
            .as_text()
            .expect("text content")
            .text
            .as_str();
        let page: PaginatedRows = serde_json::from_str(text).expect("valid PaginatedRows JSON");
        assert_eq!(page.total_count, 150);
        assert_eq!(page.returned_count, DEFAULT_EXEC_LIMIT);
        assert_eq!(page.offset, 0);
        assert!(page.truncated);
        assert!(page
            .message
            .as_deref()
            .unwrap_or("")
            .contains("Output truncated"));
        assert_eq!(page.rows.len(), DEFAULT_EXEC_LIMIT);
        assert_eq!(page.rows[0].get(".id").map(String::as_str), Some("*0"));
    }

    #[tokio::test]
    async fn test_mcp_routeros_exec_explicit_pagination() {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let args = ExecArgs {
            command: Some("/interface/print".into()),
            limit: Some(1),
            offset: Some(1),
            ..Default::default()
        };

        let result = server
            .routeros_exec(Parameters(args))
            .await
            .expect("exec succeeds");
        assert_ne!(result.is_error, Some(true));

        let text = result.content[0]
            .as_text()
            .expect("text content")
            .text
            .as_str();
        let page: PaginatedRows = serde_json::from_str(text).expect("valid PaginatedRows JSON");
        assert_eq!(page.total_count, 2);
        assert_eq!(page.returned_count, 1);
        assert_eq!(page.offset, 1);
        assert!(!page.truncated);
        assert!(page.message.is_none());
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].get("name").map(String::as_str), Some("bridge"));
    }

    #[tokio::test]
    async fn test_mcp_routeros_exec_filter_wire_and_rows() {
        use std::collections::HashMap;

        use crate::mcp::apply_filter_to_command;
        use crate::safety::UncheckedCommand;

        // 1. Verify wire query translation is sorted and handles optional '?' / '=' prefixes
        let mut cmd = UncheckedCommand::new("/interface/print");
        let filter = HashMap::from([
            ("running".to_string(), "true".to_string()),
            ("?disabled".to_string(), "false".to_string()),
        ]);
        apply_filter_to_command(&mut cmd, &filter);
        let gate = SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier);
        let auth = gate.authorize(cmd).expect("authorized");
        assert_eq!(
            auth.to_wire_attributes(),
            vec![
                ("?disabled=false".to_string(), String::new()),
                ("?running=true".to_string(), String::new()),
            ]
        );

        // 2. Verify end-to-end execution filters returned rows
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let args = ExecArgs {
            command: Some("/interface/print".into()),
            filter: HashMap::from([("type".to_string(), "bridge".to_string())]),
            ..Default::default()
        };

        let result = server
            .routeros_exec(Parameters(args))
            .await
            .expect("exec succeeds");
        assert_ne!(result.is_error, Some(true));

        let text = result.content[0]
            .as_text()
            .expect("text content")
            .text
            .as_str();
        let rows: Vec<crate::router::Row> = serde_json::from_str(text).expect("valid rows JSON");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("name").map(String::as_str), Some("bridge"));
    }

    #[tokio::test]
    async fn test_mcp_routeros_exec_filter_combined_with_pagination() {
        use std::collections::HashMap;

        let (client_io, server_io) = tokio::io::duplex(65536);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let args = ExecArgs {
            command: Some("/ip/route/print".into()),
            filter: HashMap::from([("dst-address".to_string(), "10.0.5.0/24".to_string())]),
            limit: Some(1),
            offset: Some(0),
            ..Default::default()
        };

        let result = server
            .routeros_exec(Parameters(args))
            .await
            .expect("exec succeeds");
        assert_ne!(result.is_error, Some(true));

        let text = result.content[0]
            .as_text()
            .expect("text content")
            .text
            .as_str();
        let page: PaginatedRows = serde_json::from_str(text).expect("valid PaginatedRows JSON");
        assert_eq!(page.total_count, 1);
        assert_eq!(page.returned_count, 1);
        assert!(!page.truncated);
        assert_eq!(page.rows[0].get(".id").map(String::as_str), Some("*5"));
    }

    #[tokio::test]
    async fn test_mcp_routeros_batch_concurrent_reads() {
        let (client_io, server_io) = tokio::io::duplex(32768);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let args = BatchArgs {
            requests: vec![
                BatchCommandItem {
                    command: "/system/identity/print".into(),
                    ..Default::default()
                },
                BatchCommandItem {
                    id: Some("ifaces".into()),
                    command: "/interface/print".into(),
                    proplist: vec!["name".into(), "type".into()],
                    ..Default::default()
                },
                BatchCommandItem {
                    command: "/ip/address/print".into(),
                    ..Default::default()
                },
            ],
        };

        let result = server
            .routeros_batch(Parameters(args))
            .await
            .expect("batch succeeds");
        assert_ne!(result.is_error, Some(true));

        let text = result.content[0]
            .as_text()
            .expect("text content")
            .text
            .as_str();
        let val: serde_json::Value = serde_json::from_str(text).expect("valid JSON");
        let results = val.get("results").expect("results map");
        assert_eq!(
            results["/system/identity/print"][0]["name"],
            serde_json::json!("Test-Router")
        );
        assert_eq!(results["ifaces"].as_array().unwrap().len(), 2);
        assert_eq!(
            results["/ip/address/print"][0]["address"],
            serde_json::json!("192.168.88.1/24")
        );
    }

    #[tokio::test]
    async fn test_mcp_routeros_batch_mixed_policy_denial() {
        let (client_io, server_io) = tokio::io::duplex(32768);
        let _server_task = spawn_mock_router(server_io);

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadWrite, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let args = BatchArgs {
            requests: vec![
                BatchCommandItem {
                    command: "/system/identity/print".into(),
                    ..Default::default()
                },
                BatchCommandItem {
                    command: "/system/reset-configuration".into(),
                    ..Default::default()
                },
            ],
        };

        let result = server
            .routeros_batch(Parameters(args))
            .await
            .expect("batch returns structured result");
        assert_ne!(result.is_error, Some(true));

        let text = result.content[0]
            .as_text()
            .expect("text content")
            .text
            .as_str();
        let val: serde_json::Value = serde_json::from_str(text).expect("valid JSON");
        let results = val.get("results").expect("results map");
        assert_eq!(
            results["/system/identity/print"][0]["name"],
            serde_json::json!("Test-Router")
        );
        let err_msg = results["/system/reset-configuration"]["error"]
            .as_str()
            .expect("error string for denied command");
        assert!(err_msg.contains("Safety Policy Denied"));
        assert!(err_msg.contains("DangerousWrite"));
    }

    struct QueueMockConnector {
        streams: std::sync::Mutex<std::collections::VecDeque<tokio::io::DuplexStream>>,
    }

    impl QueueMockConnector {
        fn new(streams: impl IntoIterator<Item = tokio::io::DuplexStream>) -> Self {
            Self {
                streams: std::sync::Mutex::new(streams.into_iter().collect()),
            }
        }
    }

    impl crate::router::RouterConnector for QueueMockConnector {
        fn connect(
            &self,
        ) -> crate::router::BoxFuture<'_, Result<MikrotikAPI<Authenticated>, mikrotik_api::Error>>
        {
            let stream = self.streams.lock().unwrap().pop_front();
            Box::pin(async move {
                let s = stream.ok_or(mikrotik_api::Error::EndOfStream)?;
                let api = MikrotikAPI::from_stream(s);
                api.authenticate("admin", "").await
            })
        }
    }

    fn spawn_custom_router<F, Fut>(
        mut server_io: tokio::io::DuplexStream,
        handler: F,
    ) -> tokio::task::JoinHandle<()>
    where
        F: Fn(String, Vec<String>, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Vec<Vec<u8>>> + Send,
    {
        tokio::spawn(async move {
            while let Ok(words) = read_sentence(&mut server_io).await {
                if words.is_empty() {
                    continue;
                }
                let cmd = words[0].clone();
                let tag_word = format!(".tag={}", get_tag(&words));
                let frames = if cmd == "/login" {
                    vec![encode_sentence(&["!done", &tag_word])]
                } else {
                    handler(cmd, words, tag_word).await
                };
                for frame in frames {
                    if server_io.write_all(&frame).await.is_err() {
                        return;
                    }
                }
            }
        })
    }

    fn assert_tool_error(res: rmcp::model::CallToolResult, needle: &str) {
        assert_eq!(
            res.is_error,
            Some(true),
            "expected error CallToolResult, got: {res:?}"
        );
        let text = serde_json::to_string(&res.content).unwrap();
        assert!(
            text.contains(needle),
            "expected '{needle}' in tool error content: {text}"
        );
    }

    #[tokio::test]
    async fn test_mcp_get_overview_trap_propagation_and_missing_identity_name() {
        for failing_cmd in [
            "/system/identity/print",
            "/system/resource/print",
            "/interface/print",
            "/ip/address/print",
        ] {
            let (client_io, server_io) = tokio::io::duplex(16384);
            let fail = failing_cmd.to_string();
            let _task = spawn_custom_router(server_io, move |cmd, _words, tag_word| {
                let fail = fail.clone();
                async move {
                    if cmd == fail {
                        return vec![
                            encode_sentence(&["!trap", &tag_word, "=message=simulated failure"]),
                            encode_sentence(&["!done", &tag_word]),
                        ];
                    }
                    match cmd.as_str() {
                        "/system/identity/print" => vec![
                            encode_sentence(&["!re", &tag_word, "=name=R1"]),
                            encode_sentence(&["!done", &tag_word]),
                        ],
                        "/system/resource/print" => vec![
                            encode_sentence(&["!re", &tag_word, "=uptime=1m"]),
                            encode_sentence(&["!done", &tag_word]),
                        ],
                        "/interface/print" | "/ip/address/print" => {
                            vec![encode_sentence(&["!done", &tag_word])]
                        }
                        _ => vec![encode_sentence(&["!done", &tag_word])],
                    }
                }
            });

            let connector = Arc::new(MockConnector::new(client_io));
            let driver = RouterDriver::new(connector);
            let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
            let server = MkMcpServer::new(safety, driver);

            let res = server
                .get_overview()
                .await
                .expect("tool error returns CallToolResult");
            assert_tool_error(res, "simulated failure");
        }

        // Identity row present without =name should still succeed via fallback
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _task = spawn_custom_router(server_io, |cmd, _words, tag_word| async move {
            match cmd.as_str() {
                "/system/identity/print" => vec![
                    encode_sentence(&["!re", &tag_word, "=other=value"]),
                    encode_sentence(&["!done", &tag_word]),
                ],
                "/system/resource/print" => vec![
                    encode_sentence(&["!re", &tag_word, "=board-name=CHR"]),
                    encode_sentence(&["!done", &tag_word]),
                ],
                _ => vec![encode_sentence(&["!done", &tag_word])],
            }
        });

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);
        let res = server
            .get_overview()
            .await
            .expect("overview succeeds without identity name");
        let text = serde_json::to_string(&res.content).unwrap();
        assert!(text.contains("CHR"));
    }

    #[tokio::test]
    async fn test_mcp_routeros_inspect_categories_docs_and_trap() {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _task = spawn_custom_router(server_io, |cmd, words, tag_word| async move {
            if cmd != "/console/inspect" {
                return vec![encode_sentence(&["!done", &tag_word])];
            }
            let is_syntax = words.iter().any(|w| w == "=request=syntax");
            let is_fail_path = words.iter().any(|w| w == "=path=bad,path");
            if is_fail_path {
                return vec![
                    encode_sentence(&["!trap", &tag_word, "=message=no such path"]),
                    encode_sentence(&["!done", &tag_word]),
                ];
            }
            if is_syntax {
                vec![
                    encode_sentence(&[
                        "!re",
                        &tag_word,
                        "=type=syntax",
                        "=symbol=address",
                        "=text=",
                    ]),
                    encode_sentence(&[
                        "!re",
                        &tag_word,
                        "=type=syntax",
                        "=symbol=firewall",
                        "=text=   ",
                    ]),
                    encode_sentence(&[
                        "!re",
                        &tag_word,
                        "=type=syntax",
                        "=symbol=where",
                        "=text=Filter expression",
                    ]),
                    encode_sentence(&[
                        "!re",
                        &tag_word,
                        "=type=syntax",
                        "=symbol=print",
                        "=text=Display entries",
                    ]),
                    encode_sentence(&["!done", &tag_word]),
                ]
            } else {
                vec![
                    encode_sentence(&[
                        "!re",
                        &tag_word,
                        "=type=child",
                        "=node-type=dir",
                        "=name=address",
                    ]),
                    encode_sentence(&[
                        "!re",
                        &tag_word,
                        "=type=child",
                        "=node-type=path",
                        "=name=firewall",
                    ]),
                    encode_sentence(&[
                        "!re",
                        &tag_word,
                        "=type=child",
                        "=node-type=cmd",
                        "=name=print",
                    ]),
                    encode_sentence(&[
                        "!re",
                        &tag_word,
                        "=type=child",
                        "=node-type=arg",
                        "=name=where",
                    ]),
                    encode_sentence(&["!re", &tag_word, "=type=child", "=node-type=dir"]),
                    encode_sentence(&["!done", &tag_word]),
                ]
            }
        });

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        let res = server
            .routeros_inspect(Parameters(InspectArgs {
                path: Some("ip".into()),
                include_docs: true,
            }))
            .await
            .expect("inspect succeeds");
        assert_ne!(res.is_error, Some(true));
        let text = serde_json::to_string(&res.content).unwrap();
        assert!(text.contains("address"));
        assert!(text.contains("firewall"));
        assert!(text.contains("print"));
        assert!(text.contains("where"));
        assert!(text.contains("Filter expression"));
        assert!(text.contains("Display entries"));

        let err_res = server
            .routeros_inspect(Parameters(InspectArgs {
                path: Some("bad/path".into()),
                include_docs: false,
            }))
            .await
            .expect("tool error returns CallToolResult");
        assert_tool_error(err_res, "no such path");
    }

    #[tokio::test]
    async fn test_mcp_routeros_exec_error_and_task_edge_paths() {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _task = spawn_custom_router(server_io, |_cmd, _words, tag_word| async move {
            vec![
                encode_sentence(&["!trap", &tag_word, "=message=bad command"]),
                encode_sentence(&["!done", &tag_word]),
            ]
        });

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        // Missing both command and task_id
        let res = server
            .routeros_exec(Parameters(ExecArgs::default()))
            .await
            .expect("tool error returns CallToolResult");
        assert_tool_error(res, "command");

        // Poll missing task
        let res = server
            .routeros_exec(Parameters(ExecArgs {
                task_id: Some("missing-task".into()),
                task_action: Some(crate::mcp::TaskAction::Poll),
                ..Default::default()
            }))
            .await
            .expect("tool error returns CallToolResult");
        assert_tool_error(res, "missing-task");

        // Cancel missing task
        let res = server
            .routeros_exec(Parameters(ExecArgs {
                task_id: Some("missing-task".into()),
                task_action: Some(crate::mcp::TaskAction::Cancel),
                ..Default::default()
            }))
            .await
            .expect("tool error returns CallToolResult");
        assert_tool_error(res, "missing-task");

        // Router !trap on non-streaming command
        let res = server
            .routeros_exec(Parameters(ExecArgs {
                command: Some("/system/identity/print".into()),
                ..Default::default()
            }))
            .await
            .expect("tool error returns CallToolResult");
        assert_tool_error(res, "bad command");

        // Stream start failure when connector has no stream left (QueueMockConnector empty -> EndOfStream)
        let disconnected_connector = Arc::new(QueueMockConnector::new(std::iter::empty()));
        let server_disc = MkMcpServer::new(
            Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier)),
            RouterDriver::new(disconnected_connector),
        );
        let res = server_disc
            .routeros_exec(Parameters(ExecArgs {
                command: Some("/interface/listen".into()),
                ..Default::default()
            }))
            .await
            .expect("tool error returns CallToolResult");
        assert_eq!(res.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_mcp_routeros_batch_edge_paths_and_server_info() {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let _task = spawn_custom_router(server_io, |cmd, _words, tag_word| async move {
            match cmd.as_str() {
                "/system/identity/print" => vec![
                    encode_sentence(&["!re", &tag_word, "=name=BatchRouter"]),
                    encode_sentence(&["!done", &tag_word]),
                ],
                "/ip/route/print" => vec![
                    encode_sentence(&["!trap", &tag_word, "=message=route trap"]),
                    encode_sentence(&["!done", &tag_word]),
                ],
                _ => vec![encode_sentence(&["!done", &tag_word])],
            }
        });

        let connector = Arc::new(MockConnector::new(client_io));
        let driver = RouterDriver::new(connector);
        let safety = Arc::new(SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier));
        let server = MkMcpServer::new(safety, driver);

        // ServerHandler::get_info smoke check
        let info = rmcp::ServerHandler::get_info(&server);
        assert!(info.capabilities.tools.is_some());
        assert!(info.instructions.as_deref().is_some_and(|s| !s.is_empty()));

        // Empty batch requests
        let empty_err = server
            .routeros_batch(Parameters(BatchArgs { requests: vec![] }))
            .await;
        assert!(
            empty_err.is_err() || empty_err.as_ref().is_ok_and(|r| r.is_error == Some(true)),
            "empty batch requests should fail: {empty_err:?}"
        );

        // Batch containing duplicate commands without id, a streaming item error, a trap, and a valid read
        let res = server
            .routeros_batch(Parameters(BatchArgs {
                requests: vec![
                    BatchCommandItem {
                        command: "/system/identity/print".into(),
                        ..Default::default()
                    },
                    BatchCommandItem {
                        command: "/system/identity/print".into(),
                        ..Default::default()
                    },
                    BatchCommandItem {
                        id: Some("stream".into()),
                        command: "/interface/listen".into(),
                        ..Default::default()
                    },
                    BatchCommandItem {
                        id: Some("trap".into()),
                        command: "/ip/route/print".into(),
                        ..Default::default()
                    },
                ],
            }))
            .await
            .expect("batch returns mixed per-item result map");
        let text = serde_json::to_string(&res.content).unwrap();
        assert!(text.contains("BatchRouter"));
        assert!(text.contains("#2"));
        assert!(text.contains("route trap"));
    }
}
