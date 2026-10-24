use std::{env, time::Duration};

use mikrotik_api::{Authenticated, Error, MikrotikAPI};
use serde::Deserialize;
use tokio::time::timeout;

fn router_config() -> (String, String, String) {
    let addr = env::var("MIKROTIK_ADDR")
        .expect("MIKROTIK_ADDR environment variable must be set for functional tests");
    let user = env::var("MIKROTIK_USER")
        .expect("MIKROTIK_USER environment variable must be set for functional tests");
    let pass = env::var("MIKROTIK_PASS")
        .expect("MIKROTIK_PASS environment variable must be set for functional tests");

    (addr, user, pass)
}

async fn connect_and_login() -> MikrotikAPI<Authenticated> {
    let (addr, user, pass) = router_config();

    let disconnected = timeout(Duration::from_secs(5), mikrotik_api::connect(&addr))
        .await
        .expect("timed out connecting to router")
        .expect("failed to establish TCP connection to router");

    timeout(
        Duration::from_secs(5),
        disconnected.authenticate(&user, &pass),
    )
    .await
    .expect("timed out authenticating with router")
    .expect("failed to authenticate with valid credentials")
}

#[derive(Debug, Deserialize)]
struct Identity {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct BasicResources {
    pub uptime: String,
    pub version: String,
    pub board_name: String,
    pub architecture_name: String,
    pub cpu: String,
    pub cpu_count: u8,
}

#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_login_success() {
    let _api = connect_and_login().await;
}

#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_login_invalid_credentials() {
    let (addr, user, _) = router_config();

    let disconnected = timeout(Duration::from_secs(5), mikrotik_api::connect(&addr))
        .await
        .expect("timed out connecting to router")
        .expect("failed to establish TCP connection to router");

    let result = timeout(
        Duration::from_secs(5),
        disconnected.authenticate(&user, "definitely-wrong-password-12345"),
    )
    .await
    .expect("timed out waiting for authentication rejection");

    assert!(
        result.is_err(),
        "expected authentication with wrong password to fail"
    );
}

#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_fetch_router_identity() {
    let mut api = connect_and_login().await;

    let identity = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<Identity>("/system/identity/print", None),
    )
    .await
    .expect("timed out fetching router identity")
    .expect("failed to fetch router identity");

    assert!(
        !identity.name.trim().is_empty(),
        "router identity name should not be empty"
    );
    println!("Connected to router identity: '{}'", identity.name);
}

#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_fetch_resources_uptime_and_version() {
    let mut api = connect_and_login().await;

    let res = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<BasicResources>("/system/resource/print", None),
    )
    .await
    .expect("timed out fetching basic system resources")
    .expect("failed to fetch basic system resources");

    assert!(!res.uptime.is_empty(), "uptime should not be empty");
    assert!(!res.version.is_empty(), "version should not be empty");
    assert!(!res.board_name.is_empty(), "board_name should not be empty");
    assert!(
        !res.architecture_name.is_empty(),
        "architecture_name should not be empty"
    );
    assert!(!res.cpu.is_empty(), "cpu should not be empty");
    assert!(res.cpu_count > 0, "cpu_count should be > 0");

    println!(
        "Router resources: board={}, arch={}, version={}, uptime={}, cpus={}",
        res.board_name, res.architecture_name, res.version, res.uptime, res.cpu_count
    );
}

#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_readonly_user_cannot_write() {
    let mut api = connect_and_login().await;

    let result = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<()>(
            "/system/identity/set",
            Some(&[("name", "unauthorized-change")]),
        ),
    )
    .await
    .expect("timed out waiting for router response to unauthorized write");

    assert!(
        result.is_err(),
        "expected read-only test user to be denied write permission"
    );
}

#[derive(Debug, Deserialize)]
struct InterfaceName {
    #[allow(dead_code)]
    pub name: String,
}

/// Tests 2-byte word length encoding where total word length is 256 (0x0100).
///
/// With the endianness bug (`to_ne_bytes()` on little-endian), `(256u16 | 0x8000)` (0x8100)
/// is encoded as `[0x00, 0x81]`. The leading `0x00` byte acts as an empty-word sentence
/// terminator on the wire, causing RouterOS to execute `/interface/print` BEFORE seeing
/// the `?name=...` filter and return all interfaces instead of an empty list.
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_word_len_two_bytes_256_endianness() {
    let mut api = connect_and_login().await;

    // "?name=" is 6 bytes; 6 + 250 = 256 bytes (0x0100).
    let filter_val = "a".repeat(250);
    assert_eq!(format!("?name={}", filter_val).len(), 256);

    let interfaces = timeout(
        Duration::from_secs(5),
        api.generic_array_call::<InterfaceName>(
            "/interface/print",
            Some(&[("?name", filter_val.as_str())]),
        ),
    )
    .await
    .expect("timed out waiting for 256-byte word query response")
    .expect("failed to execute 256-byte word query");

    assert!(
        interfaces.is_empty(),
        "expected 0 interfaces matching 250-char name, got {} (sentence was truncated by 0x00 byte!)",
        interfaces.len()
    );
}

/// Tests 2-byte word length encoding where total word length is 258 (0x0102).
///
/// With the endianness bug (`to_ne_bytes()` on little-endian), `(258u16 | 0x8000)` (0x8102)
/// is encoded as `[0x02, 0x81]`. RouterOS reads:
/// - `0x02` -> 2-byte word `"\x81?"`
/// - `word[1]` = `'n'` (110) -> 110-byte word `word[2..112]`
/// - `word[112]` = `'H'` (72) -> 72-byte word `word[113..185]`, which starts with `"=bad=..."`!
/// - `word[185]` = `'H'` (72) -> 72-byte word `word[186..258]` (ending right at 258!)
/// - `0x00` -> end of sentence!
/// Because `word[113..185]` starts with `"=bad="`, RouterOS interprets it as an attribute
/// parameter on `/interface/print` and immediately returns `!trap =message=unknown parameter`!
/// When properly encoded in big-endian (`[0x81, 0x02]`), the entire 258-byte string is a single
/// `?name=...` filter word and RouterOS returns `Ok([])`.
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_word_len_two_bytes_258_corrupted_words() {
    let mut api = connect_and_login().await;

    // "?name=" (6) + "H"*107 + "=bad=" (5) + "H"*140 = 258 bytes (0x0102).
    let filter_val = format!("{}=bad={}", "H".repeat(107), "H".repeat(140));
    assert_eq!(format!("?name={}", filter_val).len(), 258);

    let interfaces = timeout(
        Duration::from_secs(5),
        api.generic_array_call::<InterfaceName>(
            "/interface/print",
            Some(&[("?name", filter_val.as_str())]),
        ),
    )
    .await
    .expect("timed out waiting for 258-byte word query response")
    .expect("router rejected corrupted sentence from 258-byte word");

    assert!(
        interfaces.is_empty(),
        "expected 0 interfaces matching 252-char name, got {}",
        interfaces.len()
    );
}

/// Tests 3-byte word length encoding where total word length is 16385 (0x4001).
///
/// Even with big-endian `to_be_bytes()`, `((16385u32) | 0xC00000).to_be_bytes()` produces
/// `[0x00, 0xC0, 0x40, 0x01]`. With the slice range bug (`&bytes[..3]` instead of `&bytes[1..]`),
/// the client sends `[0x00, 0xC0, 0x40]`. The leading `0x00` prematurely terminates the
/// sentence on RouterOS before the `?name=...` filter, returning all interfaces instead of 0.
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_word_len_three_bytes_16385_slice_range() {
    let mut api = connect_and_login().await;

    // "?name=" is 6 bytes; 6 + 16379 = 16385 bytes (0x4001).
    let filter_val = "a".repeat(16379);
    assert_eq!(format!("?name={}", filter_val).len(), 16385);

    let interfaces = timeout(
        Duration::from_secs(5),
        api.generic_array_call::<InterfaceName>(
            "/interface/print",
            Some(&[("?name", filter_val.as_str())]),
        ),
    )
    .await
    .expect("timed out waiting for 16385-byte word query response")
    .expect("failed to execute 16385-byte word query");

    assert!(
        interfaces.is_empty(),
        "expected 0 interfaces matching 16379-char name, got {} (sentence was truncated by 0x00 byte!)",
        interfaces.len()
    );
}

/// Verifies that a sentence combining 1-byte, 2-byte, and 3-byte words using RouterOS
/// postfix query operators (`?#|` logical OR) is parsed end-to-end by the router and leaves
/// the multiplexed TCP session clean for subsequent commands.
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_word_len_compound_query_and_session_integrity() {
    let mut api = connect_and_login().await;

    let two_byte_filter = "x".repeat(300); // word len = 306 (2-byte length)
    let three_byte_filter = "y".repeat(16400); // word len = 16406 (3-byte length)

    // Postfix query stack:
    // 1. ?name=<300 'x's>      -> false
    // 2. ?name=<16400 'y's>    -> false
    // 3. ?#|                   -> false || false = false
    // 4. ?name=lo              -> true (only for loopback interface 'lo')
    // 5. ?#|                   -> false || (name == "lo") -> matches only 'lo'
    let interfaces = timeout(
        Duration::from_secs(5),
        api.generic_array_call::<InterfaceName>(
            "/interface/print",
            Some(&[
                ("?name", two_byte_filter.as_str()),
                ("?name", three_byte_filter.as_str()),
                ("?#|", ""),
                ("?name", "lo"),
                ("?#|", ""),
            ]),
        ),
    )
    .await
    .expect("timed out on compound 2-byte + 3-byte query")
    .expect("failed to execute compound 2-byte + 3-byte query");

    assert_eq!(
        interfaces.len(),
        1,
        "expected exactly 1 interface ('lo') from compound OR query, got {:?}",
        interfaces
    );
    assert_eq!(interfaces[0].name, "lo");

    // Verify the TCP stream framing is completely intact by issuing a follow-up command
    // on the exact same connection.
    let identity = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<Identity>("/system/identity/print", None),
    )
    .await
    .expect("timed out on follow-up command after large words")
    .expect("failed follow-up command after large words");

    assert!(!identity.name.is_empty());
}

/// Tests the exact protocol length encoding tier boundaries against the live router:
/// - 127 bytes (`0x7F`, max 1-byte length)
/// - 128 bytes (`0x80`, min 2-byte length)
/// - 16383 bytes (`0x3FFF`, max 2-byte length)
/// - 16384 bytes (`0x4000`, min 3-byte length)
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_word_len_tier_boundaries() {
    let mut api = connect_and_login().await;

    for target_word_len in [0x7Fusize, 0x80, 0x3FFF, 0x4000] {
        // "?name=" prefix is 6 bytes
        let filter_val = "z".repeat(target_word_len - 6);
        assert_eq!(format!("?name={}", filter_val).len(), target_word_len);

        let interfaces = timeout(
            Duration::from_secs(5),
            api.generic_array_call::<InterfaceName>(
                "/interface/print",
                Some(&[("?name", filter_val.as_str())]),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("timed out on boundary word len {:#x}", target_word_len))
        .unwrap_or_else(|e| {
            panic!(
                "router error on boundary word len {:#x}: {:?}",
                target_word_len, e
            )
        });

        assert!(
            interfaces.is_empty(),
            "expected 0 interfaces for boundary word len {:#x}, got {}",
            target_word_len,
            interfaces.len()
        );
    }
}

/// Wrapper future that counts how many times `poll()` is invoked before resolving.
struct PollCounter<F> {
    inner: std::pin::Pin<Box<F>>,
    polls: usize,
}

impl<F: std::future::Future> PollCounter<F> {
    fn new(fut: F) -> Self {
        Self {
            inner: Box::pin(fut),
            polls: 0,
        }
    }
}

impl<F: std::future::Future> std::future::Future for PollCounter<F> {
    type Output = (F::Output, usize);

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.polls += 1;
        match self.inner.as_mut().poll(cx) {
            std::task::Poll::Ready(val) => std::task::Poll::Ready((val, self.polls)),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Verifies that `OneShotCall` and `ArrayListCall` do not busy-spin in `Future::poll`
/// while waiting for the router's network reply (Issue #2).
///
/// Previously, `cx.waker().wake_by_ref()` was called unconditionally before returning
/// `Poll::Pending`, causing Tokio to poll the future thousands of times at 100% CPU
/// during a single network round-trip. With proper waker registration, the future is
/// polled once when initially pending and woken once when `!done` is received.
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_no_busy_spin_on_oneshot_and_array_calls() {
    let mut api = connect_and_login().await;

    // 1. Test OneShotCall poll count over live network RTT
    let (identity_res, oneshot_polls) = timeout(
        Duration::from_secs(5),
        PollCounter::new(api.generic_oneshot_call::<Identity>("/system/identity/print", None)),
    )
    .await
    .expect("timed out on PollCounter oneshot call");

    let identity = identity_res.expect("oneshot call failed");
    assert!(!identity.name.is_empty());
    println!("OneShotCall resolved in {} polls", oneshot_polls);
    assert!(
        oneshot_polls <= 3,
        "OneShotCall busy-spun! Expected <= 3 polls, got {} polls",
        oneshot_polls
    );

    // 2. Test ArrayListCall poll count over live network RTT
    let (interfaces_res, array_polls) = timeout(
        Duration::from_secs(5),
        PollCounter::new(api.generic_array_call::<InterfaceName>("/interface/print", None)),
    )
    .await
    .expect("timed out on PollCounter array call");

    let interfaces = interfaces_res.expect("array call failed");
    assert!(!interfaces.is_empty());
    println!(
        "ArrayListCall ({} items) resolved in {} polls",
        interfaces.len(),
        array_polls
    );
    assert!(
        array_polls <= 3,
        "ArrayListCall busy-spun! Expected <= 3 polls, got {} polls",
        array_polls
    );

    // 3. Test !trap error response also wakes cleanly without busy-spinning
    let (trap_res, trap_polls) = timeout(
        Duration::from_secs(5),
        PollCounter::new(api.generic_oneshot_call::<Identity>(
            "/system/identity/set",
            Some(&[("name", "should-not-work")]),
        )),
    )
    .await
    .expect("timed out on PollCounter trap call");

    assert!(trap_res.is_err(), "expected !trap on read-only user write");
    println!("Trap OneShotCall resolved in {} polls", trap_polls);
    assert!(
        trap_polls <= 3,
        "Trap OneShotCall busy-spun! Expected <= 3 polls, got {} polls",
        trap_polls
    );
}

/// Verifies that when the router terminates the session (`/quit` -> `!fatal` + TCP EOF),
/// `event_loop` exits cleanly without spinning and wakes all in-flight callers and streams
/// (Issue #3).
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_disconnect_on_quit_notifies_inflight_callers_and_streams() {
    use futures::StreamExt;

    let mut api = connect_and_login().await;

    // 1. Start an infinite streaming listener on the connection
    let mut tag = 0u16;
    let mut stream = Box::pin(api.interfaces_changes(&mut tag).await);

    // 2. Send `/quit`, which causes RouterOS to send `!fatal` and close the TCP connection
    let quit_res = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<()>("/quit", None),
    )
    .await
    .expect("in-flight OneShotCall hung after router closed connection on /quit");

    assert!(
        quit_res.is_err(),
        "expected Err(EndOfStream) on /quit disconnection, got {:?}",
        quit_res
    );

    // 3. The active streaming listener must also be woken and terminate (return None)
    // Drain any queued initial events until None is reached.
    timeout(Duration::from_secs(5), async {
        while stream.next().await.is_some() {}
    })
    .await
    .expect("active StreamingCall hung after router closed connection");
}

/// Verifies that `generic_oneshot_call` does not panic when the router omits `!re`
/// and returns only a `!done` sentence (e.g. a filtered query matching 0 items) (Issue #4).
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_oneshot_call_with_empty_reply_does_not_panic() {
    let mut api = connect_and_login().await;

    // Query a non-existent interface via `generic_oneshot_call`:
    // RouterOS replies only with `!done .tag=xxx` (no `!re` frame).
    let res = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<InterfaceName>(
            "/interface/print",
            Some(&[("?name", "nonexistent-interface-issue-4")]),
        ),
    )
    .await
    .expect("timed out waiting for empty oneshot response");

    assert!(
        res.is_err(),
        "expected Err(EmptyResponse) when router returns only !done without !re, got {:?}",
        res
    );

    // Verify that `event_loop` is still alive and healthy for subsequent calls
    let identity = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<Identity>("/system/identity/print", None),
    )
    .await
    .expect("timed out on follow-up call after empty oneshot response")
    .expect("follow-up call failed after empty oneshot response");

    assert!(!identity.name.is_empty());
}

/// Verifies that completed requests (OneShotCall, ArrayListCall) and cancelled streams
/// are cleanly removed from the internal `SharedTagMap`, preventing memory leaks and
/// tag space exhaustion (Issue #5).
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_tag_map_cleaned_up_after_commands_and_cancel() {
    let mut api = connect_and_login().await;

    // After login, tag_map should have 0 active calls.
    assert_eq!(api.active_calls_count(), 0);

    // 1. Run multiple oneshot calls sequentially
    for _ in 0..10 {
        let identity = timeout(
            Duration::from_secs(5),
            api.generic_oneshot_call::<Identity>("/system/identity/print", None),
        )
        .await
        .expect("timed out fetching identity")
        .expect("failed to fetch identity");
        assert!(!identity.name.is_empty());

        // Each call should be removed upon completion
        assert_eq!(api.active_calls_count(), 0);
    }

    // 2. Run an array call
    let ifaces = timeout(
        Duration::from_secs(5),
        api.generic_array_call::<InterfaceName>("/interface/print", None),
    )
    .await
    .expect("timed out fetching interfaces")
    .expect("failed to fetch interfaces");
    assert!(!ifaces.is_empty());
    assert_eq!(api.active_calls_count(), 0);

    // 3. Start a streaming listener: tag_map active calls should increase to 1
    let mut stream_tag = 0u16;
    let _stream = api.interfaces_changes(&mut stream_tag).await;
    assert_eq!(api.active_calls_count(), 1);

    // 4. Cancel the stream: RouterOS will reply with !trap/!done on stream_tag,
    // and !done on cancel_tag. Both should be removed, active calls back to 0.
    let _cancel_res = timeout(Duration::from_secs(5), api.cancel(stream_tag))
        .await
        .expect("timed out cancelling stream");

    // Wait briefly if needed for RouterOS to finish sending the stream's !done
    let start = std::time::Instant::now();
    while api.active_calls_count() > 0 && start.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(api.active_calls_count(), 0);
}

/// Verifies that deserialization handles attribute values containing '=' characters,
/// and that HashMap and Trap deserialization work properly against the live router (Issue #6).
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_deserialize_hashmap_and_trap_with_equals() {
    use std::collections::HashMap;

    let mut api = connect_and_login().await;

    // 1. Fetch system resources as a generic HashMap<String, String>
    let res = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<HashMap<String, String>>("/system/resource/print", None),
    )
    .await
    .expect("timed out fetching system resources as HashMap")
    .expect("failed to deserialize system resources into HashMap");

    assert!(
        res.contains_key("uptime"),
        "HashMap should contain 'uptime'"
    );
    assert!(
        res.contains_key("version"),
        "HashMap should contain 'version'"
    );
    println!("Fetched resources as HashMap: {} entries", res.len());

    // 2. Trigger a trap from the router with an '=' sign in the parameters/command
    let trap_res = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<()>(
            "/system/identity/set",
            Some(&[("name", "test=val=with=multiple=equals=signs")]),
        ),
    )
    .await
    .expect("timed out waiting for trap response");

    assert!(
        trap_res.is_err(),
        "expected error response from unauthorized set with equals"
    );

    // 3. Verify connection and deserializer remain in a healthy state for subsequent commands
    let identity = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<Identity>("/system/identity/print", None),
    )
    .await
    .expect("timed out fetching identity after trap")
    .expect("subsequent call failed after trap with equals");

    assert!(!identity.name.is_empty());
}

/// Verifies that words containing non-ASCII / UTF-8 characters in queries or responses
/// are safely handled without UB or panics (Issue #8).
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_non_ascii_query_and_response_handling() {
    let mut api = connect_and_login().await;

    // Send a query containing non-ASCII characters (e.g. searching for an interface with accents/symbols)
    let res = timeout(
        Duration::from_secs(5),
        api.generic_array_call::<InterfaceName>(
            "/interface/print",
            Some(&[("?name", "café-éèà-test-🚀")]),
        ),
    )
    .await
    .expect("timed out querying interface with non-ASCII name")
    .expect("query with non-ASCII characters failed");

    // Router should return an empty list without error or corruption
    assert!(res.is_empty());

    // Follow-up standard call to ensure connection remains fully functional
    let identity = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<Identity>("/system/identity/print", None),
    )
    .await
    .expect("timed out on follow-up identity call")
    .expect("follow-up call failed");
    assert!(!identity.name.is_empty());
}

/// Verifies that MikrotikAPI implements Send and can be passed across thread boundaries
/// into `tokio::spawn` worker tasks (Issue #9).
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_mikrotik_api_send_and_spawn_across_threads() {
    let api = connect_and_login().await;

    // Move `api` across thread boundary into a background tokio worker task
    let handle = tokio::spawn(async move {
        let mut api = api;
        let identity = api
            .generic_oneshot_call::<Identity>("/system/identity/print", None)
            .await
            .expect("call failed inside spawned tokio task");
        identity.name
    });

    let name = timeout(Duration::from_secs(5), handle)
        .await
        .expect("timed out waiting for spawned tokio task")
        .expect("spawned task panicked");

    assert!(!name.is_empty(), "router identity name should not be empty");
}

/// Verifies that frame parsing and tag handling in event_loop handle varied RouterOS replies
/// cleanly without dropping frames or panicking (Issue #7).
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_event_loop_frame_dispatching_and_tag_handling() {
    let mut api = connect_and_login().await;

    // 1. OneShotCall: /system/identity/print
    let identity = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<Identity>("/system/identity/print", None),
    )
    .await
    .expect("timed out fetching identity")
    .expect("fetching identity failed");
    assert!(!identity.name.is_empty());

    // 2. ArrayListCall: /interface/print with proplist
    let interfaces = timeout(
        Duration::from_secs(5),
        api.generic_array_call::<InterfaceName>(
            "/interface/print",
            Some(&[(".proplist", "name,type,running")]),
        ),
    )
    .await
    .expect("timed out fetching interfaces with proplist")
    .expect("fetching interfaces failed");
    assert!(!interfaces.is_empty());

    // 3. BasicResources: multiple attributes returned by router
    let res = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<BasicResources>("/system/resource/print", None),
    )
    .await
    .expect("timed out fetching system resources")
    .expect("system_resources call failed");
    assert!(!res.version.is_empty());

    // 4. Multiple rapid queries in succession
    for _ in 0..5 {
        let id = timeout(
            Duration::from_secs(5),
            api.generic_oneshot_call::<Identity>("/system/identity/print", None),
        )
        .await
        .expect("timed out in loop")
        .expect("call in loop failed");
        assert_eq!(id.name, identity.name);
    }
}

/// Verifies that SystemResources deserialization succeeds against the live router,
/// correctly handling memory >= 4 GiB (u64), optional factory-software, and unexpected fields (Issue #10).
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_system_resources_deserialization_live() {
    let mut api = connect_and_login().await;

    let resources = timeout(Duration::from_secs(5), api.system_resources())
        .await
        .expect("timed out fetching system resources")
        .expect("system_resources deserialization failed on live router");

    println!(
        "Live SystemResources: board={}, arch={}, version={}, total_mem={}, free_mem={}",
        resources.board_name,
        resources.architecture_name,
        resources.version,
        resources.total_memory,
        resources.free_memory
    );

    assert!(!resources.board_name.is_empty());
    assert!(!resources.version.is_empty());
    assert_eq!(resources.platform, "MikroTik");
    assert!(
        resources.total_memory > 0,
        "total_memory should be non-zero"
    );
    assert!(resources.free_memory > 0, "free_memory should be non-zero");
    assert!(
        resources.total_hdd_space > 0,
        "total_hdd_space should be non-zero"
    );

    // On CCR2004-16G-2S+, total_memory is 4294967296 (4 GiB), which previously overflowed u32
    if resources.board_name.contains("CCR2004") {
        assert_eq!(
            resources.total_memory, 4294967296,
            "CCR2004 should have 4 GiB of RAM (4294967296 bytes)"
        );
        assert!(
            resources.factory_software.is_some(),
            "CCR2004 hardware should report factory-software"
        );
    }
}

/// Verifies that Interface deserialization succeeds across all interfaces on the live router,
/// correctly handling optional FastPath counters, optional MTU / actual-mtu, and RouterOS 7
/// last-link-up-time alias (Issue #10).
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_interfaces_deserialization_live() {
    let mut api = connect_and_login().await;

    let ifaces = timeout(Duration::from_secs(5), api.interfaces())
        .await
        .expect("timed out fetching interfaces")
        .expect("interfaces deserialization failed on live router");

    println!("Successfully deserialized {} live interfaces", ifaces.len());
    assert!(
        !ifaces.is_empty(),
        "router should have at least one interface"
    );

    let mut found_last_link_up = false;
    let mut found_mtu = false;

    for iface in &ifaces {
        assert!(!iface.id.is_empty(), "interface ID should not be empty");
        assert!(!iface.name.is_empty(), "interface name should not be empty");
        assert!(
            !iface.iface_type.is_empty(),
            "interface type should not be empty"
        );

        if iface.last_link_up.is_some() {
            found_last_link_up = true;
        }
        if iface.mtu.is_some() {
            found_mtu = true;
        }
    }

    assert!(
        found_mtu,
        "expected at least one interface to have an MTU configured"
    );
    assert!(
        found_last_link_up,
        "expected at least one active interface to have last_link_up populated via last-link-up-time alias"
    );
}

/// Verifies that SentenceDeserializer handles signed integers, floats, newtype wrappers,
/// and derived field enums against the live router (Issue #29).
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_field_enums_signed_ints_and_newtypes_live() {
    let mut api = connect_and_login().await;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "kebab-case")]
    enum LiveInterfaceType {
        Ether,
        Bridge,
        Loopback,
        #[serde(other)]
        Other,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct InterfaceId(String);

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    struct InterfaceWithTypeEnum {
        #[serde(rename = ".id")]
        id: InterfaceId,
        name: String,
        #[serde(rename = "type")]
        iface_type: LiveInterfaceType,
        #[serde(default)]
        link_downs: Option<i32>,
        rx_byte: i64,
        running: bool,
    }

    let ifaces: Vec<InterfaceWithTypeEnum> = timeout(
        Duration::from_secs(5),
        api.generic_array_call("/interface/print", None),
    )
    .await
    .expect("timed out querying interfaces")
    .expect("failed to deserialize interfaces with enum and signed ints");

    assert!(!ifaces.is_empty(), "expected at least one interface");
    for iface in ifaces {
        assert!(!iface.id.0.is_empty(), "id newtype should be populated");
        assert!(!iface.name.is_empty());
        assert!(iface.rx_byte >= 0, "rx_byte parsed as signed i64");
        let _ = iface.running;
        if let Some(downs) = iface.link_downs {
            assert!(downs >= 0);
        }
        assert!(matches!(
            iface.iface_type,
            LiveInterfaceType::Ether
                | LiveInterfaceType::Bridge
                | LiveInterfaceType::Loopback
                | LiveInterfaceType::Other
        ));
    }
}

/// Verifies that calling `generic_oneshot_call` and `generic_array_call` with a mismatched struct
/// returns `Err(Error::Deserialize(_))` (and NOT `Err(Error::EndOfStream)`), and subsequent calls
/// on the same `MikrotikAPI` instance continue to succeed (Issue #25).
#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_deserialization_failure_returns_deserialize_error_and_preserves_connection() {
    let mut api = connect_and_login().await;

    #[derive(Debug, Deserialize)]
    struct MismatchedStruct {
        #[allow(dead_code)]
        non_existent_required_field: u64,
    }

    // 1. generic_oneshot_call with mismatched struct fails deserialization
    let oneshot_err = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<MismatchedStruct>("/system/identity/print", None),
    )
    .await
    .expect("timed out waiting for generic_oneshot_call")
    .expect_err("expected generic_oneshot_call to fail with deserialization error");

    assert!(
        matches!(oneshot_err, Error::Deserialize(_)),
        "expected Error::Deserialize, got {:?}",
        oneshot_err
    );

    // 2. generic_array_call with mismatched struct fails deserialization
    let array_err = timeout(
        Duration::from_secs(5),
        api.generic_array_call::<MismatchedStruct>("/interface/print", None),
    )
    .await
    .expect("timed out waiting for generic_array_call")
    .expect_err("expected generic_array_call to fail with deserialization error");

    assert!(
        matches!(array_err, Error::Deserialize(_)),
        "expected Error::Deserialize, got {:?}",
        array_err
    );

    // 3. Immediately verify the connection is STILL valid and functional on the same `api` instance
    let identity = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<Identity>("/system/identity/print", None),
    )
    .await
    .expect("timed out fetching identity after deserialization failure")
    .expect("failed to fetch identity on preserved connection");
    assert!(!identity.name.trim().is_empty());

    let resources = timeout(Duration::from_secs(5), api.system_resources())
        .await
        .expect("timed out fetching system resources after deserialization failure")
        .expect("failed to fetch system resources on preserved connection");
    assert!(!resources.board_name.is_empty());
}
