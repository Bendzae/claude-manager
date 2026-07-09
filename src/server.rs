use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{self, Config};
use crate::tmux::{self, DiffStats, SessionStatus, TmuxSession};
use crate::worker::{TaskInfo, Worker, WorkerUpdate};

struct ServerState {
    worker: Worker,
    hostname: String,
    /// Last snapshot, served while the worker has no fresh update
    /// (e.g. two requests within one worker tick).
    last_state: Mutex<Option<Value>>,
}

type ApiError = (StatusCode, String);

fn internal(e: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    (StatusCode::BAD_REQUEST, msg.into())
}

pub fn run(bind: &str) -> Result<()> {
    let state = Arc::new(ServerState {
        worker: Worker::spawn(),
        hostname: crate::app::detect_hostname(),
        last_state: Mutex::new(None),
    });

    if let Ok(cfg) = Config::load() {
        sync_hints(&state.worker, &cfg);
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/manifest.json", get(manifest))
        .route("/icon.png", get(icon))
        .route("/api/state", get(api_state))
        .route("/api/sessions/{name}/output", get(api_output))
        .route("/api/sessions/{name}/send", post(api_send))
        .route("/api/sessions/{name}/keys", post(api_keys))
        .route("/api/sessions/{name}/kill", post(api_kill))
        .route("/api/tasks", post(api_create_task))
        .route("/api/sessions", post(api_create_session))
        .with_state(state);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(bind).await?;
        let port = listener.local_addr()?.port();
        println!("claude-manager serving on http://{bind}");
        println!("Expose over your tailnet with: tailscale serve --bg {port}");
        axum::serve(listener, app).await?;
        Ok(())
    })
}

/// Mirror of `App::sync_worker_hints` for the headless server.
fn sync_hints(worker: &Worker, cfg: &Config) {
    let tasks: Vec<TaskInfo> = cfg
        .projects
        .iter()
        .flat_map(|p| {
            p.tasks.iter().map(|t| TaskInfo {
                project_name: p.name.clone(),
                project_path: p.path.clone(),
                branch: t.branch.clone(),
                base_branch: t.base_branch().to_string(),
                stacked: t.stacked,
            })
        })
        .collect();

    let project_paths: Vec<(String, String)> = cfg
        .projects
        .iter()
        .map(|p| (p.name.clone(), p.path.clone()))
        .collect();

    if let Ok(mut hints) = worker.hints.lock() {
        hints.tasks = tasks;
        hints.project_paths = project_paths;
    }
}

fn status_str(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Running => "running",
        SessionStatus::WaitingForInput => "waiting_input",
        SessionStatus::WaitingForPermission => "waiting_permission",
        SessionStatus::Finished => "finished",
    }
}

fn diff_json(d: &DiffStats) -> Value {
    json!({ "added": d.added, "removed": d.removed })
}

fn session_json(s: &TmuxSession, u: &WorkerUpdate) -> Value {
    json!({
        "tmux_name": s.name,
        "name": s.session_name,
        "status": u.statuses.get(&s.name).map(|st| status_str(*st)),
        "branch": u.session_branches.get(&s.name),
        "diff": u.diff_stats.get(&s.name).map(diff_json),
    })
}

fn build_state(cfg: &Config, u: &WorkerUpdate, hostname: &str) -> Value {
    let projects: Vec<Value> = cfg
        .projects
        .iter()
        .map(|p| {
            let tasks: Vec<Value> = p
                .tasks
                .iter()
                .map(|t| {
                    let sessions: Vec<Value> =
                        tmux::sessions_for_task(&p.name, &t.name, &u.sessions)
                            .iter()
                            .map(|s| session_json(s, u))
                            .collect();
                    json!({
                        "name": t.name,
                        "branch": t.branch,
                        "base_branch": t.base_branch(),
                        "archived": t.archived,
                        "stacked": t.stacked,
                        "pr_url": u.pr_urls.get(&t.branch),
                        "diff": u.task_diff_stats.get(&t.branch).map(diff_json),
                        "sessions": sessions,
                    })
                })
                .collect();
            let adhoc: Vec<Value> = tmux::adhoc_sessions_for_project(&p.name, &u.sessions)
                .iter()
                .map(|s| session_json(s, u))
                .collect();
            json!({
                "name": p.name,
                "path": p.path,
                "branch": u.project_branches.get(&p.name),
                "tasks": tasks,
                "adhoc_sessions": adhoc,
            })
        })
        .collect();

    json!({ "host": hostname, "projects": projects })
}

async fn api_state(State(state): State<Arc<ServerState>>) -> Result<Response, ApiError> {
    let snapshot = tokio::task::spawn_blocking(move || {
        let cfg = Config::load().map_err(internal)?;
        sync_hints(&state.worker, &cfg);

        let update = state.worker.latest.lock().unwrap().take();
        let value = match update {
            Some(u) => build_state(&cfg, &u, &state.hostname),
            None => match state.last_state.lock().unwrap().clone() {
                Some(v) => v,
                // First request racing the worker's first tick: list sessions
                // inline so the tree renders immediately (statuses fill in on
                // the next poll).
                None => {
                    let u = WorkerUpdate {
                        sessions: tmux::list_sessions().unwrap_or_default(),
                        statuses: Default::default(),
                        diff_stats: Default::default(),
                        task_diff_stats: Default::default(),
                        session_branches: Default::default(),
                        pr_urls: Default::default(),
                        stack_prs: Default::default(),
                        project_branches: Default::default(),
                        run_sessions: Default::default(),
                    };
                    build_state(&cfg, &u, &state.hostname)
                }
            },
        };
        *state.last_state.lock().unwrap() = Some(value.clone());
        Ok::<_, ApiError>(value)
    })
    .await
    .map_err(internal)??;

    Ok(axum::Json(snapshot).into_response())
}

/// Reject session names that don't look like claude-manager tmux sessions so
/// the API can't be used to poke at arbitrary tmux targets.
fn validate_session_name(name: &str) -> Result<(), ApiError> {
    if name.starts_with("cm") && !name.contains(':') {
        Ok(())
    } else {
        Err(bad_request("not a claude-manager session"))
    }
}

#[derive(Deserialize)]
struct OutputParams {
    lines: Option<usize>,
}

async fn api_output(
    Path(name): Path<String>,
    Query(params): Query<OutputParams>,
) -> Result<Response, ApiError> {
    validate_session_name(&name)?;
    let lines = params.lines.unwrap_or(300).clamp(50, 5000);

    let text = tokio::task::spawn_blocking(move || tmux::capture_output(&name, lines))
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "session not found".to_string()))?;

    Ok(axum::Json(json!({ "text": text })).into_response())
}

fn default_submit() -> bool {
    true
}

#[derive(Deserialize)]
struct SendBody {
    text: String,
    #[serde(default = "default_submit")]
    submit: bool,
}

async fn api_send(
    Path(name): Path<String>,
    axum::Json(body): axum::Json<SendBody>,
) -> Result<StatusCode, ApiError> {
    validate_session_name(&name)?;

    tokio::task::spawn_blocking(move || {
        if body.text.is_empty() {
            if body.submit {
                tmux::send_key(&name, "Enter")
            } else {
                Ok(())
            }
        } else {
            tmux::send_text(&name, &body.text, body.submit)
        }
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct KeyBody {
    key: String,
}

async fn api_keys(
    Path(name): Path<String>,
    axum::Json(body): axum::Json<KeyBody>,
) -> Result<StatusCode, ApiError> {
    validate_session_name(&name)?;

    const NAMED_KEYS: &[&str] = &[
        "Enter", "Escape", "Up", "Down", "Left", "Right", "Tab", "BSpace",
    ];
    let allowed = NAMED_KEYS.contains(&body.key.as_str())
        || (body.key.chars().count() == 1 && body.key.chars().all(|c| c.is_ascii_alphanumeric()));
    if !allowed {
        return Err(bad_request(format!("key '{}' not allowed", body.key)));
    }

    tokio::task::spawn_blocking(move || tmux::send_key(&name, &body.key))
        .await
        .map_err(internal)?
        .map_err(internal)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn api_kill(Path(name): Path<String>) -> Result<StatusCode, ApiError> {
    validate_session_name(&name)?;

    tokio::task::spawn_blocking(move || {
        let fallback = config::load_sessions()
            .get(&name)
            .filter(|r| r.use_worktree)
            .map(|r| tmux::SessionCleanupInfo {
                project_path: r.project_path.clone(),
                worktree_path: tmux::worktree_dir(&r.project_name, &r.task_name, &r.session_name)
                    .to_string_lossy()
                    .to_string(),
                branch_name: None,
            });
        tmux::kill_session_with_fallback(&name, fallback)?;
        config::remove_session_record(&name);
        Ok::<_, anyhow::Error>(())
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct CreateTaskBody {
    project: String,
    name: String,
    prompt: Option<String>,
}

async fn api_create_task(
    axum::Json(body): axum::Json<CreateTaskBody>,
) -> Result<Response, ApiError> {
    let tmux_name = tokio::task::spawn_blocking(move || {
        let cfg = Config::load()?;
        let project = cfg
            .projects
            .iter()
            .find(|p| p.name == body.project)
            .ok_or_else(|| anyhow::anyhow!("project '{}' not found", body.project))?
            .clone();

        let task_name = body.name.trim().to_string();
        let branch = tmux::to_branch_name(&task_name);
        if branch.is_empty() {
            anyhow::bail!("task name produces an empty branch name");
        }

        if !tmux::branch_exists(&project.path, &branch) {
            tmux::create_task_branch(&project.path, &branch)?;
        }

        let task_name_for_modify = task_name.clone();
        let branch_for_modify = branch.clone();
        let project_name_for_modify = project.name.clone();
        Config::modify(move |c| {
            c.add_task(
                &project_name_for_modify,
                task_name_for_modify,
                branch_for_modify,
            );
        })?;

        create_session_for_task(
            &cfg,
            &project,
            &task_name,
            &branch,
            body.prompt.as_deref().filter(|p| !p.trim().is_empty()),
        )
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;

    Ok(axum::Json(json!({ "tmux_name": tmux_name })).into_response())
}

#[derive(Deserialize)]
struct CreateSessionBody {
    project: String,
    task: String,
    prompt: Option<String>,
}

async fn api_create_session(
    axum::Json(body): axum::Json<CreateSessionBody>,
) -> Result<Response, ApiError> {
    let tmux_name = tokio::task::spawn_blocking(move || {
        let cfg = Config::load()?;
        let project = cfg
            .projects
            .iter()
            .find(|p| p.name == body.project)
            .ok_or_else(|| anyhow::anyhow!("project '{}' not found", body.project))?
            .clone();
        let task = project
            .tasks
            .iter()
            .find(|t| t.name == body.task)
            .ok_or_else(|| anyhow::anyhow!("task '{}' not found", body.task))?
            .clone();

        create_session_for_task(
            &cfg,
            &project,
            &task.name,
            &task.branch,
            body.prompt.as_deref().filter(|p| !p.trim().is_empty()),
        )
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;

    Ok(axum::Json(json!({ "tmux_name": tmux_name })).into_response())
}

fn create_session_for_task(
    cfg: &Config,
    project: &config::Project,
    task_name: &str,
    branch: &str,
    prompt: Option<&str>,
) -> Result<String> {
    let sessions = tmux::list_sessions()?;
    let session_name = tmux::next_session_number(&project.name, task_name, &sessions).to_string();

    let tmux_name = tmux::create_session(
        &project.name,
        &project.path,
        task_name,
        branch,
        &session_name,
        true,
        &project.copy_patterns,
        &project.setup_commands,
        prompt,
        &cfg.startup_skills,
    )?;

    config::add_session_record(
        &tmux_name,
        config::SessionRecord {
            project_name: project.name.clone(),
            project_path: project.path.clone(),
            task_name: task_name.to_string(),
            task_branch: branch.to_string(),
            session_name,
            use_worktree: true,
            archived: false,
        },
    );

    Ok(tmux_name)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("web/index.html"))
}

async fn app_js() -> Response {
    static_file("application/javascript", include_str!("web/app.js"))
}

async fn style_css() -> Response {
    static_file("text/css", include_str!("web/style.css"))
}

async fn manifest() -> Response {
    static_file("application/json", include_str!("web/manifest.json"))
}

async fn icon() -> Response {
    (
        [(header::CONTENT_TYPE, "image/png")],
        include_bytes!("web/icon.png").as_slice(),
    )
        .into_response()
}

fn static_file(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}
