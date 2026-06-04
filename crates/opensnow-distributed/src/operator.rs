use std::collections::HashMap;

use anyhow::Result;
use opensnow_catalog::{Catalog, Warehouse as CatalogWarehouse};

/// Desired configuration for a virtual warehouse from the operator's
/// perspective. This is derived from catalog metadata and used to
/// drive Kubernetes resources (StatefulSets, KEDA, etc.).
#[derive(Debug, Clone)]
pub struct WarehouseSpec {
    pub name: String,
    /// Logical size class for this warehouse.
    /// Typical values: "small" | "medium" | "large" | "xlarge".
    pub size: String,
    /// Minimum number of worker replicas for this warehouse.
    /// Derived from `min_nodes` in the catalog.
    pub min_replicas: i32,
    /// Maximum number of worker replicas for this warehouse.
    /// Derived from `max_nodes` in the catalog.
    pub max_replicas: i32,
    /// Time in seconds after which an idle warehouse should be
    /// automatically suspended. Derived from `auto_suspend_seconds`.
    pub auto_suspend_seconds: i32,
    /// High-level state for the warehouse. For now this is driven by
    /// the catalog and treated as authoritative.
    /// Expected values: "RUNNING" | "SUSPENDED".
    pub state: String,
}

/// Computed / observed state for a warehouse, decoupled from the
/// catalog metadata. This is mainly useful for tests and future
/// telemetry integration.
#[derive(Debug, Clone)]
pub struct WarehouseState {
    pub name: String,
    pub current_replicas: i32,
    pub desired_replicas: i32,
}

/// Recommended worker resources for a given warehouse. This is
/// intentionally high-level and does not try to model the full
/// Kubernetes API. A future `kube`-based controller can translate
/// this into StatefulSet / Deployment + KEDA ScaledObject specs.
#[derive(Debug, Clone)]
pub struct WorkerDeploymentSpec {
    /// Name of the StatefulSet / worker pool for this warehouse.
    pub name: String,
    /// Kubernetes namespace where the worker resources live.
    pub namespace: String,
    /// Container image to run for workers.
    pub image: String,
    /// CPU request/limit for each worker pod, e.g. "2" for 2 vCPUs.
    pub cpu: String,
    /// Memory request/limit for each worker pod, e.g. "8Gi".
    pub memory: String,
}

/// Map a warehouse size class to recommended CPU and memory.
///
/// These are deliberately coarse and meant as a starting point for
/// K8s resource templates:
/// - small  => 2 vCPU,  8Gi RAM
/// - medium => 4 vCPU, 16Gi RAM
/// - large  => 8 vCPU, 32Gi RAM
/// - xlarge => 16 vCPU, 64Gi RAM
fn size_to_resources(size: &str) -> (&'static str, &'static str) {
    match size.to_lowercase().as_str() {
        "small" => ("2", "8Gi"),
        "medium" => ("4", "16Gi"),
        "large" => ("8", "32Gi"),
        "xlarge" => ("16", "64Gi"),
        // Fallback: treat unknown sizes as "small" to avoid over-
        // provisioning. This is intentionally conservative.
        _ => ("2", "8Gi"),
    }
}

/// Describe the worker resources for a given warehouse. This does
/// **not** talk to the Kubernetes API – it just captures what the
/// operator *wants* the world to look like.
pub fn worker_resources_for(
    warehouse: &WarehouseSpec,
    image: &str,
    namespace: &str,
) -> WorkerDeploymentSpec {
    let (cpu, memory) = size_to_resources(&warehouse.size);
    WorkerDeploymentSpec {
        name: format!("opensnow-worker-{}", warehouse.name),
        namespace: namespace.to_string(),
        image: image.to_string(),
        cpu: cpu.to_string(),
        memory: memory.to_string(),
    }
}

fn i64_to_i32_clamped(value: i64) -> i32 {
    if value > i32::MAX as i64 {
        i32::MAX
    } else if value < i32::MIN as i64 {
        i32::MIN
    } else {
        value as i32
    }
}

/// Adapter that loads warehouses from the catalog and exposes them as
/// `WarehouseSpec` instances used by the operator.
pub fn load_warehouse_specs(catalog: &Catalog) -> Result<Vec<WarehouseSpec>> {
    let warehouses: Vec<CatalogWarehouse> = catalog.list_warehouses()?;
    let specs = warehouses
        .into_iter()
        .map(|w| WarehouseSpec {
            name: w.name,
            size: w.size,
            min_replicas: i64_to_i32_clamped(w.min_nodes),
            max_replicas: i64_to_i32_clamped(w.max_nodes),
            auto_suspend_seconds: i64_to_i32_clamped(w.auto_suspend_seconds),
            state: w.state,
        })
        .collect();
    Ok(specs)
}

/// Plan of actions the operator will take to reconcile the desired
/// warehouse state with the current world (Kubernetes cluster).
#[derive(Debug, Clone)]
pub enum PlanAction {
    Scale {
        warehouse: String,
        from: i32,
        to: i32,
    },
    Noop {
        warehouse: String,
        replicas: i32,
    },
}

#[derive(Debug, Clone)]
pub struct ReconcilePlan {
    pub actions: Vec<PlanAction>,
}

/// Build a reconciliation plan given the desired warehouse specs and
/// the current number of worker replicas observed in the cluster.
///
/// For each warehouse:
/// - If state == RUNNING  → desired >= min_replicas (at least 1)
/// - If state == SUSPENDED → desired = 0
/// - Desired is clamped into [min_replicas, max_replicas]
/// - If current != desired → emit `Scale`, otherwise `Noop`.
pub fn build_reconcile_plan(
    specs: &[WarehouseSpec],
    current_worker_replicas: &HashMap<String, i32>,
) -> ReconcilePlan {
    let mut actions = Vec::with_capacity(specs.len());

    for spec in specs {
        let current = *current_worker_replicas.get(&spec.name).unwrap_or(&0);

        // Normalise min/max to avoid surprising behaviour.
        let min = spec.min_replicas.max(0);
        let mut max = spec.max_replicas.max(0);
        if max < min {
            // If configuration is inconsistent, treat it as
            // "fixed-size" at `min` replicas.
            max = min;
        }

        let mut desired = match spec.state.as_str() {
            "RUNNING" => {
                // At least one replica for RUNNING warehouses, unless
                // max == 0 in which case we keep it at 0.
                if max == 0 {
                    0
                } else {
                    let baseline = std::cmp::max(min, 1);
                    baseline.clamp(min, max)
                }
            }
            "SUSPENDED" => 0,
            // Unknown states are treated conservatively as suspended
            // for now. A future controller could surface these as
            // warnings.
            _ => 0,
        };

        if desired < 0 {
            desired = 0;
        }

        if current == desired {
            actions.push(PlanAction::Noop {
                warehouse: spec.name.clone(),
                replicas: current,
            });
        } else {
            actions.push(PlanAction::Scale {
                warehouse: spec.name.clone(),
                from: current,
                to: desired,
            });
        }
    }

    ReconcilePlan { actions }
}

// ---------------------------------------------------------------------
// Future work notes (not implemented in this pass)
// ---------------------------------------------------------------------
// A full Kubernetes operator would:
// - Watch warehouses via an API or dedicated CRD.
// - Watch StatefulSets / Deployments and KEDA ScaledObjects.
// - Continuously build `ReconcilePlan`s and apply `Scale` actions via
//   the Kubernetes API (using the `kube` crate).
// - Feed in autoscaling metrics (queue depth, CPU, concurrency, etc.)
//   to adjust desired replica counts beyond the simple rules above.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_reconcile_plan_running_min_zero() {
        let specs = vec![WarehouseSpec {
            name: "default".to_string(),
            size: "small".to_string(),
            min_replicas: 0,
            max_replicas: 3,
            auto_suspend_seconds: 300,
            state: "RUNNING".to_string(),
        }];

        let current: HashMap<String, i32> = HashMap::new();
        let plan = build_reconcile_plan(&specs, &current);
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            PlanAction::Scale {
                warehouse,
                from,
                to,
            } => {
                assert_eq!(warehouse, "default");
                assert_eq!(*from, 0);
                assert_eq!(*to, 1); // RUNNING + min=0 => at least 1
            }
            _ => panic!("expected Scale action"),
        }
    }

    #[test]
    fn test_build_reconcile_plan_running_within_bounds() {
        let specs = vec![WarehouseSpec {
            name: "etl".to_string(),
            size: "medium".to_string(),
            min_replicas: 2,
            max_replicas: 5,
            auto_suspend_seconds: 300,
            state: "RUNNING".to_string(),
        }];

        let mut current: HashMap<String, i32> = HashMap::new();
        current.insert("etl".to_string(), 2);

        let plan = build_reconcile_plan(&specs, &current);
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            PlanAction::Noop {
                warehouse,
                replicas,
            } => {
                assert_eq!(warehouse, "etl");
                assert_eq!(*replicas, 2);
            }
            _ => panic!("expected Noop action"),
        }
    }

    #[test]
    fn test_build_reconcile_plan_suspended_scales_to_zero() {
        let specs = vec![WarehouseSpec {
            name: "adhoc".to_string(),
            size: "small".to_string(),
            min_replicas: 1,
            max_replicas: 4,
            auto_suspend_seconds: 60,
            state: "SUSPENDED".to_string(),
        }];

        let mut current: HashMap<String, i32> = HashMap::new();
        current.insert("adhoc".to_string(), 3);

        let plan = build_reconcile_plan(&specs, &current);
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            PlanAction::Scale {
                warehouse,
                from,
                to,
            } => {
                assert_eq!(warehouse, "adhoc");
                assert_eq!(*from, 3);
                assert_eq!(*to, 0);
            }
            _ => panic!("expected Scale action"),
        }
    }

    #[test]
    fn test_build_reconcile_plan_inconsistent_min_max() {
        let specs = vec![WarehouseSpec {
            name: "fixed".to_string(),
            size: "large".to_string(),
            min_replicas: 5,
            max_replicas: 3, // inconsistent, will be normalised to 5
            auto_suspend_seconds: 300,
            state: "RUNNING".to_string(),
        }];

        let current: HashMap<String, i32> = HashMap::new();
        let plan = build_reconcile_plan(&specs, &current);

        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            PlanAction::Scale {
                warehouse,
                from,
                to,
            } => {
                assert_eq!(warehouse, "fixed");
                assert_eq!(*from, 0);
                assert_eq!(*to, 5); // clamped to fixed size of 5
            }
            _ => panic!("expected Scale action"),
        }
    }

    #[test]
    fn test_worker_resources_for_size_mapping() {
        let spec = WarehouseSpec {
            name: "default".to_string(),
            size: "medium".to_string(),
            min_replicas: 1,
            max_replicas: 4,
            auto_suspend_seconds: 300,
            state: "RUNNING".to_string(),
        };

        let worker = worker_resources_for(&spec, "worker-image:latest", "opensnow");
        assert_eq!(worker.name, "opensnow-worker-default");
        assert_eq!(worker.namespace, "opensnow");
        assert_eq!(worker.image, "worker-image:latest");
        assert_eq!(worker.cpu, "4"); // medium => 4 vCPU
        assert_eq!(worker.memory, "16Gi");
    }
}
