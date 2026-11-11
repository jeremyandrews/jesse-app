//! An out of tree driver, written against the `jesse_eval` crate root and nothing else.
//!
//! This is the shape a runner living in another repository takes: implement [`Driver`],
//! hand it to [`run_suite`] in a [`RunConfig`], and read the [`RunReport`]. If a type this
//! needs stops being reachable from the crate root, this file stops compiling.

use jesse_eval::{
    run_suite, BoxFuture, CancellationToken, Driver, PreparedWorkspace, PriceDeck, RunConfig,
    RunReport, Suite, Task, TaskRun,
};

/// Answers every task in memory: writes the file the task asserts on, and replies with one
/// stream-json result line naming the task.
struct InMemoryDriver;

impl Driver for InMemoryDriver {
    fn id(&self) -> &'static str {
        "in-memory"
    }

    fn is_mock(&self) -> bool {
        true
    }

    fn run_task<'a>(
        &'a self,
        task: &'a Task,
        workspace: &'a PreparedWorkspace,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, TaskRun> {
        Box::pin(async move {
            if let Err(e) = std::fs::write(workspace.dir.join("out.txt"), "done") {
                return TaskRun::failed(format!("could not write out.txt: {e}"));
            }
            TaskRun::from_lines(
                vec![format!(
                    r#"{{"type":"result","subtype":"success","result":"answered {}"}}"#,
                    task.id
                )],
                1,
                None,
            )
        })
    }
}

#[test]
fn an_out_of_tree_driver_runs_a_suite_through_the_runner() {
    let suite = Suite::from_json(
        br#"{
            "name": "out-of-tree",
            "tasks": [
                {"id": "one", "class": "c", "prompt": "write out.txt", "workspace": "fixture",
                 "assertions": [
                    {"type": "file_exists", "path": "out.txt"},
                    {"type": "answer_matches", "pattern": "answered one"}
                 ]}
            ]
        }"#,
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    let cfg = RunConfig {
        driver: Box::new(InMemoryDriver),
        prices: PriceDeck::ZERO,
        out_dir: out.path().to_path_buf(),
        runs: 1,
        fixtures_root: None,
    };

    let report: RunReport = run_suite(&suite, &cfg).unwrap();

    assert_eq!(report.driver, "in-memory");
    assert!(report.mock);
    assert_eq!(report.tasks.len(), 1);
    let task = &report.tasks[0];
    assert_eq!(task.id, "one");
    assert!(task.error.is_none(), "{:?}", task.error);
    assert!(task.passed, "{:?}", task.assertions);
    assert!(out.path().join("results.json").is_file());
    assert!(out.path().join("scorecard.md").is_file());
}
