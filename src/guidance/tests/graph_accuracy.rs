use super::{GraphIndex, HarvestInput, ResolutionMethod};
use common_core::golden::compare_series;

// Accuracy harness over the checked-in fixture repo (fixture_graph/):
// precision/recall per relation plus the severity-1 false-"no callers"
// rate, scored with the shared `compare_series` gate. Controls
// (`control: true`) veto promotion on any `Unique` label regardless of
// aggregates — confidence never stands in for correctness.

fn fixture_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixture_graph")
}

fn load_graph() -> (String, GraphIndex, serde_json::Value) {
    let root = fixture_root();
    let root_str = root.to_string_lossy().into_owned();
    let mut inputs = Vec::new();
    let mut names: Vec<_> = std::fs::read_dir(&root)
        .expect("fixture dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
        .collect();
    names.sort();
    for path in &names {
        inputs.push(HarvestInput {
            path: path.to_string_lossy().into_owned(),
            format: "rust".to_string(),
            text: std::fs::read_to_string(path).expect("fixture reads"),
        });
    }
    let graph = GraphIndex::build(&inputs, std::slice::from_ref(&root_str)).expect("build");
    let truth: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("ground_truth.json")).expect("truth reads"),
    )
    .expect("truth parses");
    (root_str, graph, truth)
}

fn relative(root: &str, absolute: &str) -> String {
    absolute
        .strip_prefix(&format!("{root}/"))
        .unwrap_or(absolute)
        .to_string()
}

fn resolution_label(resolution: ResolutionMethod) -> String {
    match resolution {
        ResolutionMethod::Unique => "unique".to_string(),
        ResolutionMethod::Homonym { count } => format!("homonym:{count}"),
        ResolutionMethod::Unresolved => "unresolved".to_string(),
    }
}

fn rate(hit: usize, total: usize) -> f64 {
    if total == 0 {
        1.0
    } else {
        hit as f64 / total as f64
    }
}

#[test]
fn fixture_accuracy_meets_checked_in_floors() {
    let (root, graph, truth) = load_graph();
    let rel = |path: &str| relative(&root, path);

    // Actual relation sets, relative and sorted.
    let mut actual_calls: Vec<(String, String, String, Vec<String>, String)> = graph
        .call_edges()
        .iter()
        .map(|edge| {
            let mut files: Vec<String> = edge.callee_files.iter().map(|file| rel(file)).collect();
            files.sort();
            (
                rel(&edge.caller_file),
                edge.caller_symbol.clone().unwrap_or_default(),
                edge.callee.clone(),
                files,
                resolution_label(edge.resolution),
            )
        })
        .collect();
    actual_calls.sort();
    let mut actual_imports: Vec<(String, String, Option<String>)> = graph
        .import_edges()
        .iter()
        .map(|edge| {
            (
                rel(&edge.importer),
                edge.specifier.clone(),
                edge.resolved_file.as_deref().map(&rel),
            )
        })
        .collect();
    actual_imports.sort();

    // Expected relation sets from ground truth.
    let expected_calls: Vec<(String, String, String, Vec<String>, String)> = truth["call_edges"]
        .as_array()
        .expect("call_edges")
        .iter()
        .map(|entry| {
            let mut files: Vec<String> = entry["callee_files"]
                .as_array()
                .expect("files")
                .iter()
                .map(|file| file.as_str().expect("str").to_string())
                .collect();
            files.sort();
            (
                entry["caller"].as_str().expect("str").to_string(),
                entry["caller_symbol"].as_str().expect("str").to_string(),
                entry["callee"].as_str().expect("str").to_string(),
                files,
                entry["resolution"].as_str().expect("str").to_string(),
            )
        })
        .collect();
    let expected_imports: Vec<(String, String, Option<String>)> = truth["import_edges"]
        .as_array()
        .expect("import_edges")
        .iter()
        .map(|entry| {
            (
                entry["importer"].as_str().expect("str").to_string(),
                entry["specifier"].as_str().expect("str").to_string(),
                entry["resolved"].as_str().map(str::to_string),
            )
        })
        .collect();

    // Control veto first: a `Unique` on a control fails outright.
    for entry in truth["call_edges"].as_array().expect("call_edges") {
        if entry["control"].as_bool().unwrap_or(false) {
            let callee = entry["callee"].as_str().expect("str");
            for edge in graph
                .call_edges()
                .iter()
                .filter(|edge| edge.callee == callee)
            {
                assert!(
                    edge.resolution != ResolutionMethod::Unique,
                    "control veto: {callee} must not resolve Unique"
                );
            }
        }
    }

    let call_hit = actual_calls
        .iter()
        .filter(|actual| expected_calls.contains(actual))
        .count();
    let import_hit = actual_imports
        .iter()
        .filter(|actual| expected_imports.contains(actual))
        .count();
    let call_precision = rate(call_hit, actual_calls.len());
    let call_recall = rate(call_hit, expected_calls.len());
    let import_precision = rate(import_hit, actual_imports.len());
    let import_recall = rate(import_hit, expected_imports.len());

    let seeds: Vec<String> = truth["dependents"]["seeds"]
        .as_array()
        .expect("seeds")
        .iter()
        .map(|seed| format!("{root}/{}", seed.as_str().expect("str")))
        .collect();
    let mut actual_closure: Vec<String> = graph
        .dependents_closure(&seeds)
        .iter()
        .map(|file| rel(file))
        .collect();
    actual_closure.sort();
    let mut expected_closure: Vec<String> = truth["dependents"]["closure"]
        .as_array()
        .expect("closure")
        .iter()
        .map(|file| file.as_str().expect("str").to_string())
        .collect();
    expected_closure.sort();
    let closure_match = f64::from(actual_closure == expected_closure);

    // Severity-1: a symbol with known callers must never report none.
    let expected_callers = truth["symbols_with_callers"].as_object().expect("callers");
    let mut no_caller_errors = 0usize;
    for (symbol, callers) in expected_callers {
        let actual: Vec<String> = graph
            .call_edges()
            .iter()
            .filter(|edge| edge.callee == *symbol && !edge.callee_files.is_empty())
            .map(|edge| rel(&edge.caller_file))
            .collect();
        if !callers.as_array().expect("array").is_empty() && actual.is_empty() {
            no_caller_errors += 1;
        }
    }
    let no_caller_ok = if no_caller_errors == 0 { 1.0 } else { 0.0 };

    // Floors come from the fixture; eps is the tightest allowance, so a
    // loosened floor travels with this derivation under reasoned update.
    let floors = &truth["thresholds"];
    let precision_floor = floors["precision"].as_f64().expect("floor");
    let recall_floor = floors["recall"].as_f64().expect("floor");
    let eps = (1.0 - precision_floor).min(1.0 - recall_floor);
    let measured = [
        Some(call_precision),
        Some(call_recall),
        Some(import_precision),
        Some(import_recall),
        Some(closure_match),
        Some(no_caller_ok),
    ];
    let report = compare_series(&measured, &[Some(1.0); 6], eps);
    assert!(
        report.matches,
        "accuracy below checked-in floors: measured={measured:?} report={report:?}"
    );
    assert_eq!(
        floors["false_no_caller_rate"],
        serde_json::json!(0.0),
        "this harness only supports a zero false-no-caller floor"
    );
    assert_eq!(no_caller_errors, 0, "severity-1: symbols lost all callers");
}
