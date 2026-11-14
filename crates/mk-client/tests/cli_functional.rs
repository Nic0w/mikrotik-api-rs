use std::{
    process::Output,
    sync::{Arc, Mutex},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
};

fn encode_word(word: &str) -> Vec<u8> {
    let len = word.len();
    let mut buf = Vec::new();
    if len < 0x80 {
        buf.push(len as u8);
    } else if len < 0x4000 {
        buf.extend_from_slice(&((len as u16) | 0x8000).to_be_bytes());
    } else {
        buf.extend_from_slice(&((len as u32) | 0xC000_0000).to_be_bytes()[1..]);
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
    } else if b0 & 0x40 == 0 {
        let b1 = r.read_u8().await.ok()?;
        (((b0 & 0x3F) as usize) << 8) | (b1 as usize)
    } else {
        let mut rest = [0u8; 2];
        r.read_exact(&mut rest).await.ok()?;
        (((b0 & 0x1F) as usize) << 16) | ((rest[0] as usize) << 8) | (rest[1] as usize)
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

async fn spawn_mock_router<F>(handler: F) -> (String, Arc<Mutex<Vec<Vec<String>>>>)
where
    F: Fn(&str, &[String], &str) -> Vec<Vec<u8>> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen);

    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            while let Some(words) = read_sentence(&mut sock).await {
                if words.is_empty() {
                    continue;
                }
                seen_clone.lock().unwrap().push(words.clone());
                let cmd = words[0].as_str();
                let tag = tag_of(&words);
                let frames = handler(cmd, &words, &tag);
                for f in frames {
                    if sock.write_all(&f).await.is_err() {
                        return;
                    }
                }
                let _ = sock.flush().await;
            }
        }
    });

    (addr, seen)
}

async fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mk-client"))
        .args(args)
        .output()
        .await
        .expect("failed to execute mk-client binary")
}

#[tokio::test]
async fn test_cli_auth_failure_exits_1() {
    let (addr, _seen) = spawn_mock_router(|cmd, _words, tag| {
        if cmd == "/login" {
            vec![
                encode_sentence(&["!trap", tag, "=message=invalid user name or password"]),
                encode_sentence(&["!done", tag]),
            ]
        } else {
            vec![encode_sentence(&["!done", tag])]
        }
    })
    .await;

    let out = run_cli(&["-A", &addr, "-L", "admin", "-P", "wrong", "identify"]).await;
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Authentication failed"));
}

#[tokio::test]
async fn test_cli_identify_and_full_and_trap() {
    // 1. Plain `identify`
    let (addr, _) = spawn_mock_router(|cmd, _words, tag| match cmd {
        "/login" => vec![encode_sentence(&["!done", tag])],
        "/system/identity/print" => vec![
            encode_sentence(&["!re", tag, "=name=MockRouter"]),
            encode_sentence(&["!done", tag]),
        ],
        _ => vec![encode_sentence(&["!done", tag])],
    })
    .await;

    let out = run_cli(&["-A", &addr, "-L", "admin", "-P", "pass", "identify"]).await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("MockRouter"));

    // 2. `identify --full`
    let (addr_full, _) = spawn_mock_router(|cmd, _words, tag| match cmd {
        "/login" => vec![encode_sentence(&["!done", tag])],
        "/system/identity/print" => vec![
            encode_sentence(&["!re", tag, "=name=MockFull"]),
            encode_sentence(&["!done", tag]),
        ],
        "/system/resource/print" => vec![
            encode_sentence(&[
                "!re",
                tag,
                "=uptime=1d2h",
                "=version=7.16",
                "=build-time=2025-01-01 00:00:00",
                "=free-memory=1048576",
                "=total-memory=2097152",
                "=cpu=ARM64",
                "=cpu-count=4",
                "=cpu-load=5",
                "=free-hdd-space=4194304",
                "=total-hdd-space=8388608",
                "=architecture-name=arm64",
                "=board-name=RB5009",
                "=platform=MikroTik",
            ]),
            encode_sentence(&["!done", tag]),
        ],
        _ => vec![encode_sentence(&["!done", tag])],
    })
    .await;

    let out_full = run_cli(&[
        "-A", &addr_full, "-L", "admin", "-P", "pass", "identify", "--full",
    ])
    .await;
    assert!(
        out_full.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out_full.stderr)
    );
    let combined_full = format!(
        "{}{}",
        String::from_utf8_lossy(&out_full.stdout),
        String::from_utf8_lossy(&out_full.stderr)
    );
    assert!(combined_full.contains("MockFull"));
    assert!(combined_full.contains("RB5009"));

    // 3. `identify` failing with !trap -> exit 1
    let (addr_fail, _) = spawn_mock_router(|cmd, _words, tag| match cmd {
        "/login" => vec![encode_sentence(&["!done", tag])],
        _ => vec![
            encode_sentence(&["!trap", tag, "=message=identity boom"]),
            encode_sentence(&["!done", tag]),
        ],
    })
    .await;
    let out_fail = run_cli(&["-A", &addr_fail, "-L", "admin", "-P", "pass", "identify"]).await;
    assert_eq!(out_fail.status.code(), Some(1));
}

#[tokio::test]
async fn test_cli_custom_flags_and_modes() {
    // 1. Missing mode flag -> exit 2
    let (addr_missing, _) =
        spawn_mock_router(|_cmd, _words, tag| vec![encode_sentence(&["!done", tag])]).await;
    let out_missing = run_cli(&[
        "-A",
        &addr_missing,
        "-L",
        "admin",
        "-P",
        "pass",
        "custom",
        "/system/identity/print",
    ])
    .await;
    assert_eq!(out_missing.status.code(), Some(2));

    // 2. Conflicting mode flags -> exit 2
    let (addr_conflict, _) =
        spawn_mock_router(|_cmd, _words, tag| vec![encode_sentence(&["!done", tag])]).await;
    let out_conflict = run_cli(&[
        "-A",
        &addr_conflict,
        "-L",
        "admin",
        "-P",
        "pass",
        "custom",
        "--one-off",
        "--array-list",
        "/system/identity/print",
    ])
    .await;
    assert_eq!(out_conflict.status.code(), Some(2));

    // 3. --one-off with -p and repeatable -a
    let (addr_one, seen_one) = spawn_mock_router(|cmd, _words, tag| match cmd {
        "/login" => vec![encode_sentence(&["!done", tag])],
        _ => vec![
            encode_sentence(&["!re", tag, "=name=RouterA"]),
            encode_sentence(&["!done", tag]),
        ],
    })
    .await;
    let out_one = run_cli(&[
        "-A",
        &addr_one,
        "-L",
        "admin",
        "-P",
        "pass",
        "custom",
        "--one-off",
        "-p",
        "name,id",
        "-a",
        "numbers=*1",
        "-a",
        "comment=hi",
        "/ip/address/set",
    ])
    .await;
    assert!(
        out_one.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out_one.stderr)
    );
    {
        let recorded = seen_one.lock().unwrap();
        let cmd_words = recorded
            .iter()
            .find(|w| w.first().map(String::as_str) == Some("/ip/address/set"))
            .expect("custom command sentence recorded");
        assert!(cmd_words.iter().any(|w| w == "=.proplist=name,id"));
        assert!(cmd_words.iter().any(|w| w == "=numbers=*1"));
        assert!(cmd_words.iter().any(|w| w == "=comment=hi"));
    }

    // 4. --array-list
    let (addr_arr, _) = spawn_mock_router(|cmd, _words, tag| match cmd {
        "/login" => vec![encode_sentence(&["!done", tag])],
        _ => vec![
            encode_sentence(&["!re", tag, "=name=ether1"]),
            encode_sentence(&["!re", tag, "=name=ether2"]),
            encode_sentence(&["!done", tag]),
        ],
    })
    .await;
    let out_arr = run_cli(&[
        "-A",
        &addr_arr,
        "-L",
        "admin",
        "-P",
        "pass",
        "custom",
        "--array-list",
        "/interface/print",
    ])
    .await;
    assert!(out_arr.status.success());

    // 5. --listen with !re then !done
    let (addr_listen, _) = spawn_mock_router(|cmd, _words, tag| match cmd {
        "/login" => vec![encode_sentence(&["!done", tag])],
        _ => vec![
            encode_sentence(&["!re", tag, "=message=link up"]),
            encode_sentence(&["!done", tag]),
        ],
    })
    .await;
    let out_listen = run_cli(&[
        "-A",
        &addr_listen,
        "-L",
        "admin",
        "-P",
        "pass",
        "custom",
        "--listen",
        "/log/listen",
    ])
    .await;
    assert!(out_listen.status.success());

    // 6. --one-off !trap -> exit 1
    let (addr_trap, _) = spawn_mock_router(|cmd, _words, tag| match cmd {
        "/login" => vec![encode_sentence(&["!done", tag])],
        _ => vec![
            encode_sentence(&["!trap", tag, "=message=custom failed"]),
            encode_sentence(&["!done", tag]),
        ],
    })
    .await;
    let out_trap = run_cli(&[
        "-A",
        &addr_trap,
        "-L",
        "admin",
        "-P",
        "pass",
        "custom",
        "--one-off",
        "/bad/cmd",
    ])
    .await;
    assert_eq!(out_trap.status.code(), Some(1));
}

#[tokio::test]
async fn test_cli_active_users_events_and_error_exit() {
    // 1. ActiveUsers: login with address, login without address, logout, then !done -> exit 0
    let (addr, _) = spawn_mock_router(|cmd, _words, tag| match cmd {
        "/login" => vec![encode_sentence(&["!done", tag])],
        "/user/active/listen" => vec![
            encode_sentence(&[
                "!re",
                tag,
                "=.id=*1",
                "=when=nov/14/2026 12:00:00",
                "=name=alice",
                "=address=10.0.0.5",
                "=via=ssh",
            ]),
            encode_sentence(&[
                "!re",
                tag,
                "=.id=*2",
                "=when=nov/14/2026 12:01:00",
                "=name=serial",
                "=via=local",
            ]),
            encode_sentence(&["!re", tag, "=.id=*1", "=.dead=true"]),
            encode_sentence(&["!done", tag]),
        ],
        _ => vec![encode_sentence(&["!done", tag])],
    })
    .await;

    let out = run_cli(&["-A", &addr, "-L", "admin", "-P", "pass", "active-users"]).await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("alice"));
    assert!(combined.contains("unknown"));
    assert!(combined.contains("*1"));

    // 2. ActiveUsers trap -> exit 1
    let (addr_trap, _) = spawn_mock_router(|cmd, _words, tag| match cmd {
        "/login" => vec![encode_sentence(&["!done", tag])],
        "/user/active/listen" => vec![
            encode_sentence(&["!trap", tag, "=message=stream denied"]),
            encode_sentence(&["!done", tag]),
        ],
        _ => vec![encode_sentence(&["!done", tag])],
    })
    .await;

    let out_trap = run_cli(&[
        "-A",
        &addr_trap,
        "-L",
        "admin",
        "-P",
        "pass",
        "active-users",
    ])
    .await;
    assert_eq!(out_trap.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out_trap.stderr).contains("stream denied"));
}

#[cfg(feature = "tls")]
#[tokio::test]
async fn test_cli_tls_missing_ca_exits_1() {
    let out = run_cli(&[
        "-A",
        "127.0.0.1:8729",
        "--ssl",
        "--tls-ca",
        "/nonexistent/missing-ca.pem",
        "-L",
        "admin",
        "-P",
        "pass",
        "identify",
    ])
    .await;
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Failed to read CA certificate"));
}
