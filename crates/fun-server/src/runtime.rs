use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use fun_core::{
    decode_response, encode_request, DiscoveredFunction, FunError, FunctionError, FunctionRequest,
    FunctionResponse, Project, RuntimeKind,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, OwnedSemaphorePermit, RwLock, Semaphore},
    time::timeout,
};
use tracing::{debug, warn};

pub struct WorkerPool {
    workers: HashMap<String, Arc<Mutex<WorkerProcess>>>,
    environment: RwLock<HashMap<OsString, OsString>>,
    permits: Arc<Semaphore>,
    timeout: Duration,
    max_request_body_bytes: usize,
    max_response_body_bytes: usize,
}

#[derive(Debug)]
pub enum InvocationError {
    Busy,
    Timeout,
    RequestTooLarge,
    Worker(String),
    Function(FunctionError),
    NotFound,
}

struct WorkerProcess {
    command: WorkerCommand,
    working_directory: PathBuf,
    environment: HashMap<OsString, OsString>,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

#[derive(Clone)]
struct WorkerCommand {
    executable: OsString,
    arguments: Vec<OsString>,
}

impl WorkerCommand {
    fn new(executable: impl Into<OsString>, arguments: Vec<OsString>) -> Self {
        Self {
            executable: executable.into(),
            arguments,
        }
    }

    fn staged(path: PathBuf) -> Self {
        Self::new(path.into_os_string(), Vec::new())
    }
}

impl WorkerPool {
    pub async fn build_and_start(project: &Project) -> Result<Self, FunError> {
        Self::build_selected(project, None).await
    }

    pub async fn build_and_start_function(
        project: &Project,
        function_name: &str,
    ) -> Result<Self, FunError> {
        if !project.functions.contains_key(function_name) {
            return Err(FunError::FunctionNotFound(function_name.to_owned()));
        }
        Self::build_selected(project, Some(function_name)).await
    }

    async fn build_selected(project: &Project, selected: Option<&str>) -> Result<Self, FunError> {
        let target_directory = target_directory(&project.root);
        let mut workers = HashMap::new();
        let environment: HashMap<OsString, OsString> =
            project.worker_environment.clone().into_iter().collect();
        for function in project.functions.values() {
            if selected.is_some_and(|name| name != function.name) {
                continue;
            }
            let command = prepare_worker(function, &project.root, &target_directory).await?;
            let worker = WorkerProcess::spawn(
                command,
                function.working_directory.clone(),
                environment.clone(),
            )
            .await
            .map_err(FunError::Worker)?;
            workers.insert(function.name.clone(), Arc::new(Mutex::new(worker)));
        }
        Ok(Self {
            workers,
            environment: RwLock::new(environment),
            permits: Arc::new(Semaphore::new(project.config.functions.max_concurrency)),
            timeout: Duration::from_millis(project.config.functions.default_timeout_ms),
            max_request_body_bytes: project.config.functions.max_request_body_bytes,
            max_response_body_bytes: project.config.functions.max_response_body_bytes,
        })
    }

    pub async fn invoke(
        &self,
        function_name: &str,
        request: FunctionRequest,
    ) -> Result<FunctionResponse, InvocationError> {
        let worker = self
            .workers
            .get(function_name)
            .cloned()
            .ok_or(InvocationError::NotFound)?;
        if request.body.len() > self.max_request_body_bytes {
            return Err(InvocationError::RequestTooLarge);
        }
        let _permit: OwnedSemaphorePermit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| InvocationError::Busy)?;
        let Ok(mut worker) = worker.try_lock() else {
            return Err(InvocationError::Busy);
        };
        worker
            .invoke(request, self.timeout, self.max_response_body_bytes)
            .await
    }

    pub async fn shutdown(&self) {
        for worker in self.workers.values() {
            worker.lock().await.terminate().await;
        }
    }

    pub async fn reload_function(
        &self,
        project: &Project,
        function_name: &str,
    ) -> Result<(), FunError> {
        let function = project
            .functions
            .get(function_name)
            .ok_or_else(|| FunError::FunctionNotFound(function_name.to_owned()))?;
        let environment = self.environment.read().await.clone();
        let target_directory = target_directory(&project.root);
        let command = prepare_worker(function, &project.root, &target_directory).await?;
        let replacement =
            WorkerProcess::spawn(command, function.working_directory.clone(), environment)
                .await
                .map_err(FunError::Worker)?;
        let worker = self
            .workers
            .get(function_name)
            .ok_or_else(|| FunError::FunctionNotFound(function_name.to_owned()))?;
        let mut current = worker.lock().await;
        current.terminate().await;
        *current = replacement;
        Ok(())
    }

    pub async fn reload_environment(
        &self,
        environment: HashMap<OsString, OsString>,
    ) -> Result<(), FunError> {
        let mut replacements = HashMap::new();
        for (name, worker) in &self.workers {
            let current = worker.lock().await;
            replacements.insert(
                name.clone(),
                WorkerProcess::spawn(
                    current.command.clone(),
                    current.working_directory.clone(),
                    environment.clone(),
                )
                .await
                .map_err(FunError::Worker)?,
            );
        }
        for (name, replacement) in replacements {
            if let Some(worker) = self.workers.get(&name) {
                let mut current = worker.lock().await;
                current.terminate().await;
                *current = replacement;
            }
        }
        *self.environment.write().await = environment;
        Ok(())
    }
}

impl WorkerProcess {
    async fn spawn(
        command: WorkerCommand,
        working_directory: PathBuf,
        environment: HashMap<OsString, OsString>,
    ) -> Result<Self, String> {
        let mut process = Command::new(&command.executable);
        process.args(&command.arguments);
        let mut child = process
            .current_dir(&working_directory)
            .env_clear()
            .envs(&environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                format!(
                    "could not start worker {}: {error}",
                    command.executable.to_string_lossy()
                )
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "worker stdin was not available".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "worker stdout was not available".to_owned())?;
        tokio::time::sleep(Duration::from_millis(25)).await;
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("could not inspect worker startup: {error}"))?
        {
            return Err(format!(
                "worker {} exited during startup with {status}",
                command.executable.to_string_lossy()
            ));
        }
        Ok(Self {
            command,
            working_directory,
            environment,
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    async fn ensure_running(&mut self) -> Result<(), String> {
        if self
            .child
            .try_wait()
            .map_err(|error| format!("could not inspect worker process: {error}"))?
            .is_some()
        {
            let replacement = Self::spawn(
                self.command.clone(),
                self.working_directory.clone(),
                self.environment.clone(),
            )
            .await?;
            *self = replacement;
        }
        Ok(())
    }

    async fn invoke(
        &mut self,
        request: FunctionRequest,
        timeout_duration: Duration,
        max_response_body_bytes: usize,
    ) -> Result<FunctionResponse, InvocationError> {
        let request_id = request.request_id.clone();
        let line =
            encode_request(request).map_err(|error| InvocationError::Worker(error.to_string()))?;
        let max_line_bytes = max_response_body_bytes
            .saturating_mul(2)
            .saturating_add(64 * 1024);
        let exchange = async {
            self.ensure_running().await?;
            self.stdin
                .write_all(&line)
                .await
                .map_err(|error| format!("failed writing worker request: {error}"))?;
            self.stdin
                .flush()
                .await
                .map_err(|error| format!("failed flushing worker request: {error}"))?;

            let mut response_line = Vec::new();
            let read = (&mut self.stdout)
                .take(max_line_bytes as u64)
                .read_until(b'\n', &mut response_line)
                .await
                .map_err(|error| format!("failed reading worker response: {error}"))?;
            if read == 0 {
                return Err("worker closed stdout before responding".to_owned());
            }
            if response_line.last() != Some(&b'\n') {
                return Err("worker response exceeded the protocol frame limit".to_owned());
            }
            if response_line.len() > max_line_bytes {
                return Err("worker response exceeded the protocol frame limit".to_owned());
            }
            decode_response(&response_line, &request_id)
                .map_err(|error| format!("invalid worker response: {error}"))
        };
        match timeout(timeout_duration, exchange).await {
            Ok(Ok(Ok(response))) => match validate_response(&response, max_response_body_bytes) {
                Ok(()) => Ok(response),
                Err(error) => {
                    self.terminate().await;
                    Err(error)
                }
            },
            Ok(Ok(Err(error))) => Err(InvocationError::Function(error)),
            Ok(Err(message)) => {
                warn!(error = %message, "function worker failed");
                self.terminate().await;
                Err(InvocationError::Worker(message))
            }
            Err(_) => {
                self.terminate().await;
                Err(InvocationError::Timeout)
            }
        }
    }

    async fn terminate(&mut self) {
        if let Err(error) = self.child.kill().await {
            debug!(%error, "worker process had already stopped");
        }
        if let Err(error) = self.child.wait().await {
            warn!(%error, "failed to reap worker process");
        }
    }
}

fn validate_response(
    response: &FunctionResponse,
    max_response_body_bytes: usize,
) -> Result<(), InvocationError> {
    if !(200..=599).contains(&response.status) {
        return Err(InvocationError::Worker(format!(
            "worker returned invalid HTTP status {}",
            response.status
        )));
    }
    if matches!(response.status, 204 | 304) && !response.body.is_empty() {
        return Err(InvocationError::Worker(format!(
            "worker returned a body for HTTP status {}",
            response.status
        )));
    }
    if response.body.len() > max_response_body_bytes {
        return Err(InvocationError::Worker(format!(
            "worker response body exceeds the configured limit of {max_response_body_bytes} bytes"
        )));
    }
    let mut has_content_length = false;
    for (name, value) in &response.headers {
        let normalized_name = name.to_ascii_lowercase();
        if http::header::HeaderName::from_bytes(name.as_bytes()).is_err()
            || http::header::HeaderValue::from_str(value).is_err()
            || matches!(
                normalized_name.as_str(),
                "connection"
                    | "transfer-encoding"
                    | "upgrade"
                    | "trailer"
                    | "keep-alive"
                    | "proxy-connection"
            )
        {
            return Err(InvocationError::Worker(
                "worker returned an invalid HTTP header".into(),
            ));
        }
        if normalized_name == "content-length" {
            if has_content_length || value.parse::<usize>().ok() != Some(response.body.len()) {
                return Err(InvocationError::Worker(
                    "worker returned an invalid content-length header".into(),
                ));
            }
            has_content_length = true;
        }
    }
    Ok(())
}

async fn prepare_worker(
    function: &DiscoveredFunction,
    project_root: &Path,
    target_directory: &Path,
) -> Result<WorkerCommand, FunError> {
    match function.runtime {
        RuntimeKind::Rust => {
            let output = Command::new("cargo")
                .arg("build")
                .arg("--manifest-path")
                .arg(&function.manifest_path)
                .env("CARGO_TARGET_DIR", target_directory)
                .current_dir(&function.working_directory)
                .output()
                .await
                .map_err(|error| {
                    FunError::Worker(format!(
                        "could not run cargo build for function `{}`: {error}",
                        function.name
                    ))
                })?;
            ensure_build_succeeded(function, output)?;
            stage_executable(
                function,
                project_root,
                executable_path(function, target_directory),
            )
        }
        RuntimeKind::Go1x => {
            let executable = persisted_executable_path(project_root, function);
            if let Some(parent) = executable.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut build = Command::new("go");
            build
                .arg("build")
                .arg("-o")
                .arg(&executable)
                .arg(".")
                .current_dir(&function.working_directory);
            let output = build.output().await.map_err(|error| {
                FunError::Worker(format!(
                    "could not run go build for function `{}` (install Go 1.x or newer): {error}",
                    function.name
                ))
            })?;
            ensure_build_succeeded(function, output)?;
            Ok(WorkerCommand::staged(executable))
        }
        RuntimeKind::NodeJs => Ok(WorkerCommand::new(
            resolve_tool("node", function.runtime)?,
            vec![OsString::from("index.js")],
        )),
        RuntimeKind::Python3 => Ok(WorkerCommand::new(
            resolve_tool("python3", function.runtime)?,
            vec![OsString::from("main.py")],
        )),
        RuntimeKind::Provided => {
            let bootstrap = function.working_directory.join("bootstrap");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&bootstrap)?.permissions().mode();
                if mode & 0o111 == 0 {
                    return Err(FunError::Worker(format!(
                        "provided bootstrap for function `{}` is not executable; run chmod +x {}",
                        function.name,
                        bootstrap.display()
                    )));
                }
            }
            Ok(WorkerCommand::staged(bootstrap))
        }
    }
}

fn resolve_tool(tool: &str, runtime: RuntimeKind) -> Result<OsString, FunError> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(tool);
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if std::fs::metadata(&candidate)?.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return candidate
                .canonicalize()
                .map(|path| path.into_os_string())
                .map_err(|error| {
                    FunError::Worker(format!(
                        "could not resolve `{tool}` for runtime `{runtime}`: {error}"
                    ))
                });
        }
    }
    let installation_hint = match runtime {
        RuntimeKind::NodeJs => "install Node.js and ensure `node` is on PATH",
        RuntimeKind::Python3 => "install Python 3 and ensure `python3` is on PATH",
        _ => "install the required runtime and ensure it is on PATH",
    };
    Err(FunError::Worker(format!(
        "function runtime `{runtime}` requires `{tool}`; {installation_hint}"
    )))
}

fn ensure_build_succeeded(
    function: &DiscoveredFunction,
    output: std::process::Output,
) -> Result<(), FunError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(FunError::Worker(format!(
            "failed to build function `{}` for runtime `{}`:\n{}{}",
            function.name,
            function.runtime,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn stage_executable(
    function: &DiscoveredFunction,
    project_root: &Path,
    built_executable: PathBuf,
) -> Result<WorkerCommand, FunError> {
    let executable = persisted_executable_path(project_root, function);
    if let Some(parent) = executable.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&built_executable, &executable).map_err(|error| {
        FunError::Worker(format!(
            "could not stage function `{}` executable: {error}",
            function.name
        ))
    })?;
    Ok(WorkerCommand::staged(executable))
}

fn executable_path(function: &DiscoveredFunction, target_directory: &Path) -> PathBuf {
    let binary_name = function.package_name.clone();
    #[cfg(windows)]
    let binary_name = format!("{binary_name}.exe");
    target_directory.join("debug").join(binary_name)
}

fn persisted_executable_path(project_root: &Path, function: &DiscoveredFunction) -> PathBuf {
    static NEXT_EXECUTABLE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let binary_name = function.package_name.clone();
    #[cfg(windows)]
    let binary_name = format!("{binary_name}.exe");
    project_root
        .join(".rust-fun")
        .join("bin")
        .join(&function.name)
        .join(format!(
            "{}-{}-{binary_name}",
            std::process::id(),
            NEXT_EXECUTABLE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
}

fn target_directory(project_root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                project_root.join(path)
            }
        })
        .unwrap_or_else(|| project_root.join(".rust-fun/target"))
}

pub fn worker_request(
    method: String,
    path: String,
    query: Vec<(String, String)>,
    path_parameters: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> FunctionRequest {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let request_id = format!(
        "req-{}",
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    FunctionRequest {
        request_id,
        method,
        path,
        query,
        path_parameters,
        headers,
        body,
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
