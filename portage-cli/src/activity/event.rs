//! Typed activity events — the wire schema for library channels and JSONL.

use serde::{Deserialize, Serialize};

/// Schema version embedded on every serialised event (`"v": 1`).
pub const ACTIVITY_EVENT_VERSION: u32 = 1;

/// Kind of top-level activity session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityMode {
    #[default]
    Merge,
    Unmerge,
    Depclean,
    FetchOnly,
    BuildpkgOnly,
}

/// How a package is being acted on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PkgKind {
    #[default]
    Source,
    Binpkg,
    FetchOnly,
}

/// Host BDEPEND vs target ROOT — wire form matches resume markers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityMergeRoot {
    Host,
    #[default]
    Target,
}

impl ActivityMergeRoot {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Target => "target",
        }
    }
}

impl From<crate::query::depgraph::MergeRoot> for ActivityMergeRoot {
    fn from(r: crate::query::depgraph::MergeRoot) -> Self {
        match r {
            crate::query::depgraph::MergeRoot::Host => Self::Host,
            crate::query::depgraph::MergeRoot::Target => Self::Target,
        }
    }
}

/// Subset of merge/depgraph flags useful for dashboards and ETA.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionFlags {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jobs: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub emptytree: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub update: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deep: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_going: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fetchonly: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub buildpkgonly: bool,
}

/// One phase duration inside a finished package.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhaseTiming {
    pub phase: String,
    pub seconds: f64,
}

/// One plan entry for live-session ETA (`SessionStart.plan` / session.json).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityPlanPkg {
    pub cpn: String,
    pub cpv: String,
    pub merge_root: ActivityMergeRoot,
}

/// Structured progress event. Serialises with `tag = "event"` + `"v": 1`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ActivityEvent {
    SessionStart {
        v: u32,
        job_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_job_id: Option<String>,
        pid: u32,
        started_at: f64,
        argv: Vec<String>,
        merge_root: String,
        host_root: String,
        mode: ActivityMode,
        plan_total: u32,
        flags: SessionFlags,
        /// Full merge plan (install order) for critical-path ETA / dashboards.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        plan: Vec<ActivityPlanPkg>,
        /// Build-order blockers, same shape as `DepgraphOutcome::build_blockers`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blockers: Vec<Vec<usize>>,
    },
    SessionHeartbeat {
        v: u32,
        job_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_job_id: Option<String>,
        at: f64,
        completed: u32,
        failed: u32,
    },
    SessionEnd {
        v: u32,
        job_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_job_id: Option<String>,
        at: f64,
        ok: bool,
        completed: u32,
        failed: u32,
        seconds: f64,
    },
    PkgStart {
        v: u32,
        job_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_job_id: Option<String>,
        cpv: String,
        cpn: String,
        merge_root: ActivityMergeRoot,
        index: u32,
        of: u32,
        kind: PkgKind,
        at: f64,
    },
    PhaseEnter {
        v: u32,
        job_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_job_id: Option<String>,
        cpv: String,
        merge_root: ActivityMergeRoot,
        phase: String,
        at: f64,
    },
    PhaseLeave {
        v: u32,
        job_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_job_id: Option<String>,
        cpv: String,
        merge_root: ActivityMergeRoot,
        phase: String,
        at: f64,
        seconds: f64,
    },
    PkgEnd {
        v: u32,
        job_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_job_id: Option<String>,
        cpv: String,
        cpn: String,
        merge_root: ActivityMergeRoot,
        kind: PkgKind,
        ok: bool,
        at: f64,
        seconds: f64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        phases: Vec<PhaseTiming>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl ActivityEvent {
    /// Unix time as `f64` seconds (subsecond precision).
    pub fn now() -> f64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }

    pub fn job_id(&self) -> &str {
        match self {
            Self::SessionStart { job_id, .. }
            | Self::SessionHeartbeat { job_id, .. }
            | Self::SessionEnd { job_id, .. }
            | Self::PkgStart { job_id, .. }
            | Self::PhaseEnter { job_id, .. }
            | Self::PhaseLeave { job_id, .. }
            | Self::PkgEnd { job_id, .. } => job_id,
        }
    }

    /// One JSON object per line for `--activity-fd` / JSONL sinks.
    pub fn to_jsonl_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_jsonl_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}

/// How a caller wants this merge to correlate with a larger plan (stages).
#[derive(Clone, Debug, Default)]
pub struct ActivitySessionOpts {
    /// Reuse this job id instead of minting a new one (outer session).
    pub job_id: Option<String>,
    /// UI-only parent correlation (e.g. whole stage1 run id).
    pub parent_job_id: Option<String>,
}
