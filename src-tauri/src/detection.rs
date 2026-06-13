use crate::models::{
    BinaryCandidate, ConfigCandidate, DetectionReport, DetectionResolution, DetectionSource,
    ResolutionKind, ToolId, ToolSetup, ValidationEvidence,
};
use crate::store::Store;
use crate::tools::{command_name, common_bin_dirs, default_config_dir, home_dir, is_our_launcher};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn detect_tool_setup(tool_id: &ToolId, store: &Store) -> DetectionReport {
    let mut config_candidates = detect_config_candidates(tool_id, store);
    config_candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.path.to_string_lossy().cmp(&b.path.to_string_lossy()))
    });

    let mut binary_candidates = detect_binary_candidates(tool_id);
    binary_candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.path.to_string_lossy().cmp(&b.path.to_string_lossy()))
    });

    let resolution = resolve_detection(tool_id, &config_candidates, &binary_candidates);
    DetectionReport {
        tool_id: tool_id.clone(),
        config_candidates,
        binary_candidates,
        resolution,
    }
}

pub fn validate_config_dir(tool_id: &ToolId, store: &Store, path: &Path) -> ConfigCandidate {
    let app_root = store.tool_accounts_root(tool_id);
    let is_app_managed = path.starts_with(&app_root);
    let source = if is_env_config(tool_id, path) {
        DetectionSource::Env
    } else if path == default_config_dir(tool_id) {
        DetectionSource::Default
    } else if is_app_managed {
        DetectionSource::AppManaged
    } else {
        DetectionSource::Manual
    };
    config_candidate(tool_id, path.to_path_buf(), source, is_app_managed)
}

pub fn validate_binary_path(tool_id: &ToolId, path: &Path) -> BinaryCandidate {
    let source = if path_in_path_env(path) {
        DetectionSource::Path
    } else {
        DetectionSource::Manual
    };
    binary_candidate(tool_id, path.to_path_buf(), source)
}

pub fn setup_from_manual(
    tool_id: &ToolId,
    store: &Store,
    binary_path: PathBuf,
    default_config_dir: PathBuf,
) -> (ToolSetup, Vec<String>) {
    let config = validate_config_dir(tool_id, store, &default_config_dir);
    let binary = validate_binary_path(tool_id, &binary_path);
    let mut warnings = Vec::new();
    warnings.extend(config.warnings.clone());
    warnings.extend(binary.warnings.clone());
    if !config.valid {
        warnings.push("Config path validation is weak".to_string());
    }
    if !binary.valid {
        warnings.push("Binary path validation is weak".to_string());
    }
    (
        ToolSetup {
            binary_path: Some(binary_path),
            default_config_dir: Some(default_config_dir),
            binary_source: DetectionSource::Manual,
            config_source: DetectionSource::Manual,
            validated_at: Some(chrono::Utc::now().to_rfc3339()),
            validation_warnings: warnings.clone(),
        },
        warnings,
    )
}

fn detect_config_candidates(tool_id: &ToolId, store: &Store) -> Vec<ConfigCandidate> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let app_root = store.tool_accounts_root(tool_id);

    if let Some(path) = env_config_dir(tool_id) {
        let is_app_managed = path.starts_with(&app_root);
        if !is_app_managed {
            push_config_candidate(
                &mut out,
                &mut seen,
                tool_id,
                path,
                DetectionSource::Env,
                false,
            );
        }
    }

    push_config_candidate(
        &mut out,
        &mut seen,
        tool_id,
        default_config_dir(tool_id),
        DetectionSource::Default,
        false,
    );

    out
}

fn push_config_candidate(
    out: &mut Vec<ConfigCandidate>,
    seen: &mut BTreeSet<String>,
    tool_id: &ToolId,
    path: PathBuf,
    source: DetectionSource,
    is_app_managed: bool,
) {
    let key = path.to_string_lossy().to_string();
    if seen.insert(key) {
        out.push(config_candidate(tool_id, path, source, is_app_managed));
    }
}

fn config_candidate(
    tool_id: &ToolId,
    path: PathBuf,
    source: DetectionSource,
    is_app_managed: bool,
) -> ConfigCandidate {
    let mut evidence = Vec::new();
    let mut score = source_score(&source);

    add_evidence(
        &mut evidence,
        &mut score,
        "directory exists",
        path.is_dir(),
        2,
    );
    match tool_id {
        ToolId::Claude => {
            add_evidence(
                &mut evidence,
                &mut score,
                "settings.json",
                path.join("settings.json").exists(),
                1,
            );
            add_evidence(
                &mut evidence,
                &mut score,
                ".claude.json",
                path.join(".claude.json").exists() || home_dir().join(".claude.json").exists(),
                1,
            );
            add_evidence(
                &mut evidence,
                &mut score,
                "projects",
                path.join("projects").exists(),
                1,
            );
            add_evidence(
                &mut evidence,
                &mut score,
                "history.jsonl",
                path.join("history.jsonl").exists(),
                1,
            );
            add_evidence(
                &mut evidence,
                &mut score,
                "credential",
                claude_credential_exists(&path),
                4,
            );
        }
        ToolId::Codex => {
            add_evidence(
                &mut evidence,
                &mut score,
                "auth.json",
                codex_auth_valid(&path.join("auth.json")),
                4,
            );
            add_evidence(
                &mut evidence,
                &mut score,
                "config.toml",
                path.join("config.toml").exists(),
                2,
            );
            add_evidence(
                &mut evidence,
                &mut score,
                "sessions",
                path.join("sessions").exists(),
                1,
            );
            add_evidence(
                &mut evidence,
                &mut score,
                "skills/rules",
                path.join("skills").exists() || path.join("rules").exists(),
                1,
            );
            add_evidence(
                &mut evidence,
                &mut score,
                "memory/goals",
                path.join("memories_1.sqlite").exists() || path.join("goals_1.sqlite").exists(),
                1,
            );
        }
        ToolId::Antigravity => {
            add_evidence(
                &mut evidence,
                &mut score,
                "directory exists",
                path.is_dir(),
                1,
            );
        }
    }

    let mut warnings = Vec::new();
    if is_app_managed {
        warnings.push(
            "This is an account profile managed by the app. Use it for that account only, not as the shared default config.".to_string(),
        );
    }
    let valid = path.is_dir()
        && evidence
            .iter()
            .any(|item| item.found && item.label != "directory exists");

    ConfigCandidate {
        path,
        source,
        score,
        valid,
        is_app_managed,
        evidence,
        warnings,
    }
}

fn detect_binary_candidates(tool_id: &ToolId) -> Vec<BinaryCandidate> {
    let binary = command_name(tool_id);
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            push_binary_candidate(
                &mut out,
                &mut seen,
                tool_id,
                dir.join(binary),
                DetectionSource::Path,
            );
        }
    }

    for dir in common_bin_dirs() {
        push_binary_candidate(
            &mut out,
            &mut seen,
            tool_id,
            dir.join(binary),
            DetectionSource::Path,
        );
    }

    out
}

fn push_binary_candidate(
    out: &mut Vec<BinaryCandidate>,
    seen: &mut BTreeSet<String>,
    tool_id: &ToolId,
    path: PathBuf,
    source: DetectionSource,
) {
    let key = path.to_string_lossy().to_string();
    if seen.insert(key) && path.exists() {
        out.push(binary_candidate(tool_id, path, source));
    }
}

fn binary_candidate(tool_id: &ToolId, path: PathBuf, source: DetectionSource) -> BinaryCandidate {
    let mut evidence = Vec::new();
    let mut score = source_score(&source);
    let exists = path.exists();
    let executable = is_executable(&path);
    let is_app_launcher = is_our_launcher(&path);
    let resolved_path = fs::canonicalize(&path).ok();
    let in_profile = path
        .to_string_lossy()
        .contains(&format!("/accounts/{}/", tool_id.as_str()));

    add_evidence(&mut evidence, &mut score, "exists", exists, 2);
    add_evidence(&mut evidence, &mut score, "executable", executable, 2);
    add_evidence(
        &mut evidence,
        &mut score,
        "not app launcher",
        !is_app_launcher,
        2,
    );
    add_evidence(
        &mut evidence,
        &mut score,
        "responds to --version",
        command_version_ok(&path),
        1,
    );

    let mut warnings = Vec::new();
    if is_app_launcher {
        warnings
            .push("This is an AI Account Switcher launcher, not the real CLI binary".to_string());
    }
    if in_profile {
        warnings.push("Binary path appears to be inside an account profile".to_string());
    }

    BinaryCandidate {
        path,
        resolved_path,
        source,
        score,
        valid: exists && executable && !is_app_launcher && !in_profile,
        is_app_launcher,
        evidence,
        warnings,
    }
}

fn resolve_detection(
    tool_id: &ToolId,
    configs: &[ConfigCandidate],
    binaries: &[BinaryCandidate],
) -> DetectionResolution {
    let valid_configs = configs.iter().filter(|c| c.valid).collect::<Vec<_>>();
    let valid_binaries = binaries.iter().filter(|b| b.valid).collect::<Vec<_>>();

    if valid_configs.is_empty() || valid_binaries.is_empty() {
        return DetectionResolution {
            kind: ResolutionKind::NeedsManualInput,
            setup: None,
            reason: "No valid config or binary candidate was found".to_string(),
        };
    }

    let config = auto_pick_config(&valid_configs);
    let binary = auto_pick_binary(&valid_binaries);
    match (config, binary) {
        (Some(config), Some(binary)) => DetectionResolution {
            kind: ResolutionKind::Resolved,
            setup: Some(ToolSetup {
                binary_path: Some(binary.path.clone()),
                default_config_dir: Some(config.path.clone()),
                binary_source: binary.source.clone(),
                config_source: config.source.clone(),
                validated_at: Some(chrono::Utc::now().to_rfc3339()),
                validation_warnings: config
                    .warnings
                    .iter()
                    .chain(binary.warnings.iter())
                    .cloned()
                    .collect(),
            }),
            reason: format!("Resolved {}", tool_id.display_name()),
        },
        _ => DetectionResolution {
            kind: ResolutionKind::NeedsUserChoice,
            setup: None,
            reason: "Multiple plausible candidates need user choice".to_string(),
        },
    }
}

fn auto_pick_config<'a>(valid: &[&'a ConfigCandidate]) -> Option<&'a ConfigCandidate> {
    if valid.len() == 1 && !valid[0].is_app_managed {
        return Some(valid[0]);
    }
    let non_app = valid
        .iter()
        .copied()
        .filter(|candidate| !candidate.is_app_managed)
        .collect::<Vec<_>>();
    if non_app.len() == 1 {
        return Some(non_app[0]);
    }
    let top = non_app.first()?;
    let second = non_app.get(1);
    if second.is_none_or(|candidate| top.score >= candidate.score + 3) {
        Some(top)
    } else {
        None
    }
}

fn auto_pick_binary<'a>(valid: &[&'a BinaryCandidate]) -> Option<&'a BinaryCandidate> {
    if valid.len() == 1 {
        return Some(valid[0]);
    }
    let top = valid.first()?;
    let second = valid.get(1);
    if second.is_none_or(|candidate| top.score >= candidate.score + 3) {
        Some(top)
    } else {
        None
    }
}

fn add_evidence(
    evidence: &mut Vec<ValidationEvidence>,
    score: &mut u32,
    label: &str,
    found: bool,
    weight: u32,
) {
    if found {
        *score += weight;
    }
    evidence.push(ValidationEvidence {
        label: label.to_string(),
        found,
    });
}

fn source_score(source: &DetectionSource) -> u32 {
    match source {
        DetectionSource::Env => 4,
        DetectionSource::Default => 3,
        DetectionSource::Path => 2,
        DetectionSource::Manual => 2,
        DetectionSource::AppManaged => 1,
        DetectionSource::Fallback => 0,
    }
}

fn env_config_dir(tool_id: &ToolId) -> Option<PathBuf> {
    let name = match tool_id {
        ToolId::Claude => "CLAUDE_CONFIG_DIR",
        ToolId::Codex => "CODEX_HOME",
        ToolId::Antigravity => return None,
    };
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn is_env_config(tool_id: &ToolId, path: &Path) -> bool {
    env_config_dir(tool_id).is_some_and(|env| env == path)
}

fn path_in_path_env(path: &Path) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| path.parent().is_some_and(|parent| parent == dir))
    })
}

fn claude_credential_exists(config_dir: &Path) -> bool {
    let suffix = claude_keychain_suffix(config_dir);
    keychain_service_exists(&format!("Claude Code-credentials-{suffix}"))
        || config_dir.join(".credentials.json").exists()
}

fn claude_keychain_suffix(config_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(config_dir.to_string_lossy().as_bytes());
    hasher
        .finalize()
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn keychain_service_exists(service: &str) -> bool {
    Command::new("security")
        .args(["find-generic-password", "-s", service, "-w"])
        .output()
        .map(|out| out.status.success() && !out.stdout.is_empty())
        .unwrap_or(false)
}

fn codex_auth_valid(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    value
        .get("tokens")
        .and_then(|tokens| tokens.get("access_token"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|token| !token.is_empty())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn command_version_ok(path: &Path) -> bool {
    Command::new(path)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}
