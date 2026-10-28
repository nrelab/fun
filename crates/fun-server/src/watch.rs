use std::{
    collections::HashSet, future::Future, hash::Hash, path::Path, sync::Arc, time::Duration,
};

use fun_core::{discover_project, load_worker_environment, FunError, Project};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::{sync::mpsc, time::timeout};
use tracing::{error, info, warn};

use crate::WorkerPool;

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(300);

#[derive(Eq, Hash, PartialEq)]
enum Change {
    Function(String),
    Environment,
}

pub fn watch_project(
    project: Project,
    pool: Arc<WorkerPool>,
) -> Result<impl Future<Output = Result<(), FunError>> + Send, FunError> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })
    .map_err(|error| FunError::Worker(format!("could not start file watcher: {error}")))?;

    let functions_root = project.root.join(&project.config.functions.directory);
    watcher
        .watch(&functions_root, RecursiveMode::Recursive)
        .map_err(|error| {
            FunError::Worker(format!(
                "could not watch functions directory {}: {error}",
                functions_root.display()
            ))
        })?;
    watcher
        .watch(&project.root, RecursiveMode::NonRecursive)
        .map_err(|error| {
            FunError::Worker(format!(
                "could not watch project directory {}: {error}",
                project.root.display()
            ))
        })?;

    info!(path = %functions_root.display(), "watching function source files");
    Ok(async move {
        let _watcher = watcher;
        let mut receiver = receiver;
        loop {
            let event = receiver
                .recv()
                .await
                .ok_or_else(|| FunError::Worker("file watcher stopped unexpectedly".into()))?;
            let mut pending = collect_changes(event, &project);
            loop {
                match timeout(DEBOUNCE_WINDOW, receiver.recv()).await {
                    Ok(Some(Ok(event))) => pending.extend(collect_changes(Ok(event), &project)),
                    Ok(Some(Err(error))) => warn!(%error, "file watcher reported an error"),
                    Ok(None) => {
                        return Err(FunError::Worker("file watcher stopped unexpectedly".into()));
                    }
                    Err(_) => break,
                }
            }

            if pending.contains(&Change::Environment) {
                match load_worker_environment(&project.root) {
                    Ok(environment) => match pool
                        .reload_environment(environment.into_iter().collect())
                        .await
                    {
                        Ok(()) => info!("reloaded worker environment from .env"),
                        Err(error) => error!(%error, "could not reload worker environment"),
                    },
                    Err(error) => error!(%error, "could not reload .env"),
                }
            }
            let mut functions: Vec<_> = pending
                .into_iter()
                .filter_map(|change| match change {
                    Change::Function(name) => Some(name),
                    Change::Environment => None,
                })
                .collect();
            functions.sort();
            for function_name in functions {
                match discover_project(&project.root) {
                    Ok(updated_project) => {
                        match pool.reload_function(&updated_project, &function_name).await {
                            Ok(()) => info!(function = %function_name, "function reloaded"),
                            Err(error) => {
                                error!(function = %function_name, %error, "function rebuild failed; keeping last working worker")
                            }
                        }
                    }
                    Err(error) => {
                        error!(%error, "project validation failed; keeping last working worker")
                    }
                }
            }
        }
    })
}

fn collect_changes(event: notify::Result<Event>, project: &Project) -> HashSet<Change> {
    let mut changes = HashSet::new();
    let event = match event {
        Ok(event) => event,
        Err(error) => {
            warn!(%error, "file watcher reported an error");
            return changes;
        }
    };
    if matches!(event.kind, EventKind::Access(_)) {
        return changes;
    }
    for path in event.paths {
        if is_env_path(&project.root, &path) {
            changes.insert(Change::Environment);
            continue;
        }
        if path
            .components()
            .any(|component| component.as_os_str() == ".rust-fun")
        {
            continue;
        }
        if let Some((name, _)) = project
            .functions
            .iter()
            .find(|(_, function)| path.starts_with(&function.working_directory))
        {
            changes.insert(Change::Function(name.clone()));
        }
    }
    changes
}

fn is_env_path(root: &Path, path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == ".env")
        && path.parent().is_some_and(|parent| parent == root)
}
