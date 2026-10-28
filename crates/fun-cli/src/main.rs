use std::{
    error::Error,
    fs,
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use fun_core::{discover_project, FunctionRequest, FunctionResponse, Project, RuntimeKind};
use fun_server::{serve, InvocationError, ServerOptions, WorkerPool};

#[derive(Debug, Parser)]
#[command(name = "fun", version, about = "Local-first Rust function runtime")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create a sample project for a supported runtime.
    Init {
        #[arg(long, default_value = "rust")]
        runtime: RuntimeKind,
        #[arg(default_value = "my-functions")]
        path: PathBuf,
    },
    /// Build functions and serve them over local HTTP.
    Dev {
        #[arg(long)]
        host: Option<IpAddr>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long, conflicts_with = "no_watch")]
        watch: bool,
        #[arg(long, conflicts_with = "watch")]
        no_watch: bool,
    },
    /// List discovered functions and routes.
    List,
    /// Run each discovered function's runtime-specific tests.
    Test,
    /// Invoke a function directly.
    Invoke {
        function: String,
        #[arg(long)]
        json: Option<String>,
        #[arg(long, default_value = "POST")]
        method: String,
        #[arg(long, default_value = "/")]
        path: String,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    match cli.command {
        Commands::Init { path, runtime } => init_project(&path, runtime)?,
        Commands::Dev {
            host,
            port,
            watch,
            no_watch,
        } => {
            let project = load_project()?;
            let configured_host = project.config.dev.host.parse::<IpAddr>()?;
            let options = ServerOptions {
                host: host.unwrap_or(configured_host),
                port: port.unwrap_or(project.config.dev.port),
                watch: if watch {
                    Some(true)
                } else if no_watch {
                    Some(false)
                } else {
                    None
                },
            };
            if !options.host.is_loopback() {
                return Err("only loopback addresses are allowed for `fun dev`".into());
            }
            if options.port == 0 {
                return Err("port must be between 1 and 65535".into());
            }
            serve(project, options).await?;
        }
        Commands::List => list_functions(&load_project()?),
        Commands::Test => test_functions(&load_project()?).await?,
        Commands::Invoke {
            function,
            json,
            method,
            path,
        } => {
            let project = load_project()?;
            invoke(&project, &function, json, method, path).await?;
        }
    }
    Ok(())
}

fn load_project() -> Result<Project, Box<dyn Error>> {
    Ok(discover_project(&std::env::current_dir()?)?)
}

fn init_project(path: &Path, runtime: RuntimeKind) -> Result<(), Box<dyn Error>> {
    let path = if path.exists() {
        if !path.is_dir() {
            return Err(format!("{} exists and is not a directory", path.display()).into());
        }
        if fs::read_dir(path)?.next().is_some() {
            return Err(format!(
                "refusing to overwrite non-empty directory {}",
                path.display()
            )
            .into());
        }
        path.to_path_buf()
    } else {
        fs::create_dir_all(path)?;
        path.to_path_buf()
    };

    let mut files = vec![
        ("rust-fun.toml".to_owned(), project_manifest().to_owned()),
        (".env.example".to_owned(), "GREETING=Hello\n".to_owned()),
        (
            ".gitignore".to_owned(),
            ".env\n.rust-fun/\ntarget/\n".to_owned(),
        ),
    ];
    let executable = if runtime == RuntimeKind::Rust {
        files.push((
            "functions/hello/Cargo.toml".into(),
            worker_manifest().into(),
        ));
        files.push(("functions/hello/src/main.rs".into(), worker_source().into()));
        None
    } else {
        files.push((
            "functions/hello/fun.toml".into(),
            format!("runtime = \"{runtime}\"\n"),
        ));
        let (relative, contents) = match runtime {
            RuntimeKind::Rust => unreachable!(),
            RuntimeKind::NodeJs => (
                "functions/hello/index.js",
                include_str!("templates/nodejs/index.js"),
            ),
            RuntimeKind::Python3 => (
                "functions/hello/main.py",
                include_str!("templates/python3/main.py"),
            ),
            RuntimeKind::Go1x => {
                files.push((
                    "functions/hello/go.mod".into(),
                    include_str!("templates/go1x/go.mod").into(),
                ));
                (
                    "functions/hello/main.go",
                    include_str!("templates/go1x/main.go"),
                )
            }
            RuntimeKind::Provided => (
                "functions/hello/bootstrap",
                include_str!("templates/provided/bootstrap"),
            ),
        };
        files.push((relative.into(), contents.into()));
        if runtime == RuntimeKind::Python3 {
            files.push((
                "functions/hello/test_main.py".into(),
                include_str!("templates/python3/test_main.py").into(),
            ));
        }
        (runtime == RuntimeKind::Provided).then_some("functions/hello/bootstrap")
    };
    for (relative, _) in &files {
        if path.join(relative).exists() {
            return Err(format!(
                "refusing to overwrite existing file {}",
                path.join(relative).display()
            )
            .into());
        }
    }
    for (relative, contents) in files {
        let destination = path.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        file.write_all(contents.as_bytes())?;
    }
    if let Some(executable) = executable {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let executable = path.join(executable);
            let mut permissions = fs::metadata(&executable)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(executable, permissions)?;
        }
    }
    println!(
        "Created Rust Fun project at {} (runtime: {runtime})",
        path.display()
    );
    Ok(())
}

fn project_manifest() -> &'static str {
    r#"[project]
name = "my-functions"
version = "0.1.0"

[dev]
host = "127.0.0.1"
port = 3000
watch = true

[functions]
directory = "functions"
default_timeout_ms = 5000
max_concurrency = 64
max_request_body_bytes = 1048576
max_response_body_bytes = 1048576

[[routes]]
path = "/api/hello"
function = "hello"
methods = ["POST"]
"#
}

fn worker_manifest() -> &'static str {
    include_str!("templates/rust-http/Cargo.toml.template")
}

fn worker_source() -> &'static str {
    include_str!("templates/rust-http/src/main.rs")
}

fn list_functions(project: &Project) {
    for function in project.functions.values() {
        let routes: Vec<_> = project
            .config
            .routes
            .iter()
            .filter(|route| route.function == function.name)
            .map(|route| {
                let methods = if route.methods.is_empty() {
                    "*".to_owned()
                } else {
                    route.methods.join(",")
                };
                format!("{methods} {}", route.path)
            })
            .collect();
        if routes.is_empty() {
            println!("{}\t{}\t(no routes)", function.name, function.runtime);
        } else {
            println!(
                "{}\t{}\t{}",
                function.name,
                function.runtime,
                routes.join("; ")
            );
        }
    }
}

async fn test_functions(project: &Project) -> Result<(), Box<dyn Error>> {
    for function in project.functions.values() {
        if function.runtime == RuntimeKind::Provided {
            println!(
                "Skipping function `{}` (runtime `provided` has no standard test command)",
                function.name
            );
            continue;
        }
        println!("Testing function `{}`", function.name);
        let mut command = match function.runtime {
            RuntimeKind::Rust => {
                let mut command = tokio::process::Command::new("cargo");
                command
                    .arg("test")
                    .arg("--manifest-path")
                    .arg(&function.manifest_path)
                    .env("CARGO_TARGET_DIR", target_directory(&project.root));
                command
            }
            RuntimeKind::NodeJs => {
                let mut command = tokio::process::Command::new("node");
                command.arg("--test");
                command
            }
            RuntimeKind::Python3 => {
                let mut command = tokio::process::Command::new("python3");
                command.args(["-m", "unittest", "discover"]);
                command
            }
            RuntimeKind::Go1x => {
                let mut command = tokio::process::Command::new("go");
                command.args(["test", "./..."]);
                command
            }
            RuntimeKind::Provided => unreachable!(),
        };
        let output = command
            .current_dir(&function.working_directory)
            .output()
            .await
            .map_err(|error| {
                format!(
                    "could not run tests for function `{}` (runtime `{}`; install its local toolchain): {error}",
                    function.name, function.runtime
                )
            })?;
        if !output.status.success() {
            return Err(format!(
                "tests failed for function `{}`:\n{}{}",
                function.name,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        print!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
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

async fn invoke(
    project: &Project,
    function: &str,
    json: Option<String>,
    method: String,
    path: String,
) -> Result<(), Box<dyn Error>> {
    if !project.functions.contains_key(function) {
        return Err(format!("function `{function}` was not found").into());
    }
    let body = match json {
        Some(value) => {
            let parsed: serde_json::Value = serde_json::from_str(&value)?;
            serde_json::to_vec(&parsed)?
        }
        None => Vec::new(),
    };
    let pool = WorkerPool::build_and_start_function(project, function).await?;
    let request = FunctionRequest {
        request_id: "cli-1".into(),
        method,
        path,
        query: Vec::new(),
        path_parameters: Vec::new(),
        headers: Vec::new(),
        body,
    };
    let response = pool.invoke(function, request).await;
    pool.shutdown().await;
    let response: FunctionResponse = response.map_err(invocation_error)?;
    match serde_json::from_slice::<serde_json::Value>(&response.body) {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
        Err(_) => println!("{}", String::from_utf8_lossy(&response.body)),
    }
    Ok(())
}

fn invocation_error(error: InvocationError) -> Box<dyn Error> {
    let message = match error {
        InvocationError::Busy => "runtime is busy".to_owned(),
        InvocationError::Timeout => "function timed out".to_owned(),
        InvocationError::RequestTooLarge => "request body exceeds configured limit".to_owned(),
        InvocationError::Worker(message) => format!("function worker failed: {message}"),
        InvocationError::Function(error) => format!("function failed: {}", error.message),
        InvocationError::NotFound => "function was not found".to_owned(),
    };
    message.into()
}
