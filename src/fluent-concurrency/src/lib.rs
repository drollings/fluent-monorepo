#![forbid(unsafe_code)]
#![deny(warnings, clippy::all, clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::large_stack_arrays,
    clippy::non_std_lazy_statics,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::zero_sized_map_values,
    clippy::unnecessary_literal_bound,
    clippy::cast_possible_wrap,
    clippy::unreadable_literal,
    clippy::similar_names,
    clippy::single_char_pattern,
    clippy::byte_char_slices
)]

pub mod capability;
pub mod flow;
pub mod io;
pub mod pool;
pub mod queue;
pub mod router;
pub mod runtime;
pub mod scope;
pub mod zone;

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use fluent_wvr::{Capability, CapabilitySet, Reserve, Runtime};
    use tokio::runtime::Runtime as TokioRt;

    use crate::runtime::test::TestRuntime;
    use crate::runtime::tokio::TokioRuntime;

    struct TestCapA;
    impl Capability for TestCapA {
        fn name(&self) -> &'static str {
            "cap_a"
        }
    }

    struct TestCapB;
    impl Capability for TestCapB {
        fn name(&self) -> &'static str {
            "cap_b"
        }
    }

    #[test]
    fn test_tokio_runtime_spawn() {
        let rt = TokioRt::new().unwrap();
        let runtime = TokioRuntime;
        rt.block_on(async {
            let handle = runtime.spawn(Box::pin(async {}));
            handle.await.unwrap();
        });
    }

    #[test]
    fn test_tokio_runtime_sleep_now() {
        let rt = TokioRt::new().unwrap();
        let runtime = TokioRuntime;
        rt.block_on(async {
            let before = runtime.now();
            runtime.sleep(std::time::Duration::from_millis(1)).await;
            let after = runtime.now();
            assert!(after >= before);
        });
    }

    #[test]
    fn test_test_runtime_with_paused_time() {
        let rt = TokioRt::new().unwrap();
        rt.block_on(async {
            let handle = rt.handle().clone();
            let test_runtime = TestRuntime::new(handle, 42);
            let before = test_runtime.now();
            test_runtime.sleep(std::time::Duration::from_millis(5)).await;
            let after = test_runtime.now();
            assert!(after >= before);
        });
    }

    #[test]
    fn test_test_runtime_spawn() {
        let rt = TokioRt::new().unwrap();
        rt.block_on(async {
            let handle = rt.handle().clone();
            let test_runtime = TestRuntime::new(handle, 42);
            let join = test_runtime.spawn(Box::pin(async {}));
            join.await.unwrap();
        });
    }

    #[test]
    fn test_capability_set_insert_get() {
        let caps = CapabilitySet::new().with(TestCapA).with(TestCapB);
        assert!(caps.get::<TestCapA>().is_some());
        assert!(caps.get::<TestCapB>().is_some());
        assert_eq!(caps.get::<TestCapA>().unwrap().name(), "cap_a");
    }

    #[test]
    fn test_capability_set_missing_returns_none() {
        let caps = CapabilitySet::new().with(TestCapA);
        assert!(caps.get::<TestCapB>().is_none());
    }

    #[test]
    fn test_capability_set_clone() {
        let caps = CapabilitySet::new().with(TestCapA);
        let cloned = caps.clone();
        assert!(cloned.get::<TestCapA>().is_some());
    }

    #[test]
    fn test_reserve_acquire_and_drop_releases() {
        let counter = Arc::new(AtomicUsize::new(2));
        assert_eq!(counter.load(Ordering::SeqCst), 2);

        {
            let _reserve = Reserve::new(Arc::clone(&counter));
            assert_eq!(counter.load(Ordering::SeqCst), 1);
        }
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_reserve_commit_does_not_release() {
        let counter = Arc::new(AtomicUsize::new(2));
        assert_eq!(counter.load(Ordering::SeqCst), 2);

        let reserve = Reserve::new(Arc::clone(&counter));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        reserve.commit();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_reserve_multiple_acquires() {
        let counter = Arc::new(AtomicUsize::new(2));
        let r1 = Reserve::new(Arc::clone(&counter));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        let r2 = Reserve::new(Arc::clone(&counter));
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        r1.commit();
        drop(r2);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    mod m2 {
        use super::*;
        use crate::scope::Scope;
        use crate::zone::{Zone, ZoneSummary};
        use fluent_wvr::{WorkOutput, WorkUnit, WorkError, WorkContext};
        use internment::ArcIntern;
        use std::time::Duration;

        struct TestWorkUnit {
            name: String,
            should_fail: bool,
        }

        impl TestWorkUnit {
            fn ok(name: &str) -> Self {
                Self { name: name.into(), should_fail: false }
            }

            fn fail(name: &str) -> Self {
                Self { name: name.into(), should_fail: true }
            }
        }

        impl WorkUnit for TestWorkUnit {
            fn name(&self) -> &str { &self.name }
            fn depends(&self) -> &[ArcIntern<str>] { &[] }
            fn provides(&self) -> &[ArcIntern<str>] { &[] }
            fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
                if self.should_fail {
                    Err(WorkError::Execution("test failure".into()))
                } else {
                    Ok(WorkOutput::ok("done"))
                }
            }
        }

        #[test]
        fn test_scope_close_drains_tasks() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let mut scope = Scope::new();
                let flag = Arc::new(AtomicUsize::new(0));
                let flag_clone = Arc::clone(&flag);
                scope.spawn(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    flag_clone.fetch_add(1, Ordering::SeqCst);
                });
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert_eq!(flag.load(Ordering::SeqCst), 1);
                scope.close().await;
            });
        }

        #[test]
        fn test_scope_new_is_empty() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let mut scope = Scope::new();
                assert!(scope.is_empty());
                scope.close().await;
            });
        }

        #[test]
        fn test_zone_normal_completion() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
                let caps = CapabilitySet::new();
                let mut zone = Zone::new(runtime, caps);
                zone.register(Arc::new(TestWorkUnit::ok("task1")));
                zone.register(Arc::new(TestWorkUnit::ok("task2")));
                let summary: ZoneSummary = (&mut zone).await;
                assert_eq!(summary.completed.len(), 2);
                assert_eq!(summary.panicked.len(), 0);
                assert_eq!(summary.cancelled.len(), 0);
            });
        }

        #[test]
        fn test_zone_panic_containment() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
                let caps = CapabilitySet::new();
                let mut zone = Zone::new(runtime, caps);
                zone.register(Arc::new(TestWorkUnit::ok("good")));
                zone.register(Arc::new(TestWorkUnit::fail("bad")));
                let summary: ZoneSummary = (&mut zone).await;
                assert_eq!(summary.completed.len(), 1);
                assert_eq!(summary.panicked.len(), 1);
            });
        }

        #[test]
        fn test_zone_timeout_handling() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
                let caps = CapabilitySet::new();
                let mut zone = Zone::new(runtime, caps);
                let unit = Arc::new(TestWorkUnit { name: "slow".into(), should_fail: false });
                zone.register(unit);
                tokio::time::sleep(Duration::from_millis(100)).await;
                drop(zone);
            });
        }

        #[test]
        fn test_zone_retry_with_max_retries() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
                let caps = CapabilitySet::new();
                let mut zone = Zone::new(Arc::clone(&runtime), caps.clone());
                let counter = Arc::new(AtomicUsize::new(0));
                let counter_clone = Arc::clone(&counter);

                struct RetryCounter {
                    name: String,
                    counter: Arc<AtomicUsize>,
                }
                impl WorkUnit for RetryCounter {
                    fn name(&self) -> &str { &self.name }
                    fn depends(&self) -> &[ArcIntern<str>] { &[] }
                    fn provides(&self) -> &[ArcIntern<str>] { &[] }
                    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
                        self.counter.fetch_add(1, Ordering::SeqCst);
                        Err(WorkError::Execution("retry fail".into()))
                    }
                }

                let _ctx = WorkContext { max_retries: 2, ..WorkContext::default() };
                zone.register(Arc::new(RetryCounter { name: "retry_test".into(), counter: counter_clone }));
                drop(zone);
            });
        }
    }

    mod m3 {
        use super::*;
        use crate::pool::{Limiter, PoolError, Queue, WorkerPool};
        use crate::queue::PriorityQueue;
        use crate::router::PartitionedRouter;
        use std::time::Duration;

        #[test]
        fn test_queue_push_and_pop_order() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let q = Queue::new(10);
                q.push(1).await.unwrap();
                q.push(2).await.unwrap();
                q.push(3).await.unwrap();
                assert_eq!(q.pop().await, Some(1));
                assert_eq!(q.pop().await, Some(2));
                assert_eq!(q.pop().await, Some(3));
                q.close();
            });
        }

        #[test]
        fn test_queue_bounded_full() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let q = Queue::new(2);
                assert!(q.push(1).await.is_ok());
                assert!(q.push(2).await.is_ok());
                assert_eq!(q.push(3).await, Err(PoolError::Full));
                q.close();
            });
        }

        #[test]
        fn test_queue_close_wakes_waiters() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let q: Queue<i32> = Queue::new(2);
                let q2 = Arc::new(q);
                let q_clone = Arc::clone(&q2);
                let handle = tokio::spawn(async move {
                    assert_eq!(q_clone.pop().await, None);
                });
                tokio::time::sleep(Duration::from_millis(10)).await;
                q2.close();
                handle.await.unwrap();
            });
        }

        #[test]
        fn test_worker_pool_processes_all_jobs() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let results = Arc::new(std::sync::Mutex::new(Vec::new()));
                let r = Arc::clone(&results);
                let pool = WorkerPool::new(2, 10, move |job: i32| {
                    let r = Arc::clone(&r);
                    async move {
                        let mut guard = r.lock().unwrap();
                        guard.push(job * 2);
                    }
                });
                pool.submit(1).await.unwrap();
                pool.submit(2).await.unwrap();
                pool.submit(3).await.unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                pool.shutdown().await;
                let guard = results.lock().unwrap();
                assert_eq!(guard.len(), 3);
                assert!(guard.contains(&2));
                assert!(guard.contains(&4));
                assert!(guard.contains(&6));
            });
        }

        #[test]
        fn test_limiter_caps_concurrency() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let limiter = Arc::new(Limiter::new(2));
                let counter = Arc::new(AtomicUsize::new(0));
                let max_concurrent = Arc::new(AtomicUsize::new(0));

                let mut handles = Vec::new();
                for _ in 0..5 {
                    let lim = Arc::clone(&limiter);
                    let cnt = Arc::clone(&counter);
                    let max_c = Arc::clone(&max_concurrent);
                    handles.push(tokio::spawn(async move {
                        lim.run(|| async {
                            let prev = cnt.fetch_add(1, Ordering::SeqCst);
                            max_c.fetch_max(prev + 1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            cnt.fetch_sub(1, Ordering::SeqCst);
                        })
                        .await;
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
                assert!(max_concurrent.load(Ordering::SeqCst) <= 2);
            });
        }

        #[test]
        fn test_priority_queue_fast_path() {
            let mut pq = PriorityQueue::new();
            pq.push("a", 0);
            pq.push("b", 0);
            pq.push("c", 0);
            assert_eq!(pq.pop(), Some("a"));
            assert_eq!(pq.pop(), Some("b"));
            assert_eq!(pq.pop(), Some("c"));
            assert_eq!(pq.pop(), None);
        }

        #[test]
        fn test_priority_queue_mixed() {
            let mut pq = PriorityQueue::new();
            pq.push("low", -1);
            pq.push("normal", 0);
            pq.push("high", 1);
            pq.push("critical", 2);
            assert_eq!(pq.pop(), Some("critical"));
            assert_eq!(pq.pop(), Some("high"));
            assert_eq!(pq.pop(), Some("normal"));
            assert_eq!(pq.pop(), Some("low"));
            assert_eq!(pq.pop(), None);
        }

        #[test]
        fn test_priority_queue_empty() {
            let mut pq: PriorityQueue<i32> = PriorityQueue::new();
            assert_eq!(pq.pop(), None);
        }

        #[test]
        fn test_partitioned_router_same_key() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let results = Arc::new(std::sync::Mutex::new(Vec::new()));
                let r1 = Arc::clone(&results);
                let r2 = Arc::clone(&results);
                let pool1 = WorkerPool::new(1, 10, move |job: i32| {
                    let r = Arc::clone(&r1);
                    async move {
                        let mut guard = r.lock().unwrap();
                        guard.push((0, job));
                    }
                });
                let pool2 = WorkerPool::new(1, 10, move |job: i32| {
                    let r = Arc::clone(&r2);
                    async move {
                        let mut guard = r.lock().unwrap();
                        guard.push((1, job));
                    }
                });

                let router = PartitionedRouter::new(vec![pool1, pool2], |key: &String| key.len());
                router.submit(&"a".to_string(), 10).await.unwrap();
                router.submit(&"a".to_string(), 20).await.unwrap();
                // Both go to same shard because "a".len() % 2 == 1
                tokio::time::sleep(Duration::from_millis(100)).await;
                let guard = results.lock().unwrap();
                assert_eq!(guard.len(), 2);
                let shard1_count = guard.iter().filter(|(s, _)| *s == 1).count();
                assert_eq!(shard1_count, 2);
            });
        }

        #[test]
        fn test_partitioned_router_distributes() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let results = Arc::new(std::sync::Mutex::new(Vec::new()));
                let r1 = Arc::clone(&results);
                let r2 = Arc::clone(&results);
                let pool1 = WorkerPool::new(1, 10, move |job: i32| {
                    let r = Arc::clone(&r1);
                    async move {
                        let mut guard = r.lock().unwrap();
                        guard.push((0, job));
                    }
                });
                let pool2 = WorkerPool::new(1, 10, move |job: i32| {
                    let r = Arc::clone(&r2);
                    async move {
                        let mut guard = r.lock().unwrap();
                        guard.push((1, job));
                    }
                });

                let router = PartitionedRouter::new(vec![pool1, pool2], |key: &String| key.len());
                router.submit(&"a".to_string(), 10).await.unwrap();
                router.submit(&"bb".to_string(), 20).await.unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                let guard = results.lock().unwrap();
                assert_eq!(guard.len(), 2);
                let shards: Vec<_> = guard.iter().map(|(s, _)| *s).collect();
                assert!(shards.contains(&0));
                assert!(shards.contains(&1));
            });
        }
    }

    mod m4 {
        use super::*;
        use crate::flow;

        #[test]
        fn test_credit_flow_initial_credit_allows_n_sends() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let spec = flow::CreditSpec {
                    initial: 3,
                    more_after: 2,
                };
                let (sender, _receiver) = flow::new(spec);
                let counter = Arc::new(AtomicUsize::new(0));
                sender
                    .send(|| async {
                        counter.fetch_add(1, Ordering::SeqCst);
                    })
                    .await;
                assert_eq!(counter.load(Ordering::SeqCst), 1);
            });
        }

        #[test]
        fn test_credit_flow_receiver_bumps_after_more_after() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let spec = flow::CreditSpec {
                    initial: 3,
                    more_after: 2,
                };
                let (sender, receiver) = flow::new(spec);

                // Use initial 3 credits
                let counter = Arc::new(AtomicUsize::new(0));
                for _ in 0..3 {
                    let cnt = Arc::clone(&counter);
                    sender
                        .send(|| async move {
                            cnt.fetch_add(1, Ordering::SeqCst);
                        })
                        .await;
                }
                assert_eq!(counter.load(Ordering::SeqCst), 3);

                // Receiver recv's 2 items -> should trigger bump
                receiver.recv();
                receiver.recv();

                // Now sender should have credit again
                let cnt = Arc::clone(&counter);
                sender
                    .send(|| async move {
                        cnt.fetch_add(1, Ordering::SeqCst);
                    })
                    .await;
                assert_eq!(counter.load(Ordering::SeqCst), 4);
            });
        }

        #[test]
        fn test_credit_flow_sender_blocks_when_exhausted() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let spec = flow::CreditSpec {
                    initial: 1,
                    more_after: 2,
                };
                let (sender, receiver) = flow::new(spec);

                // Use the single credit
                sender.send(|| async {}).await;

                // Sender is now blocked, but receiver recv's will bump
                let cnt = Arc::new(AtomicUsize::new(0));
                let cnt_clone = Arc::clone(&cnt);
                let handle = tokio::spawn(async move {
                    sender
                        .send(|| async move {
                            cnt_clone.fetch_add(1, Ordering::SeqCst);
                        })
                        .await;
                });

                // Receiver processes 2 items -> sends bump
                receiver.recv();
                receiver.recv();

                handle.await.unwrap();
                assert_eq!(cnt.load(Ordering::SeqCst), 1);
            });
        }
    }

    mod m5 {
        use super::*;
        use crate::io::db::DbCapability;
        use crate::io::fs::FsCapability;
        use crate::io::net::NetCapability;
        use fluent_wvr::CapabilitySet;

        #[test]
        fn test_fs_read_write_roundtrip() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let tmp = tempfile::NamedTempFile::new().unwrap();
                let path = tmp.path().to_path_buf();
                let fs = FsCapability;
                fs.write(&path, b"hello world")
                    .await
                    .expect("write failed");
                let data = fs.read(&path).await.expect("read failed");
                assert_eq!(data, b"hello world");
                let meta = fs.metadata(&path).await.expect("metadata failed");
                assert!(meta.is_file());
            });
        }

        #[test]
        fn test_net_tcp_connect_refused() {
            let rt = TokioRt::new().unwrap();
            rt.block_on(async {
                let net = NetCapability;
                let result = net
                    .tcp_connect("127.0.0.1:1")
                    .await;
                assert!(result.is_err());
            });
        }

        #[test]
        fn test_db_placeholder_errors() {
            let db = DbCapability;
            let query_result = db.query("SELECT 1");
            assert!(query_result.is_err());
            let exec_result = db.execute("INSERT INTO t VALUES (1)");
            assert!(exec_result.is_err());
        }

        #[test]
        fn test_missing_capability_returns_none() {
            let caps = CapabilitySet::new();
            assert!(caps.get::<FsCapability>().is_none());
            assert!(caps.get::<NetCapability>().is_none());
            assert!(caps.get::<DbCapability>().is_none());
        }

        #[test]
        fn test_capability_gating_fs() {
            let caps = CapabilitySet::new().with(FsCapability);
            assert!(caps.get::<FsCapability>().is_some());
            assert!(caps.get::<NetCapability>().is_none());
        }
    }
}

