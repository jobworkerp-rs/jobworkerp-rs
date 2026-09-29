//! File-backed Skills catalog with explicit immutable snapshot reloads.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use thiserror::Error;

const DEFAULT_MAX_ROOTS: usize = 16;
const DEFAULT_MAX_ENTRIES_PER_ROOT: usize = 4096;
const DEFAULT_MAX_SKILLS_PER_ROOT: usize = 128;
const DEFAULT_MAX_SKILL_FILE_BYTES: usize = 64 * 1024;

/// Bounds catalog work and the maximum content read per skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogLimits {
    pub max_roots: usize,
    pub max_entries_per_root: usize,
    pub max_skills_per_root: usize,
    pub max_skill_file_bytes: usize,
}

impl Default for CatalogLimits {
    fn default() -> Self {
        Self {
            max_roots: DEFAULT_MAX_ROOTS,
            max_entries_per_root: DEFAULT_MAX_ENTRIES_PER_ROOT,
            max_skills_per_root: DEFAULT_MAX_SKILLS_PER_ROOT,
            max_skill_file_bytes: DEFAULT_MAX_SKILL_FILE_BYTES,
        }
    }
}

/// Failure classification suitable for an administrator-facing reload report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    RootUnavailable,
    RootLimitExceeded,
    EntryLimitExceeded,
    InvalidSkill,
    SkillLimitExceeded,
    NameCollision,
}

/// A reload diagnostic. Root paths are intentionally kept out of user-facing summaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadDiagnostic {
    pub root: PathBuf,
    pub skill_name: Option<String>,
    pub kind: DiagnosticKind,
    pub message: String,
}

/// Summary visible to list/search callers; it does not include filesystem paths or content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
}

/// Activation content explicitly intended to be returned as a tool result, never a system message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillActivationToolResult {
    pub name: String,
    pub content: String,
}

/// Outcome of building and atomically publishing a fresh catalog snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadReport {
    pub generation: u64,
    pub skill_count: usize,
    pub diagnostics: Vec<ReloadDiagnostic>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SkillLookupError {
    #[error("skill not found")]
    NotFound,
}

#[derive(Debug, Clone)]
struct Skill {
    summary: SkillSummary,
    content: String,
}

/// Immutable catalog view. Capture one at the beginning of a request to keep all skill
/// operations in that request on the same generation while a reload may proceed.
#[derive(Debug, Clone)]
pub struct SkillSnapshot {
    generation: u64,
    skills: BTreeMap<String, Skill>,
}

impl SkillSnapshot {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn list(&self) -> Vec<SkillSummary> {
        self.skills
            .values()
            .map(|skill| skill.summary.clone())
            .collect()
    }

    /// Case-insensitive substring search over names and descriptions.
    pub fn search(&self, query: &str) -> Vec<SkillSummary> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return self.list();
        }
        self.skills
            .values()
            .filter(|skill| {
                skill.summary.name.to_lowercase().contains(&query)
                    || skill.summary.description.to_lowercase().contains(&query)
            })
            .map(|skill| skill.summary.clone())
            .collect()
    }

    /// Return the selected skill body as a tool result. The caller must not elevate it to system
    /// authority; this type deliberately represents a tool result rather than a prompt message.
    pub fn activate(&self, name: &str) -> Result<SkillActivationToolResult, SkillLookupError> {
        self.skills
            .get(&normalize_lookup_name(name))
            .map(|skill| SkillActivationToolResult {
                name: skill.summary.name.clone(),
                content: skill.content.clone(),
            })
            .ok_or(SkillLookupError::NotFound)
    }
}

struct CatalogState {
    snapshot: RwLock<Arc<SkillSnapshot>>,
    reload_lock: Mutex<()>,
}

/// Independent file-backed catalog. Roots are supplied by administrator configuration and
/// changing on-disk files has no effect until [`SkillCatalog::reload`] is called.
#[derive(Clone)]
pub struct SkillCatalog {
    roots: Arc<Vec<PathBuf>>,
    limits: CatalogLimits,
    state: Arc<CatalogState>,
}

impl SkillCatalog {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self::with_limits(roots, CatalogLimits::default())
    }

    pub fn with_limits(roots: Vec<PathBuf>, limits: CatalogLimits) -> Self {
        let catalog = Self {
            roots: Arc::new(roots),
            limits,
            state: Arc::new(CatalogState {
                snapshot: RwLock::new(Arc::new(SkillSnapshot {
                    generation: 0,
                    skills: BTreeMap::new(),
                })),
                reload_lock: Mutex::new(()),
            }),
        };
        // Startup is the first explicit publication boundary: existing admin-managed skills
        // must be available without requiring an HTTP reload after every process restart.
        catalog.reload();
        catalog
    }

    /// Return an immutable view. A request can retain this `Arc` across tool calls while later
    /// reloads atomically publish a new view for subsequent requests.
    pub fn snapshot(&self) -> Arc<SkillSnapshot> {
        self.state
            .snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Rescan every configured root and publish only candidates verified in this scan. Root
    /// failures invalidate that root's entire contribution; skill failures exclude that skill.
    pub fn reload(&self) -> ReloadReport {
        let _reload_guard = self
            .state
            .reload_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (skills, diagnostics) = load_catalog(&self.roots, self.limits);
        let mut current = self
            .state
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let snapshot = Arc::new(SkillSnapshot {
            generation: current.generation.saturating_add(1),
            skills,
        });
        let report = ReloadReport {
            generation: snapshot.generation,
            skill_count: snapshot.skills.len(),
            diagnostics,
        };
        *current = snapshot;
        report
    }
}

fn load_catalog(
    roots: &[PathBuf],
    limits: CatalogLimits,
) -> (BTreeMap<String, Skill>, Vec<ReloadDiagnostic>) {
    let mut candidates: HashMap<String, Vec<(PathBuf, Skill)>> = HashMap::new();
    let mut diagnostics = Vec::new();

    for (root_index, root) in roots.iter().enumerate() {
        if root_index >= limits.max_roots {
            diagnostics.push(ReloadDiagnostic {
                root: root.clone(),
                skill_name: None,
                kind: DiagnosticKind::RootLimitExceeded,
                message: "configured root count exceeds the catalog limit".to_owned(),
            });
            continue;
        }

        match load_root(root, limits) {
            Ok((skills, root_diagnostics)) => {
                diagnostics.extend(root_diagnostics);
                for (name, skill) in skills {
                    candidates
                        .entry(name)
                        .or_default()
                        .push((root.clone(), skill));
                }
            }
            Err(error) => diagnostics.push(ReloadDiagnostic {
                root: root.clone(),
                skill_name: None,
                kind: error.kind,
                message: error.message,
            }),
        }
    }

    let mut skills = BTreeMap::new();
    for (name, definitions) in candidates {
        if definitions.len() == 1 {
            if let Some((_, skill)) = definitions.into_iter().next() {
                skills.insert(name, skill);
            }
        } else {
            for (root, _) in definitions {
                diagnostics.push(ReloadDiagnostic {
                    root,
                    skill_name: Some(name.clone()),
                    kind: DiagnosticKind::NameCollision,
                    message: "multiple roots define the same normalized skill name".to_owned(),
                });
            }
        }
    }
    diagnostics.sort_by(|left, right| {
        left.root
            .cmp(&right.root)
            .then_with(|| left.skill_name.cmp(&right.skill_name))
            .then_with(|| (left.kind as u8).cmp(&(right.kind as u8)))
    });
    (skills, diagnostics)
}

struct RootLoadError {
    kind: DiagnosticKind,
    message: String,
}

type RootSkills = Vec<(String, Skill)>;
type RootLoad = (RootSkills, Vec<ReloadDiagnostic>);

fn load_root(root: &Path, limits: CatalogLimits) -> Result<RootLoad, RootLoadError> {
    let metadata = fs::symlink_metadata(root).map_err(|error| RootLoadError {
        kind: DiagnosticKind::RootUnavailable,
        message: format!("cannot inspect configured root: {error}"),
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RootLoadError {
            kind: DiagnosticKind::RootUnavailable,
            message: "configured root is not a non-symlink directory".to_owned(),
        });
    }
    let canonical_root = fs::canonicalize(root).map_err(|error| RootLoadError {
        kind: DiagnosticKind::RootUnavailable,
        message: format!("cannot resolve configured root: {error}"),
    })?;
    let entries = fs::read_dir(&canonical_root).map_err(|error| RootLoadError {
        kind: DiagnosticKind::RootUnavailable,
        message: format!("cannot scan configured root: {error}"),
    })?;

    let mut skills = Vec::new();
    let mut diagnostics = Vec::new();
    let mut skill_directories = 0usize;
    let mut entries_scanned = 0usize;
    for entry in entries {
        entries_scanned = entries_scanned.saturating_add(1);
        if entries_scanned > limits.max_entries_per_root {
            return Err(RootLoadError {
                kind: DiagnosticKind::EntryLimitExceeded,
                message: "root contains more entries than the catalog scan limit".to_owned(),
            });
        }
        let entry = entry.map_err(|error| RootLoadError {
            kind: DiagnosticKind::RootUnavailable,
            message: format!("cannot continue scanning configured root: {error}"),
        })?;
        let file_type = entry.file_type().map_err(|error| RootLoadError {
            kind: DiagnosticKind::RootUnavailable,
            message: format!("cannot inspect root entry: {error}"),
        })?;
        let entry_path = entry.path();
        let dir_name = entry.file_name().to_str().map(str::to_owned);

        if file_type.is_symlink() {
            diagnostics.push(invalid_skill_diagnostic(
                root,
                dir_name,
                "symbolic link entries are not followed".to_owned(),
            ));
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        skill_directories = skill_directories.saturating_add(1);
        if skill_directories > limits.max_skills_per_root {
            return Err(RootLoadError {
                kind: DiagnosticKind::SkillLimitExceeded,
                message: "root contains more skill directories than the catalog limit".to_owned(),
            });
        }

        let Some(dir_name) = dir_name else {
            diagnostics.push(invalid_skill_diagnostic(
                root,
                None,
                "skill directory name is not valid UTF-8".to_owned(),
            ));
            continue;
        };
        let skill_file = entry_path.join("SKILL.md");
        let file_metadata = match fs::symlink_metadata(&skill_file) {
            Ok(metadata) => metadata,
            // An empty directory or a removed SKILL.md is a normal non-skill state.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                diagnostics.push(invalid_skill_diagnostic(
                    root,
                    Some(dir_name),
                    format!("cannot inspect SKILL.md: {error}"),
                ));
                continue;
            }
        };
        if file_metadata.file_type().is_symlink() || !file_metadata.is_file() {
            diagnostics.push(invalid_skill_diagnostic(
                root,
                Some(dir_name),
                "SKILL.md must be a regular, non-symlink file".to_owned(),
            ));
            continue;
        }

        let canonical_dir = match fs::canonicalize(&entry_path) {
            Ok(path) if path.starts_with(&canonical_root) => path,
            Ok(_) => {
                diagnostics.push(invalid_skill_diagnostic(
                    root,
                    Some(dir_name),
                    "skill directory resolves outside its configured root".to_owned(),
                ));
                continue;
            }
            Err(error) => {
                diagnostics.push(invalid_skill_diagnostic(
                    root,
                    Some(dir_name),
                    format!("cannot resolve skill directory: {error}"),
                ));
                continue;
            }
        };
        if file_metadata.len() > limits.max_skill_file_bytes as u64 {
            diagnostics.push(invalid_skill_diagnostic(
                root,
                Some(dir_name),
                "SKILL.md exceeds the configured size limit".to_owned(),
            ));
            continue;
        }
        let file_path = canonical_dir.join("SKILL.md");
        let text = match read_bounded_utf8(&file_path, limits.max_skill_file_bytes) {
            Ok(text) => text,
            Err(error) => {
                diagnostics.push(invalid_skill_diagnostic(
                    root,
                    Some(dir_name),
                    format!("cannot read valid SKILL.md: {error}"),
                ));
                continue;
            }
        };
        match parse_skill(&dir_name, &text) {
            Ok((name, summary, content)) => skills.push((name, Skill { summary, content })),
            Err(message) => {
                diagnostics.push(invalid_skill_diagnostic(root, Some(dir_name), message))
            }
        }
    }
    Ok((skills, diagnostics))
}

fn read_bounded_utf8(path: &Path, max_bytes: usize) -> io::Result<String> {
    let file = File::open(path)?;
    let read_limit = max_bytes.saturating_add(1) as u64;
    let mut bytes = Vec::with_capacity(max_bytes.min(8192));
    file.take(read_limit).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SKILL.md exceeds the configured size limit",
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[derive(Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
}

fn parse_skill(directory_name: &str, text: &str) -> Result<(String, SkillSummary, String), String> {
    let mut lines = text.split_inclusive('\n');
    let first_line = lines.next().ok_or_else(|| "SKILL.md is empty".to_owned())?;
    if first_line.trim_end_matches(['\r', '\n']) != "---" {
        return Err("SKILL.md must start with YAML frontmatter".to_owned());
    }
    let yaml_start = first_line.len();
    let mut offset = yaml_start;
    let mut closing = None;
    for line in lines {
        let line_start = offset;
        offset += line.len();
        if line.trim_end_matches(['\r', '\n']) == "---" {
            closing = Some((line_start, offset));
            break;
        }
    }
    let (yaml_end, body_start) =
        closing.ok_or_else(|| "YAML frontmatter is not closed".to_owned())?;
    let frontmatter: SkillFrontmatter = serde_yaml::from_str(&text[yaml_start..yaml_end])
        .map_err(|error| format!("invalid YAML frontmatter: {error}"))?;
    let name = frontmatter.name.trim().to_owned();
    let normalized_name = normalize_skill_name(&name)
        .ok_or_else(|| "skill name does not match the supported name format".to_owned())?;
    if name != directory_name || normalized_name != directory_name {
        return Err("frontmatter name must match its lowercase skill directory".to_owned());
    }
    let description = frontmatter.description.trim().to_owned();
    if description.is_empty() {
        return Err("skill description must not be empty".to_owned());
    }
    Ok((
        normalized_name.clone(),
        SkillSummary {
            name: normalized_name,
            description,
        },
        text[body_start..].to_owned(),
    ))
}

fn normalize_skill_name(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 || !bytes[0].is_ascii_lowercase() {
        return None;
    }
    let mut previous_hyphen = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte == b'-' {
            if index == 0 || index + 1 == bytes.len() || previous_hyphen {
                return None;
            }
            previous_hyphen = true;
        } else if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            previous_hyphen = false;
        } else {
            return None;
        }
    }
    Some(name.to_ascii_lowercase())
}

fn normalize_lookup_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn invalid_skill_diagnostic(
    root: &Path,
    skill_name: Option<String>,
    message: String,
) -> ReloadDiagnostic {
    ReloadDiagnostic {
        root: root.to_path_buf(),
        skill_name,
        kind: DiagnosticKind::InvalidSkill,
        message,
    }
}
