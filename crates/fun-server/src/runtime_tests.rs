use super::*;

#[cfg(unix)]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

#[cfg(unix)]
struct TempDirectory(PathBuf);

#[cfg(unix)]
impl TempDirectory {
    fn new() -> Self {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let unique_id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fun-worker-test-{}-{unique_id}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

#[cfg(unix)]
impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn write_worker_script(directory: &Path, contents: &str) -> PathBuf {
    let script = directory.join("worker.sh");
    fs::write(&script, contents).unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).unwrap();
    script
}

#[cfg(unix)]
fn test_request() -> FunctionRequest {
    FunctionRequest {
        request_id: "req-1".into(),
        method: "POST".into(),
        path: "/".into(),
        query: Vec::new(),
        path_parameters: Vec::new(),
        headers: Vec::new(),
        body: Vec::new(),
    }
}

#[test]
fn validates_response_limits_and_hop_by_hop_headers() {
    let valid = FunctionResponse {
        status: 201,
        headers: vec![("content-length".into(), "2".into())],
        body: b"ok".to_vec(),
    };
    assert!(validate_response(&valid, 2).is_ok());

    let oversized = FunctionResponse {
        body: b"too large".to_vec(),
        ..valid.clone()
    };
    assert!(matches!(
        validate_response(&oversized, 2),
        Err(InvocationError::Worker(_))
    ));

    let invalid_header = FunctionResponse {
        headers: vec![("transfer-encoding".into(), "chunked".into())],
        ..valid
    };
    assert!(matches!(
        validate_response(&invalid_header, 8),
        Err(InvocationError::Worker(_))
    ));
}

#[test]
fn rejects_body_for_no_content_status() {
    let response = FunctionResponse {
        status: 204,
        body: b"unexpected".to_vec(),
        ..FunctionResponse::default()
    };
    assert!(matches!(
        validate_response(&response, 32),
        Err(InvocationError::Worker(_))
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn malformed_worker_is_replaced_for_the_next_request() {
    let temp = TempDirectory::new();
    let script = write_worker_script(
        &temp.0,
        "#!/bin/sh\nIFS= read -r request\nif [ ! -f marker ]; then\n  echo 'not-json'\n  : > marker\n  exit 0\nfi\nprintf '%s\\n' '{\"version\":1,\"request_id\":\"req-1\",\"outcome\":\"response\",\"status\":200,\"headers\":[],\"body\":\"\"}'\n",
    );
    let mut worker = WorkerProcess::spawn(
        WorkerCommand::staged(script),
        temp.0.clone(),
        HashMap::new(),
    )
    .await
    .unwrap();
    let first_result = worker
        .invoke(test_request(), Duration::from_secs(2), 1024)
        .await;
    assert!(
        matches!(first_result, Err(InvocationError::Worker(_))),
        "expected malformed frame error, received {first_result:?}"
    );
    let response = worker
        .invoke(test_request(), Duration::from_secs(2), 1024)
        .await
        .unwrap();
    assert!(temp.0.exists());
    assert_eq!(response.status, 200);
    worker.terminate().await;
}

#[cfg(unix)]
#[tokio::test]
async fn times_out_and_reaps_worker() {
    let temp = TempDirectory::new();
    let script = write_worker_script(&temp.0, "#!/bin/sh\nexec /bin/sleep 30\n");
    let mut worker = WorkerProcess::spawn(
        WorkerCommand::staged(script),
        temp.0.clone(),
        HashMap::new(),
    )
    .await
    .unwrap();
    let result = worker
        .invoke(test_request(), Duration::from_millis(50), 1024)
        .await;
    assert!(
        matches!(result, Err(InvocationError::Timeout)),
        "expected timeout, received {result:?}"
    );
    assert!(temp.0.exists());
    assert!(worker.child.try_wait().unwrap().is_some());
}
