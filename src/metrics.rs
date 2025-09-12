use std::{sync::Arc, time::Instant};

use kube::{ResourceExt, runtime::finalizer};
use prometheus_client::{
    encoding::EncodeLabelSet,
    metrics::{counter::Counter, family::Family, histogram::Histogram},
    registry::{Registry, Unit},
};

use crate::types::{Error, Redirect};

#[derive(Clone)]
pub struct Metrics {
    pub reconcile: ReconcileMetrics,
    pub registry: Arc<Registry>,
}

impl Default for Metrics {
    fn default() -> Self {
        let mut registry = Registry::with_prefix("redirect_controller_reconcile");
        let reconcile = ReconcileMetrics::default().register(&mut registry);
        Self {
            registry: Arc::new(registry),
            reconcile,
        }
    }
}

#[derive(Clone)]
pub struct ReconcileMetrics {
    pub runs: Counter,
    pub failures: Family<ErrorLabels, Counter>,
    pub duration: Histogram,
}

impl Default for ReconcileMetrics {
    fn default() -> Self {
        Self {
            runs: Counter::default(),
            failures: Family::<ErrorLabels, Counter>::default(),
            duration: Histogram::new([0.01, 0.1, 0.25, 0.5, 1., 5., 15., 60.]),
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ErrorLabels {
    pub instance: String,
    pub error: String,
}

impl ReconcileMetrics {
    pub fn count_and_measure(&self) -> ReconcileMeasurer {
        self.runs.inc();
        ReconcileMeasurer {
            start: Instant::now(),
            metric: self.duration.clone(),
        }
    }

    pub fn set_failure(&self, redirect: &Redirect, error: &finalizer::Error<Error>) {
        let label = match error {
            finalizer::Error::ApplyFailed(error) => error.metric_label(),
            finalizer::Error::CleanupFailed(error) => error.metric_label(),
            finalizer::Error::AddFinalizer(_) => "add_finalizer".to_string(),
            finalizer::Error::RemoveFinalizer(_) => "remove_finalizer".to_string(),
            finalizer::Error::UnnamedObject => "unnamed_object".to_string(),
            finalizer::Error::InvalidFinalizer => "invalid_finalizer".to_string(),
        };
        self.failures
            .get_or_create(&ErrorLabels {
                instance: redirect.name_any(),
                error: label,
            })
            .inc();
    }

    fn register(self, r: &mut Registry) -> Self {
        r.register_with_unit(
            "duration",
            "reconcile duration",
            Unit::Seconds,
            self.duration.clone(),
        );
        r.register("failures", "reconciliation errors", self.failures.clone());
        r.register("runs", "reconciliations", self.runs.clone());
        self
    }
}

pub struct ReconcileMeasurer {
    start: Instant,
    metric: Histogram,
}

impl Drop for ReconcileMeasurer {
    fn drop(&mut self) {
        #[allow(clippy::cast_precision_loss)]
        let duration = self.start.elapsed().as_millis() as f64 / 1000.0;
        self.metric.observe(duration);
    }
}
