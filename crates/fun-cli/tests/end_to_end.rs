use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct TestProject(std::path::PathBuf);

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct RunningServer(Child);

impl RunningServer {
    fn shutdown(mut self) -> ExitStatus {
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGINT);
        }
        self.0.wait().expect("wait for server shutdown")
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn temporary_project() -> TestProject {
    static NEXT_PROJECT_ID: AtomicU64 = AtomicU64::new(1);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_nanos();
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CLI crate is within the workspace");
    let path = workspace_root.join(format!(
        "rust-fun-e2e-{}-{nonce}-{}",
        std::process::id(),
        NEXT_PROJECT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    TestProject(path)
}

#[test]
fn init_list_invoke_and_http_dev_work_end_to_end() {
    let project = temporary_project();
    let cli = env!("CARGO_BIN_EXE_fun");

    let initialized = Command::new(cli)
        .arg("init")
        .arg(&project.0)
        .output()
        .expect("run fun init");
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );

    let listed = Command::new(cli)
        .arg("list")
        .current_dir(&project.0)
        .output()
        .expect("run fun list");
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).contains("hello"));

    let invoked = Command::new(cli)
        .args(["invoke", "hello", "--json", r#"{"name":"Rust"}"#])
        .current_dir(&project.0)
        .env("CARGO_TARGET_DIR", shared_target_directory())
        .output()
        .expect("run fun invoke");
    assert!(
        invoked.status.success(),
        "{}",
        String::from_utf8_lossy(&invoked.stderr)
    );
    assert!(String::from_utf8_lossy(&invoked.stdout).contains("Hello, Rust!"));

    let tested = Command::new(cli)
        .arg("test")
        .current_dir(&project.0)
        .env("CARGO_TARGET_DIR", shared_target_directory())
        .output()
        .expect("run fun test");
    assert!(
        tested.status.success(),
        "{}{}",
        String::from_utf8_lossy(&tested.stdout),
        String::from_utf8_lossy(&tested.stderr)
    );

    let manifest = fs::read_to_string(project.0.join("rust-fun.toml")).unwrap();
    fs::write(
        project.0.join("rust-fun.toml"),
        manifest.replace(
            "max_request_body_bytes = 1048576",
            "max_request_body_bytes = 32",
        ),
    )
    .unwrap();
    fs::write(project.0.join(".env"), "GREETING=Welcome\n").unwrap();
    let server_log_path = project.0.join("fun-dev.log");
    let server_log = fs::File::create(&server_log_path).unwrap();
    let port = free_loopback_port();
    let server = Command::new(cli)
        .args(["dev", "--port", &port.to_string()])
        .current_dir(&project.0)
        .env("CARGO_TARGET_DIR", shared_target_directory())
        .stdout(Stdio::null())
        .stderr(Stdio::from(server_log))
        .spawn()
        .expect("start fun dev");
    let server = RunningServer(server);
    let response = request_until_ready(port);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("Welcome, HTTP!"), "{response}");
    assert!(
        response.contains("content-type: application/json"),
        "{response}"
    );

    let health = send_http_request(port, "GET", "/__fun/health", b"");
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");
    assert!(health.contains(r#"{"status":"ok"}"#), "{health}");

    let method_mismatch = send_request(port, "GET", b"");
    assert!(
        method_mismatch.starts_with("HTTP/1.1 405") && method_mismatch.contains("allow: POST"),
        "{method_mismatch}"
    );

    let oversized = send_request(port, "POST", &[b'x'; 33]);
    assert!(oversized.starts_with("HTTP/1.1 413"), "{oversized}");

    let function_error = send_request(port, "POST", b"not-json");
    assert!(
        function_error.starts_with("HTTP/1.1 500"),
        "{function_error}"
    );
    assert!(
        function_error.contains("function failed"),
        "{function_error}"
    );

    let after_error = send_request(port, "POST", br#"{"name":"Again"}"#);
    assert!(after_error.starts_with("HTTP/1.1 200"), "{after_error}");
    assert!(after_error.contains("Welcome, Again!"), "{after_error}");

    fs::write(project.0.join(".env"), "GREETING=Howdy\n").unwrap();
    let env_reload = wait_for_response(port, "Howdy, Reloaded!", "Reloaded").unwrap_or_else(|| {
        panic!(
            "environment reload timed out; watcher log:\n{}",
            fs::read_to_string(&server_log_path).unwrap_or_default()
        )
    });
    assert!(env_reload.starts_with("HTTP/1.1 200"), "{env_reload}");

    let source_path = project.0.join("functions/hello/src/main.rs");
    let source = fs::read_to_string(&source_path).unwrap();
    assert!(source.contains(r#"format!("{greeting}, {name}!")"#));
    let updated_source = source.replace(
        r#"format!("{greeting}, {name}!")"#,
        r#"format!("Bonjour, {name}!")"#,
    );
    fs::write(&source_path, &updated_source).unwrap();
    let source_reload = wait_for_response(port, "Bonjour, Watched!", "Watched")
        .expect("source watcher did not reload function");
    assert!(source_reload.starts_with("HTTP/1.1 200"), "{source_reload}");

    fs::write(&source_path, "this is not valid Rust\n").unwrap();
    wait_for_log(
        &server_log_path,
        "function rebuild failed; keeping last working worker",
    );
    let last_working = send_request(port, "POST", br#"{"name":"StillWorking"}"#);
    assert!(
        last_working.contains("Bonjour, StillWorking!"),
        "{last_working}"
    );

    fs::write(&source_path, updated_source).unwrap();
    let recovered = wait_for_response(port, "Bonjour, Recovered!", "Recovered")
        .expect("function did not recover after the source was fixed");
    assert!(recovered.starts_with("HTTP/1.1 200"), "{recovered}");

    let status = server.shutdown();
    assert!(status.success(), "fun dev exited with {status}");
}

#[test]
fn generated_non_rust_runtimes_share_the_worker_contract() {
    let cli = env!("CARGO_BIN_EXE_fun");
    for (runtime, expected) in [
        ("nodejs", "Welcome, Node!"),
        ("python3", "Welcome, Python!"),
        ("go1.x", "Welcome, Go!"),
        ("provided", "Hello from provided runtime"),
    ] {
        let project = temporary_project();
        let initialized = Command::new(cli)
            .arg("init")
            .arg("--runtime")
            .arg(runtime)
            .arg(&project.0)
            .output()
            .expect("initialize runtime fixture");
        assert!(
            initialized.status.success(),
            "runtime={runtime}: {}",
            String::from_utf8_lossy(&initialized.stderr)
        );
        fs::write(project.0.join(".env"), "GREETING=Welcome\n").unwrap();
        let manifest_path = project.0.join("rust-fun.toml");
        let manifest = fs::read_to_string(&manifest_path).unwrap();
        fs::write(
            &manifest_path,
            manifest.replace("default_timeout_ms = 5000", "default_timeout_ms = 30000"),
        )
        .unwrap();

        let listed = Command::new(cli)
            .arg("list")
            .current_dir(&project.0)
            .output()
            .expect("list runtime fixture");
        assert!(listed.status.success());
        assert!(
            String::from_utf8_lossy(&listed.stdout).contains(runtime),
            "runtime={runtime}: {}",
            String::from_utf8_lossy(&listed.stdout)
        );

        let invoked = Command::new(cli)
            .args(["invoke", "hello", "--json"])
            .arg(if runtime == "nodejs" {
                r#"{"name":"Node"}"#
            } else if runtime == "python3" {
                r#"{"name":"Python"}"#
            } else if runtime == "go1.x" {
                r#"{"name":"Go"}"#
            } else {
                "{}"
            })
            .current_dir(&project.0)
            .output()
            .expect("invoke runtime fixture");
        assert!(
            invoked.status.success(),
            "runtime={runtime}: {}{}",
            String::from_utf8_lossy(&invoked.stdout),
            String::from_utf8_lossy(&invoked.stderr)
        );
        assert!(
            String::from_utf8_lossy(&invoked.stdout).contains(expected),
            "runtime={runtime}: {}",
            String::from_utf8_lossy(&invoked.stdout)
        );

        if runtime == "nodejs" {
            let port = free_loopback_port();
            let server = Command::new(cli)
                .args(["dev", "--no-watch", "--port", &port.to_string()])
                .current_dir(&project.0)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start Node.js dev server");
            let server = RunningServer(server);
            let response = request_until_ready(port);
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(response.contains("Welcome, HTTP!"), "{response}");
            assert!(server.shutdown().success());
        }

        let tested = Command::new(cli)
            .arg("test")
            .current_dir(&project.0)
            .output()
            .expect("test runtime fixture");
        assert!(
            tested.status.success(),
            "runtime={runtime}: {}{}",
            String::from_utf8_lossy(&tested.stdout),
            String::from_utf8_lossy(&tested.stderr)
        );
        if runtime == "provided" {
            assert!(
                String::from_utf8_lossy(&tested.stdout).contains("Skipping function `hello`"),
                "{}",
                String::from_utf8_lossy(&tested.stdout)
            );
        }
        if runtime == "python3" {
            let test_output = format!(
                "{}{}",
                String::from_utf8_lossy(&tested.stdout),
                String::from_utf8_lossy(&tested.stderr)
            );
            assert!(
                test_output.contains("Ran 1 test"),
                "expected generated Python unittest to run: {test_output}"
            );
        }
    }
}

fn shared_target_directory() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CLI crate is within the workspace")
        .join("target")
}

fn free_loopback_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .expect("allocate ephemeral loopback port")
        .port()
}

fn request_until_ready(port: u16) -> String {
    let body = br#"{"name":"HTTP"}"#;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(response) = try_request(port, body) {
            return response;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("fun dev did not respond before the deadline");
}

fn send_request(port: u16, method: &str, body: &[u8]) -> String {
    send_http_request(port, method, "/api/hello", body)
}

fn send_http_request(port: u16, method: &str, path: &str, body: &[u8]) -> String {
    try_http_request(port, method, path, body).expect("connect to running fun dev server")
}

fn try_request(port: u16, body: &[u8]) -> Option<String> {
    try_http_request(port, "POST", "/api/hello", body)
}

fn wait_for_response(port: u16, expected: &str, name: &str) -> Option<String> {
    let body = format!(r#"{{"name":"{name}"}}"#);
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(response) = try_http_request(port, "POST", "/api/hello", body.as_bytes()) {
            if response.contains(expected) {
                return Some(response);
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

fn wait_for_log(path: &std::path::Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if fs::read_to_string(path).is_ok_and(|contents| contents.contains(expected)) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("server did not log expected message: {expected}");
}

fn try_http_request(port: u16, method: &str, path: &str, body: &[u8]) -> Option<String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len(),
    );
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(200)) else {
        return None;
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    if stream.write_all(request.as_bytes()).is_err() || stream.write_all(body).is_err() {
        return None;
    }
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .ok()
        .filter(|_| !response.is_empty())?;
    Some(response)
}
