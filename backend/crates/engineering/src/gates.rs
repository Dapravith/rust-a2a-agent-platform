//! Mandatory quality gates for a delivery approval.
//!
//! A human may always reject or request changes. A human may *approve* only
//! when every mandatory gate passes for the exact candidate commit: evidence
//! for another commit, missing evidence, failing checks, blocking findings or
//! a `blocked` assessment make approval impossible (failed stages can never
//! become approved deliveries). The Quality Lead's `ready` is *not* required:
//! it is advice, and the human decides on `changes_required`.

use serde::Serialize;

use crate::artifacts::{CodeCandidate, QaReport, QualityAssessment, Recommendation, ReviewReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    CandidatePresent,
    ReviewForCandidate,
    NoBlockingFindings,
    QaForCandidate,
    QaChecksPassed,
    AcceptanceCriteriaVerified,
    AssessmentForCandidate,
    AssessmentNotBlocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateResult {
    pub gate: Gate,
    pub passed: bool,
    pub detail: String,
}

pub struct Evidence<'a> {
    pub candidate: Option<&'a CodeCandidate>,
    pub review: Option<&'a ReviewReport>,
    pub qa: Option<&'a QaReport>,
    pub assessment: Option<&'a QualityAssessment>,
    /// Acceptance-criterion ids from the approved plan.
    pub criteria: &'a [String],
}

pub fn evaluate(e: &Evidence<'_>) -> Vec<GateResult> {
    let mut out = Vec::new();
    let mut push = |gate, passed: bool, detail: String| {
        out.push(GateResult {
            gate,
            passed,
            detail,
        })
    };
    let sha = e.candidate.map(|c| c.commit_sha.as_str());
    push(
        Gate::CandidatePresent,
        sha.is_some(),
        sha.map_or("no candidate".into(), |s| format!("candidate {s}")),
    );
    let same = |other: Option<&str>| matches!((sha, other), (Some(a), Some(b)) if a == b);

    push(
        Gate::ReviewForCandidate,
        same(e.review.map(|r| r.commit_sha.as_str())),
        match e.review {
            None => "no review report".into(),
            Some(r) => format!("review of {}", r.commit_sha),
        },
    );
    let blocking = e.review.map_or(0, |r| r.blocking().count());
    push(
        Gate::NoBlockingFindings,
        e.review.is_some() && blocking == 0,
        format!("{blocking} high/critical finding(s)"),
    );

    push(
        Gate::QaForCandidate,
        same(e.qa.map(|q| q.commit_sha.as_str())),
        match e.qa {
            None => "no QA report".into(),
            Some(q) => format!("QA of {}", q.commit_sha),
        },
    );
    let failed: Vec<_> =
        e.qa.map(|q| {
            q.results
                .iter()
                .filter(|r| !r.passed())
                .map(|r| r.check_id.clone())
                .collect()
        })
        .unwrap_or_default();
    push(
        Gate::QaChecksPassed,
        e.qa.is_some_and(|q| !q.results.is_empty()) && failed.is_empty(),
        if failed.is_empty() {
            "all checks exited 0".into()
        } else {
            format!("failed: {}", failed.join(", "))
        },
    );
    let unverified: Vec<_> = e
        .criteria
        .iter()
        .filter(|id| {
            !e.qa.is_some_and(|q| {
                q.criteria.iter().any(|c| {
                    &c.criterion_id == *id
                        && c.status == crate::artifacts::CriterionStatus::Verified
                })
            })
        })
        .cloned()
        .collect();
    push(
        Gate::AcceptanceCriteriaVerified,
        !e.criteria.is_empty() && unverified.is_empty(),
        if unverified.is_empty() {
            "all criteria verified".into()
        } else {
            format!("unverified: {}", unverified.join(", "))
        },
    );
    push(
        Gate::AssessmentForCandidate,
        same(e.assessment.map(|a| a.commit_sha.as_str())),
        e.assessment.map_or("no assessment".into(), |a| {
            format!("assessment of {}", a.commit_sha)
        }),
    );
    push(
        Gate::AssessmentNotBlocked,
        e.assessment
            .is_some_and(|a| a.recommendation != Recommendation::Blocked),
        e.assessment.map_or("no assessment".into(), |a| {
            format!("{:?}", a.recommendation)
        }),
    );
    out
}

pub fn all_pass(results: &[GateResult]) -> bool {
    results.iter().all(|r| r.passed)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::artifacts::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const C: &str = "3333333333333333333333333333333333333333";

    fn candidate() -> CodeCandidate {
        CodeCandidate {
            base_commit_sha: A.into(),
            commit_sha: C.into(),
            branch: "b".into(),
            changed_files: vec![],
            patch_sha256: "0".repeat(64),
            checks: vec![],
            limitations: vec![],
        }
    }
    fn review(sha: &str) -> ReviewReport {
        ReviewReport {
            commit_sha: sha.into(),
            summary: "s".into(),
            conclusion: ReviewConclusion::NoBlockingFindings,
            findings: vec![],
        }
    }
    fn qa(sha: &str, exit: i32) -> QaReport {
        QaReport {
            commit_sha: sha.into(),
            environment: QaEnvironment {
                os: "o".into(),
                toolchain: "t".into(),
                sandbox: "s".into(),
            },
            results: vec![CommandResult {
                check_id: "cargo_test".into(),
                exit_code: exit,
                duration_ms: 1,
                output_tail: String::new(),
            }],
            criteria: vec![CriterionResult {
                criterion_id: "AC-1".into(),
                status: CriterionStatus::Verified,
                evidence: "e".into(),
            }],
        }
    }
    fn assessment(sha: &str, r: Recommendation) -> QualityAssessment {
        QualityAssessment {
            commit_sha: sha.into(),
            recommendation: r,
            summary: "s".into(),
            open_issues: vec![],
        }
    }

    #[test]
    fn complete_matching_evidence_passes() {
        let (c, r, q, a) = (
            candidate(),
            review(C),
            qa(C, 0),
            assessment(C, Recommendation::Ready),
        );
        let crit = vec!["AC-1".to_owned()];
        let res = evaluate(&Evidence {
            candidate: Some(&c),
            review: Some(&r),
            qa: Some(&q),
            assessment: Some(&a),
            criteria: &crit,
        });
        assert!(all_pass(&res), "{res:?}");
    }

    #[test]
    fn qa_for_another_commit_fails_the_gate() {
        let (c, r, q, a) = (
            candidate(),
            review(C),
            qa(A, 0),
            assessment(C, Recommendation::Ready),
        );
        let crit = vec!["AC-1".to_owned()];
        let res = evaluate(&Evidence {
            candidate: Some(&c),
            review: Some(&r),
            qa: Some(&q),
            assessment: Some(&a),
            criteria: &crit,
        });
        assert!(
            !res.iter()
                .find(|g| g.gate == Gate::QaForCandidate)
                .unwrap()
                .passed
        );
        assert!(!all_pass(&res));
    }

    #[test]
    fn failing_checks_missing_evidence_or_blocked_cannot_pass() {
        let c = candidate();
        let crit = vec!["AC-1".to_owned()];
        let (r, q, a) = (review(C), qa(C, 101), assessment(C, Recommendation::Ready));
        assert!(!all_pass(&evaluate(&Evidence {
            candidate: Some(&c),
            review: Some(&r),
            qa: Some(&q),
            assessment: Some(&a),
            criteria: &crit
        })));
        assert!(!all_pass(&evaluate(&Evidence {
            candidate: Some(&c),
            review: None,
            qa: None,
            assessment: None,
            criteria: &crit
        })));
        let (q, b) = (qa(C, 0), assessment(C, Recommendation::Blocked));
        assert!(!all_pass(&evaluate(&Evidence {
            candidate: Some(&c),
            review: Some(&r),
            qa: Some(&q),
            assessment: Some(&b),
            criteria: &crit
        })));
    }
}
