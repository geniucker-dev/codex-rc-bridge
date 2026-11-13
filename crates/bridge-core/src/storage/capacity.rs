//! Opt-in synthetic disk workload, not a user-count benchmark or a CI timing assertion.
use super::*;
use crate::vault::{EncryptionKey, SecretScope, Vault};
use std::{path::PathBuf, sync::Arc, time::Instant};
use tokio::{sync::Barrier, task::JoinSet};

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct Samples {
    claim: Vec<f64>,
    read: Vec<f64>,
    refresh: Vec<f64>,
    acquire: Vec<f64>,
    commit: Vec<f64>,
}
fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
fn distribution(mut samples: Vec<f64>) -> serde_json::Value {
    assert!(!samples.is_empty());
    samples.sort_by(f64::total_cmp);
    let percentile = |p: usize| samples[(samples.len() * p).div_ceil(100) - 1];
    serde_json::json!({"count":samples.len(),"p50_ms":percentile(50),
        "p95_ms":percentile(95),"p99_ms":percentile(99),"max_ms":samples.last()})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit disk capacity measurement; run in release mode with --ignored --nocapture"]
async fn sqlite_capacity() {
    const OPERATIONS: usize = 200;
    const PROBES: usize = 64;
    for history in [0, 100_000] {
        for workers in [1, 8, 32] {
            let temp =
                Temp(std::env::temp_dir().join(format!("bridge-capacity-{}", Uuid::new_v4())));
            std::fs::create_dir(&temp.0).unwrap();
            let db = temp.0.join("state.sqlite");
            let store = Arc::new(
                SqliteStore::open(
                    SqliteConnectOptions::new()
                        .filename(&db)
                        .create_if_missing(true),
                )
                .await
                .unwrap(),
            );
            let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[23; 32]).unwrap());
            let tenants: Vec<_> = (0..workers).map(|_| Uuid::new_v4()).collect();
            let account = AccountIdentity {
                workspace_id: "synthetic-workspace".into(),
                user_id: "synthetic-user".into(),
            };
            let scope = SecretScope::Account(account.clone());
            for &tenant in &tenants {
                store.create_tenant(tenant).await.unwrap();
                store.create_account(tenant, &account).await.unwrap();
                vault.write(tenant, &scope, &[0; 1024]).await.unwrap();
            }
            // Seed history in one transaction; setup time is excluded from results.
            let mut tx = store.pool.begin().await.unwrap();
            for i in 0..history {
                let tenant = tenants[i % workers];
                sqlx::query("INSERT INTO used_streams VALUES(?,?,?)")
                    .bind(tenant.to_string())
                    .bind(tenant.to_string())
                    .bind(format!("{i:064x}"))
                    .execute(&mut *tx)
                    .await
                    .unwrap();
            }
            tx.commit().await.unwrap();
            let bytes_before = std::fs::metadata(&db).unwrap().len();
            let barrier = Arc::new(Barrier::new(workers + 1));
            let mut tasks = JoinSet::new();
            for (worker, &tenant) in tenants.iter().enumerate() {
                let (store, vault, barrier, scope) =
                    (store.clone(), vault.clone(), barrier.clone(), scope.clone());
                tasks.spawn(async move {
                    let mut samples = Samples::default();
                    barrier.wait().await;
                    // 60% new streams, 30% protected reads, 10% encrypted credential-sized writes.
                    // Calls use production APIs; timings include the pool wait and encryption.
                    for i in 0..OPERATIONS {
                        let start = Instant::now();
                        match (i + worker) % 10 {
                            0..=5 => {
                                assert!(
                                    store
                                        .claim_stream(
                                            tenant,
                                            tenant,
                                            &format!("{:064x}", history + i)
                                        )
                                        .await
                                        .unwrap()
                                );
                                samples.claim.push(milliseconds(start));
                            }
                            6..=8 => {
                                assert_eq!(
                                    vault.read(tenant, &scope).await.unwrap().unwrap().len(),
                                    1024
                                );
                                samples.read.push(milliseconds(start));
                            }
                            _ => {
                                vault.write(tenant, &scope, &[1; 1024]).await.unwrap();
                                samples.refresh.push(milliseconds(start));
                            }
                        }
                    }
                    samples
                });
            }
            let started = Instant::now();
            barrier.wait().await;
            let mut all = Samples::default();
            while let Some(result) = tasks.join_next().await {
                let sample = result.unwrap();
                all.claim.extend(sample.claim);
                all.read.extend(sample.read);
                all.refresh.extend(sample.refresh);
            }
            let elapsed = started.elapsed().as_secs_f64();
            // Separate explicit-transaction probe isolates pool acquisition and COMMIT time.
            // It is deliberately not labelled as claim_stream's internal breakdown (autocommit).
            let barrier = Arc::new(Barrier::new(workers));
            for &tenant in &tenants {
                let (store, barrier) = (store.clone(), barrier.clone());
                tasks.spawn(async move {
                    let mut samples = Samples::default();
                    barrier.wait().await;
                    for i in 0..PROBES {
                        let start = Instant::now();
                        let mut connection = store.pool.acquire().await.unwrap();
                        samples.acquire.push(milliseconds(start));
                        let mut tx = sqlx::Acquire::begin(&mut *connection).await.unwrap();
                        sqlx::query("INSERT INTO used_streams VALUES(?,?,?)")
                            .bind(tenant.to_string())
                            .bind(tenant.to_string())
                            .bind(format!("{:064x}", history + OPERATIONS + i))
                            .execute(&mut *tx)
                            .await
                            .unwrap();
                        let start = Instant::now();
                        tx.commit().await.unwrap();
                        samples.commit.push(milliseconds(start));
                    }
                    samples
                });
            }
            while let Some(result) = tasks.join_next().await {
                let sample = result.unwrap();
                all.acquire.extend(sample.acquire);
                all.commit.extend(sample.commit);
            }
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM used_streams")
                .fetch_one(&store.pool)
                .await
                .unwrap();
            assert_eq!(
                count,
                (history + workers * (OPERATIONS * 6 / 10 + PROBES)) as i64
            );
            let version: String = sqlx::query_scalar("SELECT sqlite_version()")
                .fetch_one(&store.pool)
                .await
                .unwrap();
            drop(vault);
            Arc::try_unwrap(store).ok().unwrap().close().await;
            println!(
                "{}",
                serde_json::json!({
                    "os":std::env::consts::OS,"arch":std::env::consts::ARCH,"sqlite":version,
                    "profile":if cfg!(debug_assertions) { "debug" } else { "release" },
                    "journal":"delete","synchronous":"extra","fullfsync":true,"connections":1,
                    "history_rows":history,"workers":workers,"operations":workers*OPERATIONS,
                    "mixed_elapsed_seconds":elapsed,"mixed_ops_per_second":(workers*OPERATIONS) as f64/elapsed,
                    "db_bytes_before":bytes_before,"db_bytes_after":std::fs::metadata(&db).unwrap().len(),
                    "claim":distribution(all.claim),"protected_read":distribution(all.read),
                    "credential_write":distribution(all.refresh),
                    "probe_acquire":distribution(all.acquire),"probe_commit":distribution(all.commit)
                })
            );
        }
    }
}
