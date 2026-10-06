/// Metrics abstraction kept independent from any exporter or runtime.
pub trait MetricSink: Send + Sync {
    fn increment_counter(&self, name: &'static str, amount: u64);
    fn observe(&self, name: &'static str, value: f64);
}

/// No-op metrics sink for callers that do not need recording.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopMetrics;

impl MetricSink for NoopMetrics {
    fn increment_counter(&self, _name: &'static str, _amount: u64) {}

    fn observe(&self, _name: &'static str, _value: f64) {}
}
