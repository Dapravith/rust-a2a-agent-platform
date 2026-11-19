//! Prometheus metrics in the text exposition format, served at `/metrics`.
//!
//! Every label has a bounded value set: route *templates* (never raw paths),
//! HTTP method, status class, agent role, workflow outcome and stable error
//! codes. Workflow, run and user identifiers are never labels.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;

/// Upper bounds in seconds; agent calls can take minutes with a real LLM.
const BUCKETS: [f64; 14] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 15.0, 30.0, 60.0, 240.0,
];

#[derive(Default, Clone)]
struct Histogram {
    counts: [u64; BUCKETS.len()],
    sum: f64,
    count: u64,
}

impl Histogram {
    fn observe(&mut self, secs: f64) {
        for (i, le) in BUCKETS.iter().enumerate() {
            if secs <= *le {
                self.counts[i] += 1;
            }
        }
        self.sum += secs;
        self.count += 1;
    }
}

#[derive(Default)]
struct State {
    http_requests: BTreeMap<(String, &'static str, &'static str), u64>,
    http_duration: BTreeMap<String, Histogram>,
    workflows: BTreeMap<(&'static str, String), u64>,
    agent_calls: BTreeMap<(String, &'static str, String), u64>,
    agent_duration: BTreeMap<String, Histogram>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default)
}

fn with<F: FnOnce(&mut State)>(f: F) {
    // A poisoned lock only means another thread panicked mid-update; the
    // counters are still usable, so recover rather than lose metrics.
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s);
}

fn method_label(m: &axum::http::Method) -> &'static str {
    match m.as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    }
}

fn status_class(code: u16) -> &'static str {
    match code {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

/// Route-layer middleware: records count and latency per route template.
pub async fn track(req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());
    let method = method_label(req.method());
    let start = Instant::now();
    let res = next.run(req).await;
    let class = status_class(res.status().as_u16());
    let secs = start.elapsed().as_secs_f64();
    with(|s| {
        *s.http_requests
            .entry((route.clone(), method, class))
            .or_default() += 1;
        s.http_duration.entry(route).or_default().observe(secs);
    });
    res
}

/// One finished P0 workflow. `error_code` is a stable code (`None` on success).
pub fn workflow_finished(error_code: Option<&str>) {
    let (outcome, code) = match error_code {
        None => ("completed", String::new()),
        Some(c) => ("failed", c.to_owned()),
    };
    with(|s| *s.workflows.entry((outcome, code)).or_default() += 1);
}

/// One A2A call to `agent` (`worker`/`reviewer`).
pub fn agent_call(agent: &str, elapsed: Duration, error_code: Option<&str>) {
    let (outcome, code) = match error_code {
        None => ("completed", String::new()),
        Some(c) => ("failed", c.to_owned()),
    };
    with(|s| {
        *s.agent_calls
            .entry((agent.to_owned(), outcome, code))
            .or_default() += 1;
        s.agent_duration
            .entry(agent.to_owned())
            .or_default()
            .observe(elapsed.as_secs_f64());
    });
}

fn histogram(out: &mut String, name: &str, label: &str, value: &str, h: &Histogram) {
    for (i, le) in BUCKETS.iter().enumerate() {
        let _ = writeln!(
            out,
            "{name}_bucket{{{label}=\"{value}\",le=\"{le}\"}} {}",
            h.counts[i]
        );
    }
    let _ = writeln!(
        out,
        "{name}_bucket{{{label}=\"{value}\",le=\"+Inf\"}} {}",
        h.count
    );
    let _ = writeln!(out, "{name}_sum{{{label}=\"{value}\"}} {}", h.sum);
    let _ = writeln!(out, "{name}_count{{{label}=\"{value}\"}} {}", h.count);
}

/// Renders every metric; `database_up` is measured by the caller at scrape time.
pub fn render(database_up: bool) -> String {
    let s = state().lock().unwrap_or_else(|e| e.into_inner());
    let mut out = String::with_capacity(4096);
    out.push_str("# HELP a2a_database_up Whether the API can query PostgreSQL (1) or not (0).\n");
    out.push_str("# TYPE a2a_database_up gauge\n");
    let _ = writeln!(out, "a2a_database_up {}", u8::from(database_up));

    out.push_str("# HELP a2a_http_requests_total HTTP requests by route template, method and status class.\n");
    out.push_str("# TYPE a2a_http_requests_total counter\n");
    for ((route, method, class), n) in &s.http_requests {
        let _ = writeln!(
            out,
            "a2a_http_requests_total{{route=\"{route}\",method=\"{method}\",status_class=\"{class}\"}} {n}"
        );
    }
    out.push_str(
        "# HELP a2a_http_request_duration_seconds HTTP request latency by route template.\n",
    );
    out.push_str("# TYPE a2a_http_request_duration_seconds histogram\n");
    for (route, h) in &s.http_duration {
        histogram(
            &mut out,
            "a2a_http_request_duration_seconds",
            "route",
            route,
            h,
        );
    }

    out.push_str("# HELP a2a_workflows_total Finished Worker -> Reviewer workflows by outcome and error code.\n");
    out.push_str("# TYPE a2a_workflows_total counter\n");
    for ((outcome, code), n) in &s.workflows {
        let _ = writeln!(
            out,
            "a2a_workflows_total{{outcome=\"{outcome}\",error_code=\"{code}\"}} {n}"
        );
    }

    out.push_str("# HELP a2a_agent_calls_total A2A calls by agent, outcome and error code.\n");
    out.push_str("# TYPE a2a_agent_calls_total counter\n");
    for ((agent, outcome, code), n) in &s.agent_calls {
        let _ = writeln!(
            out,
            "a2a_agent_calls_total{{agent=\"{agent}\",outcome=\"{outcome}\",error_code=\"{code}\"}} {n}"
        );
    }
    out.push_str("# HELP a2a_agent_call_duration_seconds A2A call latency by agent.\n");
    out.push_str("# TYPE a2a_agent_call_duration_seconds histogram\n");
    for (agent, h) in &s.agent_duration {
        histogram(
            &mut out,
            "a2a_agent_call_duration_seconds",
            "agent",
            agent,
            h,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_buckets_are_cumulative() {
        let mut h = Histogram::default();
        h.observe(0.003);
        h.observe(0.2);
        h.observe(500.0);
        assert_eq!(h.counts[0], 1, "le=0.005");
        assert_eq!(h.counts[5], 2, "le=0.25");
        assert_eq!(h.counts[BUCKETS.len() - 1], 2, "le=240 excludes 500 s");
        assert_eq!(h.count, 3, "+Inf counts every observation");
    }

    #[test]
    fn unknown_methods_collapse_to_one_label() {
        let m = axum::http::Method::from_bytes(b"PROPFIND").unwrap_or(axum::http::Method::GET);
        assert_eq!(method_label(&m), "OTHER");
    }
}
