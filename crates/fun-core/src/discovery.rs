use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use crate::{FunError, ProjectConfig, RuntimeKind};

#[derive(Clone, Debug)]
pub struct DiscoveredFunction {
    pub name: String,
    pub package_name: String,
    pub manifest_path: PathBuf,
    pub working_directory: PathBuf,
    pub runtime: RuntimeKind,
}

#[derive(Clone, Debug)]
pub struct Project {
    pub root: PathBuf,
    pub config: ProjectConfig,
    pub functions: BTreeMap<String, DiscoveredFunction>,
    pub worker_environment: BTreeMap<OsString, OsString>,
}

pub fn discover_project(start: &Path) -> Result<Project, FunError> {
    let config_path = find_config(start)?;
    let root = config_path
        .parent()
        .ok_or_else(|| FunError::Config("project manifest has no parent directory".into()))?
        .canonicalize()?;
    let config_path = root.join("rust-fun.toml");
    if !config_path.canonicalize()?.starts_with(&root) {
        return Err(FunError::Config(
            "rust-fun.toml must resolve inside the project root".into(),
        ));
    }
    let config = ProjectConfig::from_file(&config_path)?;
    let worker_environment = load_worker_environment(&root)?;

    let functions_path = root.join(&config.functions.directory);
    let functions_canonical = functions_path.canonicalize().map_err(|error| {
        FunError::Config(format!(
            "functions directory `{}` is unavailable: {error}",
            functions_path.display()
        ))
    })?;
    if !functions_canonical.starts_with(&root) {
        return Err(FunError::Config(
            "functions.directory must resolve inside the project root".into(),
        ));
    }

    let mut functions = BTreeMap::new();
    for entry in fs::read_dir(&functions_canonical)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() {
            if file_type.is_symlink() {
                return Err(FunError::Config(format!(
                    "function entries may not be symlinks: {}",
                    entry.path().display()
                )));
            }
            continue;
        }
        let function_path = entry.path().canonicalize()?;
        if !function_path.starts_with(&root) {
            return Err(FunError::Config(format!(
                "function directory escapes the project root: {}",
                entry.path().display()
            )));
        }
        reject_escaping_symlinks(&root, &function_path)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        validate_function_name(&name)?;
        let rust_manifest = function_path.join("Cargo.toml");
        let fun_manifest = function_path.join("fun.toml");
        let (manifest_path, runtime) = if fun_manifest.is_file() {
            if rust_manifest.is_file() {
                return Err(FunError::Config(format!(
                    "function `{}` has both fun.toml and Cargo.toml; remove the ambiguous manifest",
                    entry.file_name().to_string_lossy()
                )));
            }
            if !fun_manifest.canonicalize()?.starts_with(&root) {
                return Err(FunError::Config(format!(
                    "function manifest escapes the project root: {}",
                    fun_manifest.display()
                )));
            }
            let manifest: FunctionManifest = toml::from_str(&fs::read_to_string(&fun_manifest)?)
                .map_err(|error| {
                    FunError::Config(format!(
                        "invalid function manifest {}: {error}",
                        fun_manifest.display()
                    ))
                })?;
            if manifest.runtime == RuntimeKind::Rust {
                return Err(FunError::Config(format!(
                    "function `{name}` uses runtime `rust`; Rust functions use Cargo.toml without fun.toml"
                )));
            }
            (fun_manifest, manifest.runtime)
        } else if rust_manifest.is_file() {
            if !rust_manifest.canonicalize()?.starts_with(&root) {
                return Err(FunError::Config(format!(
                    "function manifest escapes the project root: {}",
                    rust_manifest.display()
                )));
            }
            (rust_manifest, RuntimeKind::Rust)
        } else {
            continue;
        };
        let package_name = if runtime == RuntimeKind::Rust {
            let manifest: toml::Value = toml::from_str(&fs::read_to_string(&manifest_path)?)
                .map_err(|error| {
                    FunError::Config(format!(
                        "invalid function manifest {}: {error}",
                        manifest_path.display()
                    ))
                })?;
            manifest
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
                .ok_or_else(|| {
                    FunError::Config(format!(
                        "function manifest {} must declare package.name",
                        manifest_path.display()
                    ))
                })?
                .to_owned()
        } else {
            validate_runtime_files(&function_path, runtime)?;
            name.clone()
        };
        if runtime == RuntimeKind::Rust && !valid_binary_name(&package_name) {
            return Err(FunError::Config(format!(
                "function `{name}` has an invalid Cargo package name `{package_name}`"
            )));
        }
        functions.insert(
            name.clone(),
            DiscoveredFunction {
                name,
                package_name,
                manifest_path,
                working_directory: function_path,
                runtime,
            },
        );
    }

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FunctionManifest {
        runtime: RuntimeKind,
    }

    fn validate_runtime_files(directory: &Path, runtime: RuntimeKind) -> Result<(), FunError> {
        let required = match runtime {
            RuntimeKind::Rust => return Ok(()),
            RuntimeKind::NodeJs => "index.js",
            RuntimeKind::Python3 => "main.py",
            RuntimeKind::Go1x => "go.mod",
            RuntimeKind::Provided => "bootstrap",
        };
        if !directory.join(required).is_file() {
            return Err(FunError::Config(format!(
                "function `{}` uses runtime `{runtime}` but is missing `{required}`",
                directory.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
        if runtime == RuntimeKind::Go1x && !directory.join("main.go").is_file() {
            return Err(FunError::Config(format!(
                "function `{}` uses runtime `go1.x` but is missing `main.go`",
                directory.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
        Ok(())
    }

    fn reject_escaping_symlinks(root: &Path, directory: &Path) -> Result<(), FunError> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                let target = entry.path().canonicalize().map_err(|error| {
                    FunError::Config(format!(
                        "could not resolve function symlink {}: {error}",
                        entry.path().display()
                    ))
                })?;
                if !target.starts_with(root) {
                    return Err(FunError::Config(format!(
                        "function symlink escapes the project root: {}",
                        entry.path().display()
                    )));
                }
            } else if file_type.is_dir() {
                reject_escaping_symlinks(root, &entry.path())?;
            }
        }
        Ok(())
    }

    for route in &config.routes {
        if !functions.contains_key(&route.function) {
            return Err(FunError::Config(format!(
                "route `{}` refers to undiscovered function `{}`",
                route.path, route.function
            )));
        }
    }

    Ok(Project {
        root,
        config,
        functions,
        worker_environment,
    })
}

pub fn load_worker_environment(root: &Path) -> Result<BTreeMap<OsString, OsString>, FunError> {
    let env_path = root.join(".env");
    if !env_path.exists() {
        return Ok(BTreeMap::new());
    }
    let canonical_env_path = env_path
        .canonicalize()
        .map_err(|error| FunError::Config(format!("could not resolve .env file: {error}")))?;
    let canonical_root = root.canonicalize()?;
    if !canonical_env_path.starts_with(&canonical_root) || !canonical_env_path.is_file() {
        return Err(FunError::Config(
            ".env must be a regular file inside the project root".into(),
        ));
    }
    let entries = dotenvy::from_path_iter(&canonical_env_path)
        .map_err(|error| FunError::Config(format!("could not read .env: {error}")))?;
    let mut variables = BTreeMap::new();
    for entry in entries {
        let (key, value) =
            entry.map_err(|error| FunError::Config(format!("invalid .env entry: {error}")))?;
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return Err(FunError::Config(format!(
                "invalid environment variable name `{key}` in .env"
            )));
        }
        variables.insert(OsString::from(key), OsString::from(value));
    }
    Ok(variables)
}

fn find_config(start: &Path) -> Result<PathBuf, FunError> {
    let mut current = if start.is_file() {
        start
            .parent()
            .ok_or_else(|| FunError::Config("cannot determine starting directory".into()))?
            .to_path_buf()
    } else {
        start.to_path_buf()
    };
    current = current.canonicalize()?;
    loop {
        let candidate = current.join("rust-fun.toml");
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !current.pop() {
            return Err(FunError::Config(
                "could not find rust-fun.toml in this directory or its parents".into(),
            ));
        }
    }
}

fn validate_function_name(name: &str) -> Result<(), FunError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(FunError::Config(format!(
            "invalid function directory name `{name}`"
        )));
    }
    Ok(())
}

fn valid_binary_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

pub fn is_within(root: &Path, candidate: &Path) -> bool {
    candidate.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("fun-core-{name}-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn create_minimal_project(root: &Path, functions_directory: &str) {
        fs::write(
            root.join("rust-fun.toml"),
            format!("[functions]\ndirectory = \"{functions_directory}\"\n"),
        )
        .unwrap();
        fs::create_dir_all(root.join("functions/hello/src")).unwrap();
        fs::write(
            root.join("functions/hello/Cargo.toml"),
            "[package]\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(root.join("functions/hello/src/main.rs"), "fn main() {}").unwrap();
    }

    #[test]
    fn discovers_function_and_manifest_route() {
        let temp = TempDirectory::new("discovery");
        create_minimal_project(&temp.0, "functions");
        fs::write(
            temp.0.join("rust-fun.toml"),
            "[functions]\ndirectory = \"functions\"\n[[routes]]\npath = \"/hello\"\nfunction = \"hello\"\n",
        )
        .unwrap();
        let project = discover_project(&temp.0).unwrap();
        assert!(project.functions.contains_key("hello"));
    }

    #[test]
    fn discovers_each_non_rust_runtime_alongside_legacy_rust() {
        let temp = TempDirectory::new("runtime-discovery");
        create_minimal_project(&temp.0, "functions");
        let functions = temp.0.join("functions");
        for (name, runtime, files) in [
            ("node", "nodejs", &["index.js"][..]),
            ("python", "python3", &["main.py"][..]),
            ("go", "go1.x", &["go.mod", "main.go"][..]),
            ("custom", "provided", &["bootstrap"][..]),
        ] {
            let directory = functions.join(name);
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join("fun.toml"),
                format!("runtime = \"{runtime}\"\n"),
            )
            .unwrap();
            for file in files {
                fs::write(directory.join(file), "fixture").unwrap();
            }
        }

        let project = discover_project(&temp.0).unwrap();
        assert_eq!(project.functions["hello"].runtime, RuntimeKind::Rust);
        assert_eq!(project.functions["node"].runtime, RuntimeKind::NodeJs);
        assert_eq!(project.functions["python"].runtime, RuntimeKind::Python3);
        assert_eq!(project.functions["go"].runtime, RuntimeKind::Go1x);
        assert_eq!(project.functions["custom"].runtime, RuntimeKind::Provided);
    }

    #[test]
    fn rejects_ambiguous_and_incomplete_runtime_manifests() {
        let temp = TempDirectory::new("runtime-invalid");
        create_minimal_project(&temp.0, "functions");
        let node = temp.0.join("functions/node");
        fs::create_dir_all(&node).unwrap();
        fs::write(node.join("Cargo.toml"), "[package]\nname='node'\n").unwrap();
        fs::write(node.join("fun.toml"), "runtime='nodejs'\n").unwrap();
        assert!(discover_project(&temp.0).is_err());

        fs::remove_dir_all(node.join("Cargo.toml").parent().unwrap()).unwrap();
        fs::create_dir_all(&node).unwrap();
        fs::write(node.join("fun.toml"), "runtime='unsupported'\n").unwrap();
        assert!(discover_project(&temp.0).is_err());
    }

    #[test]
    fn rejects_functions_directory_escape() {
        let temp = TempDirectory::new("escape");
        create_minimal_project(&temp.0, "../");
        assert!(discover_project(&temp.0).is_err());
    }

    #[test]
    fn reads_env_file_without_mutating_the_parent_environment() {
        let temp = TempDirectory::new("env");
        create_minimal_project(&temp.0, "functions");
        fs::write(temp.0.join(".env"), "GREETING='Welcome home'\n").unwrap();
        let parent_greeting = std::env::var_os("GREETING");
        let project = discover_project(&temp.0).unwrap();
        assert_eq!(
            project
                .worker_environment
                .get(std::ffi::OsStr::new("GREETING")),
            Some(&OsString::from("Welcome home"))
        );
        assert_eq!(std::env::var_os("GREETING"), parent_greeting);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_env_file_symlink_outside_project() {
        use std::os::unix::fs::symlink;

        let temp = TempDirectory::new("env-symlink");
        let project_root = temp.0.join("project");
        fs::create_dir_all(&project_root).unwrap();
        create_minimal_project(&project_root, "functions");
        let external = temp.0.join("secret.env");
        fs::write(&external, "TOKEN=secret\n").unwrap();
        symlink(&external, project_root.join(".env")).unwrap();
        assert!(discover_project(&project_root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_function_source_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = TempDirectory::new("symlink");
        let project_root = temp.0.join("project");
        fs::create_dir_all(&project_root).unwrap();
        create_minimal_project(&project_root, "functions");
        let external = temp.0.join("outside.rs");
        fs::write(&external, "fn main() {}").unwrap();
        let source = project_root.join("functions/hello/src/external.rs");
        symlink(&external, &source).unwrap();
        assert!(discover_project(&project_root).is_err());
    }
}
