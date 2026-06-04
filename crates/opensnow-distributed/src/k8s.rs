use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::{info, warn};

use crate::operator::{PlanAction, ReconcilePlan, WarehouseSpec, build_reconcile_plan};

/// Abstraction over shell command execution, allowing tests to inject a mock
/// without spawning real processes.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> Result<String>;
}

/// Default implementation that calls the real `kubectl` binary via
/// `std::process::Command`.
pub struct ShellRunner;

impl CommandRunner for ShellRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String> {
        let output = std::process::Command::new(program).args(args).output()?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("{} exited non-zero: {}", program, stderr.trim())
        }
    }
}

/// Outcome for one action in a reconcile plan after it has been applied (or
/// skipped in dry-run mode).
#[derive(Debug, Clone)]
pub enum ApplyOutcome {
    Scaled {
        warehouse: String,
        from: i32,
        to: i32,
    },
    Noop {
        warehouse: String,
        replicas: i32,
    },
    Failed {
        warehouse: String,
        error: String,
    },
}

/// Thin controller that translates `ReconcilePlan` actions into `kubectl`
/// calls against a real Kubernetes cluster.
///
/// Uses `ShellRunner` by default; swap in a custom `CommandRunner` via
/// `KubeController::with_runner` for unit tests.
pub struct KubeController<R: CommandRunner = ShellRunner> {
    pub namespace: String,
    pub kubectl_bin: String,
    runner: R,
}

impl KubeController<ShellRunner> {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            kubectl_bin: "kubectl".to_string(),
            runner: ShellRunner,
        }
    }
}

impl<R: CommandRunner> KubeController<R> {
    pub fn with_runner(namespace: impl Into<String>, runner: R) -> Self {
        Self {
            namespace: namespace.into(),
            kubectl_bin: "kubectl".to_string(),
            runner,
        }
    }

    fn statefulset_name(warehouse: &str) -> String {
        format!("opensnow-worker-{}", warehouse)
    }

    /// Query the current `.spec.replicas` for each warehouse's StatefulSet.
    /// Returns only warehouses whose StatefulSet exists; missing ones are
    /// implicitly 0 (caller should use `unwrap_or(&0)`).
    pub fn observe_replicas(&self, warehouses: &[WarehouseSpec]) -> HashMap<String, i32> {
        let mut map = HashMap::new();
        for spec in warehouses {
            let sts = Self::statefulset_name(&spec.name);
            let result = self.runner.run(
                &self.kubectl_bin,
                &[
                    "get",
                    "statefulset",
                    &sts,
                    "-n",
                    &self.namespace,
                    "-o",
                    "jsonpath={.spec.replicas}",
                    "--ignore-not-found",
                ],
            );
            match result {
                Ok(s) if s.is_empty() => {
                    // StatefulSet does not exist yet → treat as 0 replicas
                }
                Ok(s) => {
                    if let Ok(n) = s.parse::<i32>() {
                        map.insert(spec.name.clone(), n);
                    }
                }
                Err(_) => {
                    // kubectl error → treat as unknown, leave out of map
                }
            }
        }
        map
    }

    /// Scale a single StatefulSet to `replicas`.
    pub fn apply_scale(&self, warehouse: &str, replicas: i32) -> Result<()> {
        let sts = Self::statefulset_name(warehouse);
        self.runner.run(
            &self.kubectl_bin,
            &[
                "scale",
                "statefulset",
                &sts,
                "--replicas",
                &replicas.to_string(),
                "-n",
                &self.namespace,
            ],
        )?;
        Ok(())
    }

    /// Apply an entire reconcile plan. Returns one `ApplyOutcome` per action.
    /// Scale failures are captured as `ApplyOutcome::Failed` so the caller can
    /// log and continue rather than aborting on the first error.
    pub fn apply_plan(&self, plan: &ReconcilePlan) -> Vec<ApplyOutcome> {
        let mut outcomes = Vec::with_capacity(plan.actions.len());
        for action in &plan.actions {
            match action {
                PlanAction::Noop {
                    warehouse,
                    replicas,
                } => {
                    outcomes.push(ApplyOutcome::Noop {
                        warehouse: warehouse.clone(),
                        replicas: *replicas,
                    });
                }
                PlanAction::Scale {
                    warehouse,
                    from,
                    to,
                } => match self.apply_scale(warehouse, *to) {
                    Ok(()) => outcomes.push(ApplyOutcome::Scaled {
                        warehouse: warehouse.clone(),
                        from: *from,
                        to: *to,
                    }),
                    Err(e) => outcomes.push(ApplyOutcome::Failed {
                        warehouse: warehouse.clone(),
                        error: e.to_string(),
                    }),
                },
            }
        }
        outcomes
    }
}

/// Continuously reconcile Kubernetes StatefulSets against the catalog's desired state.
///
/// Each iteration: observe current replica counts → build a plan → apply it → log outcomes.
/// Runs until the process exits. `interval_secs` controls how long to sleep between cycles.
pub async fn run_reconcile_loop<R: CommandRunner + Send + Sync + 'static>(
    controller: Arc<KubeController<R>>,
    catalog: Arc<opensnow_catalog::Catalog>,
    interval_secs: u64,
) {
    info!("K8s reconcile loop starting (interval={}s)", interval_secs);
    loop {
        let warehouses: Vec<WarehouseSpec> = catalog
            .list_warehouses()
            .unwrap_or_default()
            .into_iter()
            .map(|w| WarehouseSpec {
                name: w.name.clone(),
                size: w.size.clone(),
                min_replicas: w.min_nodes as i32,
                max_replicas: w.max_nodes as i32,
                auto_suspend_seconds: w.auto_suspend_seconds as i32,
                state: w.state.clone(),
            })
            .collect();

        let current = controller.observe_replicas(&warehouses);
        let plan = build_reconcile_plan(&warehouses, &current);
        let outcomes = controller.apply_plan(&plan);

        for outcome in &outcomes {
            match outcome {
                ApplyOutcome::Scaled {
                    warehouse,
                    from,
                    to,
                } => {
                    info!("scaled {} {} → {}", warehouse, from, to);
                }
                ApplyOutcome::Noop {
                    warehouse,
                    replicas,
                } => {
                    info!("noop {} (replicas={})", warehouse, replicas);
                }
                ApplyOutcome::Failed { warehouse, error } => {
                    warn!("failed to scale {}: {}", warehouse, error);
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::{WarehouseSpec, build_reconcile_plan};
    use std::sync::Mutex;

    struct MockRunner {
        calls: Mutex<Vec<String>>,
        responses: Mutex<HashMap<String, std::result::Result<String, String>>>,
    }

    impl MockRunner {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(HashMap::new()),
            }
        }

        fn set_response(&self, args_key: &str, response: std::result::Result<String, String>) {
            self.responses
                .lock()
                .unwrap()
                .insert(args_key.to_string(), response);
        }

        fn recorded_calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CommandRunner for MockRunner {
        fn run(&self, _program: &str, args: &[&str]) -> Result<String> {
            let key = args.join(" ");
            self.calls.lock().unwrap().push(key.clone());
            match self.responses.lock().unwrap().get(&key).cloned() {
                Some(Ok(s)) => Ok(s),
                Some(Err(e)) => anyhow::bail!(e),
                None => Ok(String::new()),
            }
        }
    }

    fn make_spec(name: &str, state: &str) -> WarehouseSpec {
        WarehouseSpec {
            name: name.to_string(),
            size: "small".to_string(),
            min_replicas: 1,
            max_replicas: 4,
            auto_suspend_seconds: 300,
            state: state.to_string(),
        }
    }

    #[test]
    fn test_observe_replicas_parses_output() {
        let runner = MockRunner::new();
        runner.set_response(
            "get statefulset opensnow-worker-default -n opensnow -o jsonpath={.spec.replicas} --ignore-not-found",
            Ok("3".to_string()),
        );

        let controller = KubeController::with_runner("opensnow", runner);
        let specs = vec![make_spec("default", "RUNNING")];
        let replicas = controller.observe_replicas(&specs);

        assert_eq!(replicas.get("default"), Some(&3));
    }

    #[test]
    fn test_observe_replicas_missing_statefulset_returns_zero() {
        let runner = MockRunner::new();
        // Empty string = --ignore-not-found returned nothing (StatefulSet absent)
        runner.set_response(
            "get statefulset opensnow-worker-etl -n opensnow -o jsonpath={.spec.replicas} --ignore-not-found",
            Ok(String::new()),
        );

        let controller = KubeController::with_runner("opensnow", runner);
        let specs = vec![make_spec("etl", "RUNNING")];
        let replicas = controller.observe_replicas(&specs);

        assert_eq!(replicas.get("etl"), None);
        // Caller uses unwrap_or(&0), so effective current = 0
    }

    #[test]
    fn test_apply_scale_sends_correct_kubectl_args() {
        let runner = MockRunner::new();
        runner.set_response(
            "scale statefulset opensnow-worker-default --replicas 2 -n opensnow",
            Ok("statefulset.apps/opensnow-worker-default scaled".to_string()),
        );

        let controller = KubeController::with_runner("opensnow", runner);
        controller.apply_scale("default", 2).unwrap();

        let calls = controller.runner.recorded_calls();
        assert_eq!(
            calls[0],
            "scale statefulset opensnow-worker-default --replicas 2 -n opensnow"
        );
    }

    #[test]
    fn test_apply_scale_propagates_kubectl_error() {
        let runner = MockRunner::new();
        runner.set_response(
            "scale statefulset opensnow-worker-broken --replicas 1 -n opensnow",
            Err("Error from server (NotFound): statefulsets.apps \"opensnow-worker-broken\" not found".to_string()),
        );

        let controller = KubeController::with_runner("opensnow", runner);
        let result = controller.apply_scale("broken", 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_apply_plan_noop_does_not_call_kubectl_scale() {
        let runner = MockRunner::new();
        // No scale response registered — if scale is called, it returns Ok("") which is fine,
        // but we verify calls to confirm only get was issued (none in noop).
        let controller = KubeController::with_runner("opensnow", runner);

        let specs = vec![make_spec("default", "RUNNING")];
        let current: HashMap<String, i32> = std::iter::once(("default".to_string(), 1)).collect();
        let plan = build_reconcile_plan(&specs, &current);

        let outcomes = controller.apply_plan(&plan);
        assert_eq!(outcomes.len(), 1);
        assert!(
            matches!(&outcomes[0], ApplyOutcome::Noop { warehouse, replicas: 1 } if warehouse == "default")
        );

        // No kubectl calls should have been made
        assert!(controller.runner.recorded_calls().is_empty());
    }

    #[test]
    fn test_apply_plan_scale_action_triggers_kubectl() {
        let runner = MockRunner::new();
        runner.set_response(
            "scale statefulset opensnow-worker-default --replicas 0 -n opensnow",
            Ok("scaled".to_string()),
        );

        let controller = KubeController::with_runner("opensnow", runner);

        let specs = vec![make_spec("default", "SUSPENDED")];
        let current: HashMap<String, i32> = std::iter::once(("default".to_string(), 2)).collect();
        let plan = build_reconcile_plan(&specs, &current);

        let outcomes = controller.apply_plan(&plan);
        assert_eq!(outcomes.len(), 1);
        assert!(
            matches!(&outcomes[0], ApplyOutcome::Scaled { warehouse, from: 2, to: 0 } if warehouse == "default")
        );
    }

    #[test]
    fn test_apply_plan_scale_failure_captured_not_panicked() {
        let runner = MockRunner::new();
        runner.set_response(
            "scale statefulset opensnow-worker-default --replicas 1 -n opensnow",
            Err("kubectl: command not found".to_string()),
        );

        let controller = KubeController::with_runner("opensnow", runner);

        let specs = vec![make_spec("default", "RUNNING")];
        let current: HashMap<String, i32> = HashMap::new();
        let plan = build_reconcile_plan(&specs, &current);

        let outcomes = controller.apply_plan(&plan);
        assert_eq!(outcomes.len(), 1);
        assert!(
            matches!(&outcomes[0], ApplyOutcome::Failed { warehouse, .. } if warehouse == "default")
        );
    }
}
