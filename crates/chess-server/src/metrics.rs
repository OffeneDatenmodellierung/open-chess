//! Prometheus metrics for the chess server.

use prometheus::{IntCounter, IntCounterVec, IntGaugeVec, Opts, Registry};

#[derive(Clone)]
pub struct Metrics {
    pub registry: Registry,
    pub moves: IntCounter,
    pub games_started: IntCounter,
    pub games_finished: IntCounterVec,
    pub challenges: IntCounterVec,
    pub streams: IntGaugeVec,
    pub analysed: IntCounterVec,
}

impl Metrics {
    pub fn new() -> Metrics {
        let registry = Registry::new_custom(Some("chess".into()), None).expect("valid prefix");
        let moves = IntCounter::new("moves_total", "Moves played").expect("valid metric");
        let games_started = IntCounter::new("games_started_total", "Games started").expect("valid metric");
        let games_finished =
            IntCounterVec::new(Opts::new("games_finished_total", "Games finished, by status"), &["status"])
                .expect("valid metric");
        let challenges =
            IntCounterVec::new(Opts::new("challenges_total", "Challenges, by outcome"), &["outcome"])
                .expect("valid metric");
        let streams =
            IntGaugeVec::new(Opts::new("streams_open", "Open NDJSON streams on this replica"), &["kind"])
                .expect("valid metric");
        let analysed = IntCounterVec::new(
            Opts::new("fairplay_sides_analysed_total", "Players' games screened"),
            &["flagged"],
        )
        .expect("valid metric");
        for c in [
            Box::new(moves.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(games_started.clone()),
            Box::new(games_finished.clone()),
            Box::new(challenges.clone()),
            Box::new(streams.clone()),
            Box::new(analysed.clone()),
        ] {
            registry.register(c).expect("unique metric");
        }
        Metrics { registry, moves, games_started, games_finished, challenges, streams, analysed }
    }

    pub fn render(&self) -> String {
        use prometheus::Encoder;
        let mut buf = Vec::new();
        let _ = prometheus::TextEncoder::new().encode(&self.registry.gather(), &mut buf);
        String::from_utf8(buf).unwrap_or_default()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics::new()
    }
}

/// Counts an open stream for as long as it's alive.
pub struct StreamGuard(prometheus::IntGauge);

impl StreamGuard {
    pub fn new(m: &Metrics, kind: &str) -> StreamGuard {
        let g = m.streams.with_label_values(&[kind]);
        g.inc();
        StreamGuard(g)
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.0.dec();
    }
}
