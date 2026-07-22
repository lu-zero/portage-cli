//! Fold [`ActivityEvent`]s into a live dashboard snapshot.

use std::collections::{HashMap, HashSet};

use super::event::{
    ActivityEvent, ActivityMergeRoot, ActivityMode, ActivityPlanPkg, PkgKind, SessionFlags,
};
use super::history::EtaPkg;

/// One package currently being acted on.
#[derive(Clone, Debug)]
pub struct InflightPkg {
    pub cpv: String,
    pub cpn: String,
    pub merge_root: ActivityMergeRoot,
    pub index: u32,
    pub of: u32,
    pub kind: PkgKind,
    pub pkg_started_at: f64,
    pub phase: String,
    pub phase_started_at: f64,
    pub phases_done: Vec<(String, f64)>,
}

/// One ongoing (or just-ended) activity session.
#[derive(Clone, Debug)]
pub struct LiveSession {
    pub job_id: String,
    pub parent_job_id: Option<String>,
    pub pid: u32,
    pub started_at: f64,
    pub argv: Vec<String>,
    pub merge_root: String,
    pub host_root: String,
    pub mode: ActivityMode,
    pub plan_total: u32,
    pub flags: SessionFlags,
    pub completed: u32,
    pub failed: u32,
    pub inflight: HashMap<(ActivityMergeRoot, String), InflightPkg>,
    /// Full plan (install order) when the session recorded it.
    pub plan: Vec<ActivityPlanPkg>,
    /// Build-order blockers for [`Self::plan`] (same indices as merge scheduler).
    pub blockers: Vec<Vec<usize>>,
    /// Successfully or failed packages removed from the remaining ETA set.
    pub finished: HashSet<(ActivityMergeRoot, String)>,
    pub ended: bool,
    pub ok: Option<bool>,
}

impl LiveSession {
    pub fn inflight_sorted(&self) -> Vec<&InflightPkg> {
        let mut v: Vec<_> = self.inflight.values().collect();
        v.sort_by_key(|p| p.index);
        v
    }

    /// Remaining plan packages + remapped blockers for critical-path ETA.
    ///
    /// Falls back to inflight-only (no blockers) when the session has no plan.
    pub fn remaining_for_eta(&self) -> (Vec<EtaPkg>, Vec<Vec<usize>>) {
        if self.plan.is_empty() {
            let pkgs: Vec<EtaPkg> = self
                .inflight_sorted()
                .into_iter()
                .map(|p| EtaPkg {
                    cpn: p.cpn.clone(),
                    cpv: p.cpv.clone(),
                })
                .collect();
            let blockers = vec![Vec::new(); pkgs.len()];
            return (pkgs, blockers);
        }

        let mut keep_old: Vec<usize> = Vec::new();
        let mut old_to_new: Vec<Option<usize>> = vec![None; self.plan.len()];
        for (i, p) in self.plan.iter().enumerate() {
            if self.finished.contains(&(p.merge_root, p.cpv.clone())) {
                continue;
            }
            old_to_new[i] = Some(keep_old.len());
            keep_old.push(i);
        }
        let pkgs: Vec<EtaPkg> = keep_old
            .iter()
            .map(|&i| {
                let p = &self.plan[i];
                EtaPkg {
                    cpn: p.cpn.clone(),
                    cpv: p.cpv.clone(),
                }
            })
            .collect();

        let blockers = if self.blockers.len() == self.plan.len() {
            keep_old
                .iter()
                .map(|&old_i| {
                    self.blockers[old_i]
                        .iter()
                        .filter_map(|&pred| old_to_new.get(pred).and_then(|x| *x))
                        .collect()
                })
                .collect()
        } else {
            vec![Vec::new(); pkgs.len()]
        };
        (pkgs, blockers)
    }
}

/// Reducer: apply events → session map (same shape as `em log current`).
#[derive(Clone, Debug, Default)]
pub struct LiveProjection {
    sessions: HashMap<String, LiveSession>,
}

impl LiveProjection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, ev: &ActivityEvent) {
        match ev {
            ActivityEvent::SessionStart {
                job_id,
                parent_job_id,
                pid,
                started_at,
                argv,
                merge_root,
                host_root,
                mode,
                plan_total,
                flags,
                plan,
                blockers,
                ..
            } => {
                self.sessions.insert(
                    job_id.clone(),
                    LiveSession {
                        job_id: job_id.clone(),
                        parent_job_id: parent_job_id.clone(),
                        pid: *pid,
                        started_at: *started_at,
                        argv: argv.clone(),
                        merge_root: merge_root.clone(),
                        host_root: host_root.clone(),
                        mode: *mode,
                        plan_total: *plan_total,
                        flags: flags.clone(),
                        completed: 0,
                        failed: 0,
                        inflight: HashMap::new(),
                        plan: plan.clone(),
                        blockers: blockers.clone(),
                        finished: HashSet::new(),
                        ended: false,
                        ok: None,
                    },
                );
            }
            ActivityEvent::SessionHeartbeat {
                job_id,
                completed,
                failed,
                ..
            } => {
                if let Some(s) = self.sessions.get_mut(job_id) {
                    s.completed = *completed;
                    s.failed = *failed;
                }
            }
            ActivityEvent::SessionEnd {
                job_id,
                ok,
                completed,
                failed,
                ..
            } => {
                if let Some(s) = self.sessions.get_mut(job_id) {
                    s.ended = true;
                    s.ok = Some(*ok);
                    s.completed = *completed;
                    s.failed = *failed;
                    s.inflight.clear();
                }
            }
            ActivityEvent::PkgStart {
                job_id,
                cpv,
                cpn,
                merge_root,
                index,
                of,
                kind,
                at,
                ..
            } => {
                if let Some(s) = self.sessions.get_mut(job_id) {
                    s.inflight.insert(
                        (*merge_root, cpv.clone()),
                        InflightPkg {
                            cpv: cpv.clone(),
                            cpn: cpn.clone(),
                            merge_root: *merge_root,
                            index: *index,
                            of: *of,
                            kind: *kind,
                            pkg_started_at: *at,
                            phase: "starting".into(),
                            phase_started_at: *at,
                            phases_done: Vec::new(),
                        },
                    );
                }
            }
            ActivityEvent::PhaseEnter {
                job_id,
                cpv,
                merge_root,
                phase,
                at,
                ..
            } => {
                if let Some(s) = self.sessions.get_mut(job_id)
                    && let Some(p) = s.inflight.get_mut(&(*merge_root, cpv.clone()))
                {
                    p.phase = phase.clone();
                    p.phase_started_at = *at;
                }
            }
            ActivityEvent::PhaseLeave {
                job_id,
                cpv,
                merge_root,
                phase,
                seconds,
                ..
            } => {
                if let Some(s) = self.sessions.get_mut(job_id)
                    && let Some(p) = s.inflight.get_mut(&(*merge_root, cpv.clone()))
                {
                    p.phases_done.push((phase.clone(), *seconds));
                }
            }
            ActivityEvent::PkgEnd {
                job_id,
                cpv,
                merge_root,
                ok,
                ..
            } => {
                if let Some(s) = self.sessions.get_mut(job_id) {
                    s.inflight.remove(&(*merge_root, cpv.clone()));
                    s.finished.insert((*merge_root, cpv.clone()));
                    if *ok {
                        s.completed += 1;
                    } else {
                        s.failed += 1;
                    }
                }
            }
            // Diagnostics do not change session/inflight state.
            ActivityEvent::Diagnostic { .. } => {}
        }
    }

    /// Active (not ended) sessions, oldest first.
    pub fn active(&self) -> Vec<&LiveSession> {
        let mut v: Vec<_> = self.sessions.values().filter(|s| !s.ended).collect();
        v.sort_by(|a, b| a.started_at.total_cmp(&b.started_at));
        v
    }

    pub fn sessions(&self) -> impl Iterator<Item = &LiveSession> {
        self.sessions.values()
    }

    pub fn get(&self, job_id: &str) -> Option<&LiveSession> {
        self.sessions.get(job_id)
    }

    /// Mutable access for disk-load patches (finished set, etc.).
    pub fn get_mut(&mut self, job_id: &str) -> Option<&mut LiveSession> {
        self.sessions.get_mut(job_id)
    }
}
