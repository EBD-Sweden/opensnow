# OpenSnow Benchmark Plan

**Status:** PLANNED — execute when AWS infra is ready  
**Goal:** Prove OpenSnow performance and cost vs Athena, DuckDB, Snowflake at scale

---

## Two environments

### Phase 1 — Local machine (now, no infra needed)
Validates correctness + baseline speed. Easy to run today.

### Phase 2 — AWS EKS cluster (later, needs infra)
Real cloud benchmark. This is the publishable number.

---

## Phase 1: Local machine baseline

**What:** Single-node OpenSnow vs DuckDB vs Athena  
**When:** Anytime — no cloud infra needed  
**Hardware:** Dev machine (whatever we have)

### Run order

```bash
# 1. TPC-H SF1 (1GB synthetic, ~2min)
python3 bench/run_tpch_benchmark.sh --sf 1 --runs 3

# 2. TPC-H SF10 (10GB, ~15min) — shows how we scale with data size
python3 bench/run_tpch_benchmark.sh --sf 10 --runs 3

# 3. ClickBench (14.8GB, download once, ~30min total)
bash bench/clickbench.sh

# 4. NYC Taxi 2023 vs Athena (public S3, real cost comparison)
bash bench/nyc_taxi.sh --athena
```

### Expected output
- CSV results per benchmark in `/tmp/opensnow-bench-results/`
- OpenSnow vs DuckDB latency ratio
- Athena cost per query vs $0 for OpenSnow

### Limitation
Local machine = single core DataFusion, no distributed execution.
Numbers are **floor** — cluster will be significantly faster.

---

## Phase 2: AWS EKS cluster (publishable benchmark)

**What:** Multi-node OpenSnow (3 workers) vs Athena on same S3 data  
**When:** After `opensnow-cloud/aws/terraform` is deployed  
**Hardware target:**

| Node | Type | vCPU | RAM | Storage |
|---|---|---|---|---|
| Coordinator × 1 | c5.2xlarge | 8 | 16GB | 100GB NVMe |
| Workers × 3 | c5.4xlarge | 16 | 32GB | 200GB NVMe |
| Total | | 56 vCPU | 112 GB | |

S3: same public NYC Taxi bucket (us-east-1), VPC Gateway Endpoint active (private, free).

### Step-by-step execution

```bash
# Step 1: Deploy infra
cd opensnow-cloud/aws/terraform
cp example.tfvars terraform.tfvars
# Edit: region=us-east-1, worker_count=3, worker_instance_type=c5.4xlarge
terraform init && terraform apply
# Save outputs: OPENSNOW_ENDPOINT, OPENSNOW_PG_ENDPOINT

# Step 2: Point benchmark scripts at the cluster
export OPENSNOW_URL=http://<alb-endpoint>:8080
export AWS_REGION=us-east-1

# Step 3: Run all benchmarks (takes ~2 hours total)
python3 bench/run_tpch_benchmark.sh --sf 10 --runs 5
bash bench/clickbench.sh --runs 5
bash bench/nyc_taxi.sh --athena --years 2019,2020,2021,2022,2023

# Step 4: Tear down (stop paying)
terraform destroy
```

### Estimated AWS cost for benchmark run

| Resource | Duration | Cost |
|---|---|---|
| EKS cluster (4 nodes c5.4xlarge) | 3 hours | ~$12 |
| RDS PostgreSQL (db.t3.medium) | 3 hours | ~$0.50 |
| S3 requests (read NYC Taxi) | — | ~$0.10 |
| ALB + NLB | 3 hours | ~$0.10 |
| **Total** | | **~$13** |

Athena comparison queries cost separately (~$2–5 in scan fees).

---

## What we're measuring

### Metric 1: Query latency (vs DuckDB, vs Athena)

| Query type | Expected OpenSnow | Expected Athena | Expected DuckDB |
|---|---|---|---|
| Simple count (1 col) | < 500ms | 2–5s (cold) | < 200ms |
| Aggregation (3 cols) | < 2s | 3–8s | < 1s |
| Multi-table JOIN | < 5s | 5–20s | < 3s |
| Full scan 100M rows | < 10s | 10–30s | < 5s |

DuckDB is single-process — OpenSnow with 3 workers should match or beat it on large data.

### Metric 2: Cost per query (vs Athena)

Athena: **$5 per TB scanned**  
OpenSnow: **$0 per query** (you pay for compute time, not scans)

| Scenario | Athena/day | OpenSnow/day |
|---|---|---|
| 1,000 queries on 50GB dataset | ~$250 | ~$3 (EC2) |
| 10,000 queries on 50GB dataset | ~$2,500 | ~$3 (EC2) |
| 100,000 queries on 50GB dataset | ~$25,000 | ~$10 (EC2) |

Break-even: OpenSnow is cheaper at >~100 queries/day on a meaningful dataset.

### Metric 3: Scale-to-zero (vs Snowflake)

Snowflake: minimum ~$2–3/credit, warehouses don't truly scale to zero.  
OpenSnow: 0 pods when idle (KEDA), costs $0 while suspended.

---

## Benchmark results template

Once we run Phase 2, fill this in and publish:

```markdown
## Results — OpenSnow v0.1 vs Athena (3-worker EKS, c5.4xlarge)

Dataset: NYC Taxi 2019-2023, 420M rows, ~8GB Parquet on S3 us-east-1
Date: TBD
Cluster: 3× c5.4xlarge workers + 1× c5.2xlarge coordinator

| Query | OpenSnow | Athena | Speedup | Athena cost |
|---|---|---|---|---|
| Total trips | Xms | Xs | Xx | $X |
| Avg fare by hour | Xms | Xs | Xx | $X |
| Top pickup zones | Xms | Xs | Xx | $X |
| Revenue by month | Xms | Xs | Xx | $X |
...

**Summary:**
- OpenSnow Xx faster than Athena on average
- OpenSnow $0 scan cost vs $X/query on Athena
- At 10,000 queries/day: OpenSnow saves ~$X/month
```

---

## Infra checklist (before Phase 2)

- [ ] AWS account with EKS permissions
- [ ] Terraform state backend configured (S3 bucket for tfstate)
- [ ] `opensnow-cloud/aws/terraform/terraform.tfvars` filled in
- [ ] VPC Gateway Endpoint for S3 confirmed active (already in Terraform)
- [ ] `opensnow/opensnow` Docker image published to ECR or Docker Hub
- [ ] `OPENSNOW_ADMIN_PASSWORD` set in AWS Secrets Manager
- [ ] Athena workgroup configured with output S3 bucket (for comparison queries)

---

## Files

```
bench/
├── README.md                  ← dataset reference + quick commands
├── BENCHMARK_PLAN.md          ← this file
├── run_tpch_benchmark.sh      ← TPC-H vs DuckDB (existing)
├── clickbench.sh              ← ClickBench 100M rows vs DuckDB
└── nyc_taxi.sh                ← NYC Taxi real S3 vs Athena
```
