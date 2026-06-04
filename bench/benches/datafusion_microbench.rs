use criterion::{Criterion, criterion_group, criterion_main};
use opensnow_bench::{build_context, run_query};
use tokio::runtime::Runtime;

fn count_star(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let ctx = rt.block_on(build_context());
    c.bench_function("count_star_1m", |b| {
        b.to_async(&rt)
            .iter(|| async { run_query(&ctx, "SELECT COUNT(*) FROM bench").await });
    });
}

fn group_by_category(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let ctx = rt.block_on(build_context());
    c.bench_function("group_by_1m", |b| {
        b.to_async(&rt).iter(|| async {
            run_query(
                &ctx,
                "SELECT category, SUM(value) AS s FROM bench GROUP BY category",
            )
            .await
        });
    });
}

fn filter_projection(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let ctx = rt.block_on(build_context());
    c.bench_function("filter_projection_1m", |b| {
        b.to_async(&rt).iter(|| async {
            run_query(
                &ctx,
                "SELECT id, value FROM bench WHERE category = 'g_3' AND value > 50.0",
            )
            .await
        });
    });
}

criterion_group!(benches, count_star, group_by_category, filter_projection);
criterion_main!(benches);
