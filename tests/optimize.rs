//! Optimization on the new store (docs/data-management.md, chapter 11): an instruction makes
//! candidate configurations, and each is built and evaluated.
//!
//! The documents are `tests/fixtures/extract`; the instructions and the evaluation data are
//! `tests/fixtures/optimize`.

use std::fs;
use std::path::{Path, PathBuf};

use dunlin::assess::{Case, report, save_eval_data};
use dunlin::optimize::{Candidate, Optimization, OptimizeRequest, State, optimize};
use dunlin::pipeline::{BuildRequest, build};
use dunlin::records::InfoId;
use dunlin::settings::Settings;
use dunlin::store::{RunRecord, SnapshotId, Store};

fn fixtures(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn copy_into(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_into(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn instruction(name: &str) -> String {
    fs::read_to_string(fixtures("optimize").join(name)).unwrap()
}

struct Project {
    root: tempfile::TempDir,
    store: Store,
}

impl Project {
    /// A project with six documents, built with its own configuration, and evaluation data.
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        copy_into(&fixtures("extract/original"), &root.path().join("docs"));
        copy_into(&fixtures("extract/additional"), &root.path().join("docs"));
        let config = instruction("Dunlinfile.yaml");
        fs::write(root.path().join("Dunlinfile.yaml"), &config).unwrap();
        let store = Store::open(&root.path().join(".dunlin/store")).unwrap();
        let project = Self { root, store };
        build(
            &project.store,
            &BuildRequest {
                source: "docs",
                root: &project.root.path().join("docs"),
                indexes: &project.indexes(),
                config: Some(config.as_bytes()),
                apply: dunlin::extract::Apply::ALL,
                extract_tag: "docs",
                transform_tag: "chunks-200",
                index_tag: "main",
                release: Some("release"),
                size: 200,
                overlap: 20,
            },
        )
        .unwrap();
        let cases: Vec<Case> = serde_json::from_str(&instruction("cases.json")).unwrap();
        save_eval_data(&project.store, "default", &cases).unwrap();
        project
    }

    fn indexes(&self) -> PathBuf {
        self.root.path().join(".dunlin/indexes")
    }

    fn request<'a>(&'a self, indexes: &'a Path, instruction: &'a str) -> OptimizeRequest<'a> {
        OptimizeRequest {
            name: "chunk-and-search",
            instruction,
            project: self.root.path(),
            indexes,
            plan_only: false,
        }
    }

    fn optimize(&self, instruction: &str) -> Optimization {
        let indexes = self.indexes();
        optimize(&self.store, &self.request(&indexes, instruction)).unwrap()
    }

    fn plan(&self, instruction: &str) -> Optimization {
        let indexes = self.indexes();
        let request = OptimizeRequest {
            plan_only: true,
            ..self.request(&indexes, instruction)
        };
        optimize(&self.store, &request).unwrap()
    }

    fn refused(&self, instruction: &str) -> String {
        let indexes = self.indexes();
        optimize(&self.store, &self.request(&indexes, instruction))
            .unwrap_err()
            .to_string()
    }

    fn resolve(&self, tag: &str) -> SnapshotId {
        self.store.resolve(tag).unwrap().unwrap()
    }

    /// The runs an optimization started.
    fn started(&self, run: &str) -> Vec<RunRecord> {
        self.store
            .runs()
            .unwrap()
            .into_iter()
            .filter(|record| record.cause.as_deref() == Some(run))
            .collect()
    }
}

fn named<'a>(optimization: &'a Optimization, name: &str) -> &'a Candidate {
    optimization
        .candidates
        .iter()
        .find(|candidate| candidate.name == name)
        .unwrap_or_else(|| panic!("no candidate {name}"))
}

#[test]
fn makes_candidates_builds_and_evaluates_them() {
    let project = Project::new();
    let served = project.resolve("release");
    let chunks = project.resolve("chunks-200");
    let text = instruction("chunk-and-search.yaml");

    // The plan says what each candidate would run, by looking up keys. Nothing is recorded.
    let runs_before = project.store.runs().unwrap().len();
    let plan = project.plan(&text);
    assert!(plan.run.is_none());
    assert_eq!(project.store.runs().unwrap().len(), runs_before);
    assert_eq!(plan.candidates.len(), 13);
    assert_eq!(plan.candidates[0].expected, ["eval"]);
    let planned = named(&plan, "chunking=length(size=80),search=top10");
    assert_eq!(planned.state, State::Planned);
    assert_eq!(planned.expected, ["transform", "index", "eval"]);
    assert_eq!(planned.items, 6);
    assert_eq!(
        named(&plan, "chunking=as-is,search=top1").expected,
        ["eval"]
    );

    let made = project.optimize(&text);
    let run = made.run.clone().unwrap();

    // The base is evaluated first, as it is: nothing is built, and it is the index in use.
    let base = &made.candidates[0];
    assert_eq!(
        (base.name.as_str(), &base.state),
        ("base", &State::Evaluated)
    );
    assert_eq!(base.ran, ["eval"]);
    assert_eq!(base.index, Some(served));
    assert_eq!(base.recall, Some(1.0));
    // The option that changes nothing is the base again.
    assert_eq!(
        named(&made, "chunking=as-is,search=top10").state,
        State::Same(0)
    );

    // Only the search changed: no build, only an evaluation, on the same index.
    let top1 = named(&made, "chunking=as-is,search=top1");
    assert_eq!(top1.ran, ["eval"]);
    assert_eq!(top1.index, Some(served));
    assert_eq!(top1.tag.as_deref(), Some("main"));
    assert!(top1.recall.unwrap() <= base.recall.unwrap());

    // An array of values is one candidate for each value. Two candidates that chunk the same
    // way share the chunks and the index: the second only evaluates.
    let small = named(&made, "chunking=length(size=20),search=top10");
    let small_top1 = named(&made, "chunking=length(size=20),search=top1");
    let mut ran = [small.ran.clone(), small_top1.ran.clone()];
    ran.sort();
    assert_eq!(
        ran,
        [
            vec!["eval".to_owned()],
            vec![
                "transform".to_owned(),
                "index".to_owned(),
                "eval".to_owned()
            ]
        ]
    );
    assert_eq!(small.index, small_top1.index);
    assert_ne!(small.index, Some(served));
    // Chunks of twenty characters hold only the one sentence of evidence that is that short.
    assert_eq!(small.recall, Some(0.2));
    assert_eq!(small_top1.recall, Some(0.0));

    // The module is another one: to the store that is a different processing, like a different
    // argument. The documents are not read again.
    let structure = named(&made, "chunking=structure,search=top10");
    assert_eq!(structure.state, State::Evaluated);
    assert!(structure.config.contains("chunk.structure"));
    let index = project.store.snapshot(structure.index.unwrap()).unwrap();
    // The candidate that only searches it differently has the same index, under its own name.
    assert_eq!(index.tags, ["chunk-and-search/10", "chunk-and-search/11"]);
    assert_eq!(structure.tag.as_deref(), Some("chunk-and-search/10"));
    let structured = project.store.snapshot(index.parents[0]).unwrap();
    assert_eq!(
        structured.parents,
        project.store.snapshot(chunks).unwrap().parents
    );
    let runs = project.started(&run);
    let transform = runs
        .iter()
        .find(|record| record.snapshot == Some(structured.id) && record.stage == "transform")
        .unwrap();
    let info = project
        .store
        .info(transform.info.as_ref().unwrap())
        .unwrap();
    assert_eq!(info["modules"][0]["name"], "chunk.structure");
    assert_eq!(info["modules"][0]["args"]["max_chars"], 200);
    assert!(runs.iter().all(|record| record.stage != "extract"));

    // The configuration of a candidate is a whole file, kept with its runs.
    let kept = String::from_utf8(
        project
            .store
            .bytes(transform.config.as_ref().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(kept, structure.config);
    let settings = Settings::parse(&kept).unwrap();
    assert_eq!(
        settings.section("extract").unwrap().tag.as_deref(),
        Some("docs")
    );
    assert_eq!(settings.section("transform").unwrap().tag, None);
    assert_eq!(settings.section("index").unwrap().tag, structure.tag);

    // What changes together is one option. The vector index is not here, so those candidates
    // are left out before anything is built, with the reason.
    let vector = named(&made, "chunking=length(size=80),search=vector");
    assert_eq!(
        vector.state,
        State::Invalid("index.vector is not on the new store yet.".into())
    );
    assert!(vector.ran.is_empty() && vector.index.is_none());
    assert_eq!(
        made.candidates
            .iter()
            .filter(|candidate| matches!(candidate.state, State::Invalid(_)))
            .count(),
        4
    );
    assert_eq!(
        made.candidates
            .iter()
            .filter(|candidate| candidate.state == State::Evaluated)
            .count(),
        8
    );

    // What is served, and the tags of the base, did not move.
    assert_eq!(project.resolve("release"), served);
    assert_eq!(project.resolve("main"), served);
    assert_eq!(project.resolve("chunks-200"), chunks);

    // The optimization is one run that keeps the instruction and lists the candidates. The runs
    // it started name it.
    let all = project.store.runs().unwrap();
    let record = all.iter().find(|record| record.id == run).unwrap();
    assert_eq!(
        (
            record.stage.as_str(),
            record.state.as_str(),
            record.snapshot
        ),
        ("optimize", "complete", None)
    );
    let kept = project
        .store
        .bytes(record.config.as_ref().unwrap())
        .unwrap();
    assert_eq!(String::from_utf8(kept).unwrap(), text);
    assert_eq!(record.stats["candidates"].as_array().unwrap().len(), 13);
    assert_eq!(record.stats["candidates"][0]["recall"], 1.0);
    assert_eq!(runs.len(), 8 * 3);
    assert!(
        all.iter().filter(|record| record.id != run).all(|record| {
            record.cause.is_none() || record.cause.as_deref() == Some(run.as_str())
        })
    );

    // Two reports are compared case by case, as two versions of documents are.
    let changed = project
        .store
        .diff(base.report.unwrap(), small.report.unwrap())
        .unwrap();
    assert!(changed.updated.contains(&vec!["pump-check".to_owned()]));
    assert_eq!(changed.updated.len() + changed.unchanged, 5);
    assert!(changed.added.is_empty() && changed.removed.is_empty());
    // Only the cases for tuning are used.
    let judged = report(&project.store, base.report.unwrap()).unwrap();
    assert_eq!((judged.cases.len(), judged.found), (5, 5));
    assert!(judged.cases.iter().all(|(id, _)| id != "final-valve"));

    // The same instruction again: every candidate is found by its keys and nothing runs.
    let again = project.optimize(&text);
    assert_ne!(again.run, made.run);
    for (first, second) in made.candidates.iter().zip(&again.candidates) {
        assert!(second.ran.is_empty(), "{} ran again", second.name);
        assert!(second.expected.is_empty());
        assert_eq!(second.state, first.state);
        assert_eq!(
            (second.index, second.report, second.recall),
            (first.index, first.report, first.recall)
        );
    }
    assert!(
        project
            .started(again.run.as_ref().unwrap())
            .iter()
            .all(|record| record.outcome.as_deref() == Some("reused"))
    );

    // The index of a candidate says how it was made.
    let listed = &project.store.snapshot(small.index.unwrap()).unwrap().items;
    let info = InfoId::parse(&listed[0].1).unwrap();
    assert_eq!(
        project.store.info(&info).unwrap()["modules"][0]["name"],
        "index.keyword"
    );
}

#[test]
fn leaves_what_goes_over_the_limit_and_refuses_a_wrong_instruction() {
    let project = Project::new();
    let text = instruction("chunk-and-search.yaml");

    // With a limit, the cheapest candidates run. The others stay in the list, not tried.
    let limited = text.replace("candidates: 20", "candidates: 2");
    let made = project.optimize(&limited);
    let evaluated: Vec<&str> = made
        .candidates
        .iter()
        .filter(|candidate| candidate.state == State::Evaluated)
        .map(|candidate| candidate.name.as_str())
        .collect();
    assert_eq!(
        evaluated,
        [
            "base",
            "chunking=as-is,search=top1",
            "chunking=length(size=20),search=top10"
        ]
    );
    let skipped = named(&made, "chunking=structure,search=top10");
    assert_eq!(
        skipped.state,
        State::Skipped("The limit of 2 candidates was reached.".into())
    );
    assert!(skipped.index.is_none());
    assert!(
        project
            .store
            .resolve("chunk-and-search/7")
            .unwrap()
            .is_none()
    );

    // The whole instruction afterwards runs only what was left.
    let whole = project.optimize(&text);
    assert!(whole.candidates[0].ran.is_empty());
    assert!(named(&whole, "chunking=as-is,search=top1").ran.is_empty());
    assert_eq!(
        named(&whole, "chunking=structure,search=top10").ran,
        ["transform", "index", "eval"]
    );

    // An instruction that is wrong in itself is refused before anything runs.
    let runs = project.store.runs().unwrap().len();
    assert_eq!(
        project.refused(&instruction("two-axes.yaml")),
        "Axes size and overlap both rewrite transform. Put what changes together in one option."
    );
    assert_eq!(
        project.refused(&text.replace("data: default", "data: missing")),
        "There is no evaluation data missing."
    );
    assert_eq!(
        project.refused(&text.replace("as-is: {}", "as-is: { transform: [{ tag: mine }] }")),
        "Option as-is writes a tag. The candidates are named by the optimization."
    );
    assert_eq!(project.store.runs().unwrap().len(), runs);
}
