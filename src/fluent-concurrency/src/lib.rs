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
    use std::time::Duration;

    use fluent_wvr::{Capability, CapabilitySet, Reserve, Runtime, WorkContext, WorkError, WorkOutput, WorkUnit};
    use internment::ArcIntern;
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

    #[tokio::test(start_paused = true)]
    async fn test_tokio_runtime_spawn() {
        tokio::time::resume();
        let runtime = TokioRuntime;
        let handle = runtime.spawn(Box::pin(async {}));
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn test_tokio_runtime_sleep_now() {
        tokio::time::resume();
        let runtime = TokioRuntime;
        let before = runtime.now();
        runtime.sleep(Duration::from_millis(1)).await;
        let after = runtime.now();
        assert!(after >= before);
    }

    #[tokio::test(start_paused = true)]
    async fn test_test_runtime_with_paused_time() {
        tokio::time::resume();
        let handle = tokio::runtime::Handle::current();
        let test_runtime = TestRuntime::new(handle, 42);
        let before = test_runtime.now();
        test_runtime.sleep(Duration::from_millis(5)).await;
        let after = test_runtime.now();
        assert!(after >= before);
    }

    #[tokio::test(start_paused = true)]
    async fn test_test_runtime_spawn() {
        tokio::time::resume();
        let handle = tokio::runtime::Handle::current();
        let test_runtime = TestRuntime::new(handle, 42);
        let join = test_runtime.spawn(Box::pin(async {}));
        join.await.unwrap();
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

    #[test]
    fn test_derived_field_access() {
        use fluent_wvr::FieldAccess;

        #[derive(FieldAccess)]
        struct Config {
            label: String,
            count: u32,
        }
        let mut cfg = Config {
            label: "hello".into(),
            count: 5,
        };
        assert_eq!(cfg.get_field("label").unwrap(), "hello");
        assert_eq!(cfg.get_field("count").unwrap(), "5");
        cfg.set_field("label", "world").unwrap();
        cfg.set_field("count", "10").unwrap();
        assert_eq!(cfg.get_field("label").unwrap(), "world");
        assert_eq!(cfg.get_field("count").unwrap(), "10");
        assert!(cfg.set_field("missing", "x").is_err());
        assert_eq!(cfg.field_names(), &["label", "count"]);
    }

    mod m2 {
        use super::*;
        use crate::scope::Scope;
        use crate::zone::{CancelReason, Zone, ZoneSummary, ZoneEvent};

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

        #[tokio::test(start_paused = true)]
        async fn test_scope_close_drains_tasks() {
            tokio::time::resume();
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
        }

        #[tokio::test(start_paused = true)]
        async fn test_scope_new_is_empty() {
            let mut scope = Scope::new();
            assert!(scope.is_empty());
            scope.close().await;
        }

        #[tokio::test(start_paused = true)]
        async fn test_zone_normal_completion() {
            let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
            let caps = CapabilitySet::new();
            let mut zone = Zone::new(runtime, caps);
            zone.register(Arc::new(TestWorkUnit::ok("task1")));
            zone.register(Arc::new(TestWorkUnit::ok("task2")));
            let summary: ZoneSummary = (&mut zone).await;
            assert_eq!(summary.completed.len(), 2);
            assert_eq!(summary.panicked.len(), 0);
            assert_eq!(summary.cancelled.len(), 0);
        }

        #[tokio::test(start_paused = true)]
        async fn test_zone_panic_containment() {
            let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
            let caps = CapabilitySet::new();
            let mut zone = Zone::new(runtime, caps);
            zone.register(Arc::new(TestWorkUnit::ok("good")));
            zone.register(Arc::new(TestWorkUnit::fail("bad")));
            let summary: ZoneSummary = (&mut zone).await;
            assert_eq!(summary.completed.len(), 1);
            assert_eq!(summary.panicked.len(), 1);
            assert_eq!(summary.cancelled.len(), 0);
        }

        #[tokio::test(start_paused = true)]
        async fn test_zone_real_timeout() {
            tokio::time::resume();
            let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
            let caps = CapabilitySet::new();
            let mut zone = Zone::new(runtime, caps);
            let unit = Arc::new(TestWorkUnit::fail("slow"));
            let ctx = WorkContext {
                timeout_ms: 50,
                max_retries: 5,
                ..WorkContext::default()
            };
            zone.register_with_context(unit, ctx);
            let summary: ZoneSummary = (&mut zone).await;
            assert_eq!(summary.completed.len(), 0);
            assert_eq!(summary.panicked.len(), 1);
            assert_eq!(summary.cancelled.len(), 0);
            match &summary.panicked[0] {
                crate::zone::ZoneEvent::Panicked { info, .. } => assert!(info.contains("timed out")),
                _ => panic!("expected Panicked event"),
            }
        }

        #[tokio::test(start_paused = true)]
        async fn test_zone_retry_with_max_retries() {
            tokio::time::resume();
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

            let unit = Arc::new(RetryCounter {
                name: "retry_test".into(),
                counter: counter_clone,
            });
            let ctx = WorkContext { max_retries: 2, ..WorkContext::default() };
            zone.register_with_context(unit, ctx);
            let summary: ZoneSummary = (&mut zone).await;
            assert_eq!(summary.completed.len(), 0);
            assert_eq!(summary.panicked.len(), 1);
            assert_eq!(counter.load(Ordering::SeqCst), 3);
        }

        #[tokio::test(start_paused = true)]
        async fn test_zone_real_panic() {
            let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
            let caps = CapabilitySet::new();
            let mut zone = Zone::new(runtime, caps);

            struct PanicUnit;
            impl WorkUnit for PanicUnit {
                fn name(&self) -> &str { "panic" }
                fn depends(&self) -> &[ArcIntern<str>] { &[] }
                fn provides(&self) -> &[ArcIntern<str>] { &[] }
                fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
                    panic!("intentional panic");
                }
            }

            zone.register(Arc::new(TestWorkUnit::ok("good")));
            zone.register(Arc::new(PanicUnit));
            let summary: ZoneSummary = (&mut zone).await;
            assert_eq!(summary.completed.len(), 1);
            assert_eq!(summary.panicked.len(), 1);
            assert_eq!(summary.cancelled.len(), 0);
            match &summary.panicked[0] {
                crate::zone::ZoneEvent::Panicked { info, .. } => assert!(info.contains("panicked")),
                _ => panic!("expected Panicked event"),
            }
        }

        #[tokio::test(start_paused = true)]
        async fn test_zone_dependency_cancellation() {
            let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
            let caps = CapabilitySet::new();
            let mut zone = Zone::new(runtime, caps);

            struct DepWorkUnit {
                name: String,
                deps: Vec<ArcIntern<str>>,
                provides: Vec<ArcIntern<str>>,
                should_fail: bool,
            }
            impl WorkUnit for DepWorkUnit {
                fn name(&self) -> &str { &self.name }
                fn depends(&self) -> &[ArcIntern<str>] { &self.deps }
                fn provides(&self) -> &[ArcIntern<str>] { &self.provides }
                fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
                    if self.should_fail {
                        Err(WorkError::Execution("dep failure".into()))
                    } else {
                        Ok(WorkOutput::ok("done"))
                    }
                }
            }

            let shared = ArcIntern::<str>::from("shared");
            zone.register(Arc::new(DepWorkUnit {
                name: "parent".into(),
                deps: vec![],
                provides: vec![shared.clone()],
                should_fail: true,
            }));
            let child = Arc::new(DepWorkUnit {
                name: "child".into(),
                deps: vec![shared.clone()],
                provides: vec![],
                should_fail: true,
            });
            zone.register_with_context(
                child,
                WorkContext {
                    max_retries: 10,
                    ..WorkContext::default()
                },
            );
            let summary: ZoneSummary = (&mut zone).await;
            assert_eq!(summary.completed.len(), 0);
            assert_eq!(summary.panicked.len(), 1);
            assert_eq!(summary.cancelled.len(), 1);
            if let ZoneEvent::Cancelled { ref name, ref reason } = summary.cancelled[0] {
                assert_eq!(&**name, "child");
                assert!(matches!(reason, CancelReason::DependencyFailed));
            } else {
                panic!("expected Cancelled event");
            }
        }

        #[tokio::test(start_paused = true)]
        async fn test_zone_drop_cancels_tasks() {
            tokio::time::resume();
            let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
            let caps = CapabilitySet::new();
            let mut zone = Zone::new(runtime, caps);
            let unit = Arc::new(TestWorkUnit::fail("slow"));
            let ctx = WorkContext {
                max_retries: 100,
                ..WorkContext::default()
            };
            zone.register_with_context(unit, ctx);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(zone);
            // If we got here without hanging, the zone dropped correctly
        }

        #[tokio::test(start_paused = true)]
        async fn test_zone_builder_chaining() {
            let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
            let caps = CapabilitySet::new();
            let mut zone = Zone::new(runtime, caps);
            zone
                .register(Arc::new(TestWorkUnit::ok("a")))
                .register(Arc::new(TestWorkUnit::ok("b")));
            let summary: ZoneSummary = (&mut zone).await;
            assert_eq!(summary.completed.len(), 2);
        }
    }

    mod m3 {
        use super::*;
        use crate::pool::{Limiter, PoolError, Queue, WorkerPool};
        use crate::queue::PriorityQueue;
        use crate::router::PartitionedRouter;

        #[tokio::test(start_paused = true)]
        async fn test_queue_push_and_pop_order() {
            let q = Queue::new(10);
            q.push(1).await.unwrap();
            q.push(2).await.unwrap();
            q.push(3).await.unwrap();
            assert_eq!(q.pop().await, Some(1));
            assert_eq!(q.pop().await, Some(2));
            assert_eq!(q.pop().await, Some(3));
            q.close();
        }

        #[tokio::test(start_paused = true)]
        async fn test_queue_bounded_full() {
            let q = Queue::new(2);
            assert!(q.push(1).await.is_ok());
            assert!(q.push(2).await.is_ok());
            assert_eq!(q.push(3).await, Err(PoolError::Full));
            q.close();
        }

        #[tokio::test(start_paused = true)]
        async fn test_queue_close_wakes_waiters() {
            let q: Queue<i32> = Queue::new(2);
            let q2 = Arc::new(q);
            let q_clone = Arc::clone(&q2);
            let handle = tokio::spawn(async move {
                assert_eq!(q_clone.pop().await, None);
            });
            tokio::task::yield_now().await;
            q2.close();
            handle.await.unwrap();
        }

        #[tokio::test(start_paused = true)]
        async fn test_worker_pool_processes_all_jobs() {
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
            pool.shutdown().await;
            let guard = results.lock().unwrap();
            assert_eq!(guard.len(), 3);
            assert!(guard.contains(&2));
            assert!(guard.contains(&4));
            assert!(guard.contains(&6));
        }

        #[tokio::test(start_paused = true)]
        async fn test_worker_pool_shutdown() {
            tokio::time::resume();
            let completed = Arc::new(AtomicUsize::new(0));
            let c = Arc::clone(&completed);
            let pool = WorkerPool::new(2, 10, move |job: i32| {
                let c = Arc::clone(&c);
                async move {
                    tokio::time::sleep(Duration::from_millis(10 * u64::try_from(job).unwrap())).await;
                    c.fetch_add(1, Ordering::SeqCst);
                }
            });
            pool.submit(1).await.unwrap();
            pool.submit(2).await.unwrap();
            pool.submit(3).await.unwrap();
            pool.shutdown().await;
            assert_eq!(completed.load(Ordering::SeqCst), 3);
        }

        #[tokio::test(start_paused = true)]
        async fn test_limiter_caps_concurrency() {
            tokio::time::resume();
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
        fn test_priority_queue_fifo_after_mixed() {
            let mut pq = PriorityQueue::new();
            pq.push("first", 0);
            pq.push("high", 1);
            pq.push("second", 0);
            assert_eq!(pq.pop(), Some("high"));
            assert_eq!(pq.pop(), Some("first"));
            assert_eq!(pq.pop(), Some("second"));
            assert_eq!(pq.pop(), None);
        }

        #[tokio::test(start_paused = true)]
        async fn test_partitioned_router_same_key() {
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
            tokio::task::yield_now().await;
            let guard = results.lock().unwrap();
            assert_eq!(guard.len(), 2);
            let shard1_count = guard.iter().filter(|(s, _)| *s == 1).count();
            assert_eq!(shard1_count, 2);
        }

        #[tokio::test(start_paused = true)]
        async fn test_partitioned_router_distributes() {
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
            tokio::task::yield_now().await;
            let guard = results.lock().unwrap();
            assert_eq!(guard.len(), 2);
            let shards: Vec<_> = guard.iter().map(|(s, _)| *s).collect();
            assert!(shards.contains(&0));
            assert!(shards.contains(&1));
        }
    }

    mod m4 {
        use super::*;
        use crate::flow;

        #[tokio::test(start_paused = true)]
        async fn test_credit_flow_initial_credit_allows_n_sends() {
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
        }

        #[tokio::test(start_paused = true)]
        async fn test_credit_flow_receiver_bumps_after_more_after() {
            let spec = flow::CreditSpec {
                initial: 3,
                more_after: 2,
            };
            let (sender, receiver) = flow::new(spec);

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

            receiver.recv();
            receiver.recv();

            let cnt = Arc::clone(&counter);
            sender
                .send(|| async move {
                    cnt.fetch_add(1, Ordering::SeqCst);
                })
                .await;
            assert_eq!(counter.load(Ordering::SeqCst), 4);
        }

        #[tokio::test(start_paused = true)]
        async fn test_credit_flow_sender_blocks_when_exhausted() {
            let spec = flow::CreditSpec {
                initial: 1,
                more_after: 2,
            };
            let (sender, receiver) = flow::new(spec);

            sender.send(|| async {}).await;

            let cnt = Arc::new(AtomicUsize::new(0));
            let cnt_clone = Arc::clone(&cnt);
            let handle = tokio::spawn(async move {
                sender
                    .send(|| async move {
                        cnt_clone.fetch_add(1, Ordering::SeqCst);
                    })
                    .await;
            });

            receiver.recv();
            receiver.recv();

            handle.await.unwrap();
            assert_eq!(cnt.load(Ordering::SeqCst), 1);
        }

        #[tokio::test(start_paused = true)]
        async fn test_credit_flow_end_to_end() {
            let spec = flow::CreditSpec {
                initial: 5,
                more_after: 3,
            };
            let (sender, receiver) = flow::new(spec);

            let sent = Arc::new(AtomicUsize::new(0));
            let received = Arc::new(AtomicUsize::new(0));

            let s = Arc::clone(&sent);
            let r = Arc::clone(&received);

            let producer = tokio::spawn(async move {
                for _ in 0..10 {
                    let s = Arc::clone(&s);
                    sender
                        .send(|| async move {
                            s.fetch_add(1, Ordering::SeqCst);
                        })
                        .await;
                }
            });

            let consumer = tokio::spawn(async move {
                for _ in 0..10 {
                    receiver.recv();
                    r.fetch_add(1, Ordering::SeqCst);
                }
            });

            producer.await.unwrap();
            consumer.await.unwrap();

            assert_eq!(sent.load(Ordering::SeqCst), 10);
            assert_eq!(received.load(Ordering::SeqCst), 10);
        }
    }

    mod m5 {
        use crate::io::db::DbCapability;
        use crate::io::fs::FsCapability;
        use crate::io::net::NetCapability;
        use fluent_wvr::CapabilitySet;

        #[tokio::test(start_paused = true)]
        async fn test_fs_read_write_roundtrip() {
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
        }

        #[tokio::test(start_paused = true)]
        async fn test_net_tcp_connect_refused() {
            let net = NetCapability::new();
            let result = net
                .tcp_connect("127.0.0.1:1")
                .await;
            assert!(result.is_err());
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

    mod e2e {
        use crate::pool::WorkerPool;
        use crate::zone::{Zone, ZoneSummary};
        use fluent_wvr::{ArcIntern, CapabilitySet, Runtime, WorkContext, WorkError, WorkOutput, WorkUnit};
        use std::sync::Arc;
        use std::time::Duration;
        use crate::runtime::tokio::TokioRuntime;

        /// End-to-end: Zone orchestrates WorkerPool-backed tasks
        #[tokio::test(start_paused = true)]
        async fn test_e2e_zone_with_worker_pool() {
            tokio::time::resume();
            let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
            let caps = CapabilitySet::new();
            let pool = Arc::new(WorkerPool::new(2, 10, |job: i32| async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let _ = job * 2;
            }));

            struct PoolWorkUnit {
                name: String,
                _pool: Arc<WorkerPool<i32>>,
                input: i32,
            }
            impl WorkUnit for PoolWorkUnit {
                fn name(&self) -> &str { &self.name }
                fn depends(&self) -> &[ArcIntern<str>] { &[] }
                fn provides(&self) -> &[ArcIntern<str>] { &[] }
                fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
                    Ok(WorkOutput::ok_with_data(
                        "done",
                        serde_json::json!({ "result": self.input * 2 }),
                    ))
                }
            }

            let mut zone = Zone::new(runtime, caps);
            zone
                .register(Arc::new(PoolWorkUnit { name: "task1".into(), _pool: Arc::clone(&pool), input: 5 }))
                .register(Arc::new(PoolWorkUnit { name: "task2".into(), _pool: Arc::clone(&pool), input: 10 }));

            let summary: ZoneSummary = (&mut zone).await;
            assert_eq!(summary.completed.len(), 2);
            assert_eq!(summary.panicked.len(), 0);
            assert_eq!(summary.cancelled.len(), 0);
        }

        /// End-to-end: Zone handles mixed success/failure/cancellation
        #[tokio::test(start_paused = true)]
        async fn test_e2e_zone_mixed_outcomes() {
            let runtime = Arc::new(TokioRuntime) as Arc<dyn Runtime>;
            let caps = CapabilitySet::new();

            struct OutcomeUnit {
                name: String,
                outcome: &'static str, // "ok", "fail", "panic"
                deps: Vec<ArcIntern<str>>,
                provides: Vec<ArcIntern<str>>,
            }
            impl WorkUnit for OutcomeUnit {
                fn name(&self) -> &str { &self.name }
                fn depends(&self) -> &[ArcIntern<str>] { &self.deps }
                fn provides(&self) -> &[ArcIntern<str>] { &self.provides }
                fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
                    match self.outcome {
                        "ok" => Ok(WorkOutput::ok("done")),
                        "fail" => Err(WorkError::Execution("failed".into())),
                        "panic" => panic!("intentional panic"),
                        _ => unreachable!(),
                    }
                }
            }

            let shared = ArcIntern::<str>::from("shared");
            let mut zone = Zone::new(runtime, caps);
            zone
                .register(Arc::new(OutcomeUnit {
                    name: "root".into(),
                    outcome: "fail",
                    deps: vec![],
                    provides: vec![shared.clone()],
                }))
                .register_with_context(
                    Arc::new(OutcomeUnit {
                        name: "child1".into(),
                        outcome: "fail",
                        deps: vec![shared.clone()],
                        provides: vec![],
                    }),
                    WorkContext {
                        max_retries: 10,
                        ..WorkContext::default()
                    },
                )
                .register(Arc::new(OutcomeUnit {
                    name: "independent".into(),
                    outcome: "panic",
                    deps: vec![],
                    provides: vec![],
                }));

            let summary: ZoneSummary = (&mut zone).await;
            assert_eq!(summary.completed.len(), 0);
            assert_eq!(summary.panicked.len(), 2);
            assert_eq!(summary.cancelled.len(), 1);
        }
    }
}
