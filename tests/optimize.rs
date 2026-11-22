//! Optimization on the new store (docs/data-management.md, chapter 11): an instruction makes a
//! configuration file for each trial, and each file is run: built and evaluated.
//!
//! The documents are `tests/fixtures/extract`; the instructions and the evaluation data are
//! `tests/fixtures/optimize`.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use knowledge_nest::assess::{Case, report, save_eval_data};
use knowledge_nest::optimize::{
    Candidate, Event, Optimization, OptimizeRequest, State, latest, optimize, table,
};
use knowledge_nest::pipeline::{RunOutcome, RunRequest, run};
use knowledge_nest::records::{InfoId, ItemId};
use knowledge_nest::settings::Settings;
use knowledge_nest::store::{RunRecord, SnapshotId, Store};

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
    /// A project with six documents, built by running its own configuration file, and
    /// evaluation data.
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        copy_into(&fixtures("extract/original"), &root.path().join("docs"));
        copy_into(&fixtures("extract/additional"), &root.path().join("docs"));
        let config = instruction("KnowledgeNestfile.yaml");
        fs::write(root.path().join("KnowledgeNestfile.yaml"), &config).unwrap();
        let store = Store::open(&root.path().join(".knowledge-nest/store")).unwrap();
        let project = Self { root, store };
        let built = project.run(&config, true);
        assert_eq!(built.ran, ["extract", "transform", "index"]);
        assert_eq!(built.tags, ["docs", "chunks-200", "main"]);
        assert_eq!(built.released.as_deref(), Some("release"));
        assert!(built.report.is_none());
        let cases: Vec<Case> = serde_json::from_str(&instruction("cases.json")).unwrap();
        save_eval_data(&project.store, "default", &cases).unwrap();
        project
    }

    fn indexes(&self) -> PathBuf {
        self.root.path().join(".knowledge-nest/indexes")
    }

    /// Runs a configuration file, as a build of the project or as a trial.
    fn run(&self, config: &str, release: bool) -> RunOutcome {
        run(
            &self.store,
            &RunRequest {
                project: self.root.path(),
                indexes: &self.indexes(),
                config,
                evaluate: true,
                release,
                latest: true,
                cause: None,
                progress: &|_, _| {},
            },
        )
        .unwrap()
    }

    fn optimize_hearing(&self, instruction: &str, progress: &dyn Fn(&Event<'_>)) -> Optimization {
        optimize(
            &self.store,
            &OptimizeRequest {
                name: "chunk-and-search",
                instruction,
                project: self.root.path(),
                indexes: &self.indexes(),
                progress,
            },
        )
        .unwrap()
    }

    fn optimize(&self, instruction: &str) -> Optimization {
        self.optimize_hearing(instruction, &|_| {})
    }

    fn refused(&self, instruction: &str) -> String {
        optimize(
            &self.store,
            &OptimizeRequest {
                name: "chunk-and-search",
                instruction,
                project: self.root.path(),
                indexes: &self.indexes(),
                progress: &|_| {},
            },
        )
        .unwrap_err()
        .to_string()
    }

    fn resolve(&self, kind: &str, tag: &str) -> SnapshotId {
        self.store.resolve(kind, tag).unwrap().unwrap()
    }

    /// The index that is served now.
    fn served(&self) -> SnapshotId {
        *self.store.releases("release").unwrap().last().unwrap()
    }

    /// What `default` names, for each kind that has it.
    fn latest(&self) -> Vec<(String, SnapshotId)> {
        self.store
            .names()
            .unwrap()
            .into_iter()
            .filter(|(_, name, _)| name == "default")
            .map(|(kind, _, snapshot)| (kind, snapshot))
            .collect()
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

    fn file(&self, candidate: &Candidate) -> String {
        fs::read_to_string(self.root.path().join(&candidate.file)).unwrap()
    }
}

fn named<'a>(optimization: &'a Optimization, name: &str) -> &'a Candidate {
    optimization
        .candidates
        .iter()
        .find(|candidate| candidate.name == name)
        .unwrap_or_else(|| panic!("no trial {name}"))
}

fn states(optimization: &Optimization) -> Vec<&str> {
    optimization
        .candidates
        .iter()
        .map(|candidate| match candidate.state {
            State::Pending => "pending",
            State::Evaluated => "evaluated",
            State::Invalid(_) => "invalid",
            State::Failed(_) => "failed",
            State::Skipped(_) => "skipped",
        })
        .collect()
}

#[test]
fn writes_a_file_for_each_trial_and_runs_it() {
    let project = Project::new();
    let served = project.served();
    let chunks = project.resolve("chunks", "chunks-200");
    // What a run by hand made last is also named default, for each kind.
    let defaults = project.latest();
    assert_eq!(
        defaults,
        [
            ("extracted".to_owned(), project.resolve("extracted", "docs")),
            ("chunks".to_owned(), chunks),
            ("index".to_owned(), served),
            (
                "eval-data".to_owned(),
                project.resolve("eval-data", "default")
            ),
        ]
    );
    let text = instruction("chunk-and-search.yaml");

    let heard = RefCell::new(Vec::new());
    let made = project.optimize_hearing(&text, &|event| {
        heard.borrow_mut().push(match event {
            Event::Trial {
                number,
                position,
                total,
                ..
            } => format!("trial {number} ({position}/{total})"),
            Event::Stage { stage, outcome } => {
                format!(
                    "{stage} {}",
                    outcome.map_or("starts", |outcome| outcome.as_str())
                )
            }
            Event::Done { number, recall, .. } => format!("done {number} {recall:?}"),
        });
    });
    assert_eq!(made.candidates.len(), 13);

    // What is going on is said as it goes: the trial, each stage as it starts and ends, and the
    // result. The base is the first of the nine trials that can run.
    let heard = heard.into_inner();
    assert_eq!(
        heard[..8],
        [
            "trial 0 (1/9)",
            "transform starts",
            "transform reused",
            "index starts",
            "index reused",
            "eval starts",
            "eval created",
            "done 0 Some(1.0)",
        ]
    );
    assert_eq!(
        heard
            .iter()
            .filter(|line| line.starts_with("trial"))
            .count(),
        9
    );

    // The base is tried first, as it is: nothing is built, and it is the index in use.
    let base = &made.candidates[0];
    assert_eq!(
        (base.name.as_str(), &base.state),
        ("base", &State::Evaluated)
    );
    assert_eq!(base.ran, ["eval"]);
    assert_eq!(base.index, Some(served));
    assert_eq!(base.recall, Some(1.0));
    assert_eq!(base.tag, "chunk-and-search/000");
    assert_eq!(base.named, ["eval-report"]);
    assert_eq!(
        Some(project.resolve("eval-report", "chunk-and-search/000")),
        base.report
    );
    // The option that changes nothing is the base again: everything is found by its key.
    let same = named(&made, "chunking=as-is,search=top10");
    assert!(same.ran.is_empty());
    assert_eq!((same.index, same.report), (base.index, base.report));

    // Only the search changed: no build, only an evaluation, on the same index.
    let top1 = named(&made, "chunking=as-is,search=top1");
    assert_eq!(top1.ran, ["eval"]);
    assert_eq!(top1.index, Some(served));
    assert_eq!(top1.named, ["eval-report"]);
    assert!(top1.recall.unwrap() <= base.recall.unwrap());

    // An array of values is one trial for each value. A trial tags what it makes itself, at
    // each stage. Two trials that chunk the same way share the chunks and the index: the
    // second only evaluates, and its tags are on the same snapshots.
    let small = named(&made, "chunking=length(size=20),search=top10");
    let small_top1 = named(&made, "chunking=length(size=20),search=top1");
    assert_eq!((small.number, small_top1.number), (4, 5));
    assert_eq!(small.ran, ["transform", "index", "eval"]);
    assert_eq!(small_top1.ran, ["eval"]);
    assert_eq!(small.tag, "chunk-and-search/004");
    assert_eq!(small.named, ["chunks", "index", "eval-report"]);
    assert_eq!(small.index, small_top1.index);
    assert_ne!(small.index, Some(served));
    let index = project.store.snapshot(small.index.unwrap()).unwrap();
    assert_eq!(index.tags, ["chunk-and-search/004", "chunk-and-search/005"]);
    assert_eq!(
        project.store.snapshot(index.parents[0]).unwrap().tags,
        ["chunk-and-search/004", "chunk-and-search/005"]
    );
    // Chunks of twenty characters hold only the one sentence of evidence that is that short.
    assert_eq!(small.recall, Some(0.2));
    assert_eq!(small_top1.recall, Some(0.0));

    // The module is another one: to the store that is a different processing, like a different
    // argument. The documents are not read again.
    let structure = named(&made, "chunking=structure,search=top10");
    assert_eq!(structure.state, State::Evaluated);
    let index = project.store.snapshot(structure.index.unwrap()).unwrap();
    let structured = project.store.snapshot(index.parents[0]).unwrap();
    assert_eq!(
        structured.parents,
        project.store.snapshot(chunks).unwrap().parents
    );
    let runs = project.started(&made.run);
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

    // The configuration of a trial is a whole file with a number, in the folder of the
    // optimization. It is also kept with the runs it started.
    assert_eq!(
        structure.file,
        ".knowledge-nest/trials/chunk-and-search/010.yaml"
    );
    assert_eq!(project.file(structure), structure.config);
    let kept = project
        .store
        .bytes(transform.config.as_ref().unwrap())
        .unwrap();
    assert_eq!(String::from_utf8(kept).unwrap(), structure.config);
    let settings = Settings::parse(&structure.config).unwrap();
    // The documents are the ones the base's name points at: the folder is not scanned.
    assert_eq!(
        settings.section("extract").unwrap().from.as_deref(),
        Some("docs")
    );
    assert_eq!(
        settings.section("transform").unwrap().steps[0].name,
        "chunk.structure"
    );
    assert_eq!(
        settings.section("transform").unwrap().tag.as_deref(),
        Some("chunk-and-search/010")
    );
    assert_eq!(
        settings.section("index").unwrap().tag.as_deref(),
        Some("chunk-and-search/010")
    );
    // Every trial is measured the same way, and none releases.
    let plan = settings.plan().unwrap();
    let eval = plan.eval.unwrap();
    assert_eq!(
        (eval.data.as_str(), eval.split.as_str()),
        ("default", "dev")
    );
    assert!(plan.release.is_none());
    // A trial that only searches differently keeps the base's modules, without its tags.
    let settings = Settings::parse(&top1.config).unwrap();
    let transform_section = settings.section("transform").unwrap();
    assert_eq!(transform_section.steps[0].name, "chunk.length");
    assert_eq!(transform_section.tag, None);
    assert_eq!(settings.section("index").unwrap().tag, None);

    // The optimization left default where it was: a trial names only what it makes itself.
    assert_eq!(project.latest(), defaults);
    assert_eq!(
        project.store.snapshot(served).unwrap().tags,
        ["main", "default"]
    );

    // Running the file of a trial by hand is the trial again. Nothing runs, and what it made
    // is now what was made last by hand, so default names it.
    let again = project.run(&project.file(structure), false);
    assert_eq!(Some(project.resolve("index", "default")), structure.index);
    assert_eq!(
        Some(project.resolve("eval-report", "default")),
        structure.report
    );
    assert!(again.ran.is_empty());
    assert_eq!(
        (Some(again.index), again.report),
        (structure.index, structure.report)
    );
    assert_eq!(again.result.unwrap().recall, structure.recall.unwrap());
    assert!(again.released.is_none());

    // What changes together is one option. The vector index is not here, so those trials are
    // left out before anything is built, with the reason. Their files are written all the same.
    let vector = named(&made, "chunking=length(size=80),search=vector");
    assert_eq!(
        vector.state,
        State::Invalid("index.vector is not on the new store yet.".into())
    );
    assert!(vector.ran.is_empty() && vector.index.is_none() && vector.named.is_empty());
    assert!(project.file(vector).contains("index.vector"));
    assert_eq!(
        states(&made),
        [
            "evaluated",
            "evaluated",
            "evaluated",
            "invalid",
            "evaluated",
            "evaluated",
            "invalid",
            "evaluated",
            "evaluated",
            "invalid",
            "evaluated",
            "evaluated",
            "invalid"
        ]
    );

    // What is served, and the tags of the base, did not move.
    assert_eq!(project.served(), served);
    assert_eq!(project.resolve("index", "main"), served);
    assert_eq!(project.resolve("chunks", "chunks-200"), chunks);

    // The optimization is one run that keeps the instruction and lists the trials. The runs it
    // started name it.
    let all = project.store.runs().unwrap();
    let record = all.iter().find(|record| record.id == made.run).unwrap();
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
    assert_eq!(
        record.stats["candidates"][10]["file"],
        ".knowledge-nest/trials/chunk-and-search/010.yaml"
    );
    assert_eq!(runs.len(), 9 * 3);
    assert!(runs.iter().all(|record| record.state == "complete"));

    // The results are read as a table: the best first, then the trials that were not
    // evaluated, with the reason.
    let lines: Vec<String> = table(&made.record).lines().map(str::to_owned).collect();
    assert_eq!(
        lines[0],
        "Optimization chunk-and-search (recall on default, dev)"
    );
    assert!(lines[1].starts_with("Trial  Recall  To base  Ran"));
    assert!(lines[2].starts_with("000    1.000   -        eval"));
    assert!(lines[2].ends_with("base"));
    assert!(lines[3].starts_with("001    1.000   +0.000   nothing"));
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("005    0.000   -1.000   eval"))
    );
    assert!(lines[14].ends_with(
        "chunking=structure,search=vector (invalid: index.vector is not on the new store yet.)"
    ));
    assert_eq!(
        lines[15],
        "The files of the trials are in .knowledge-nest/trials/chunk-and-search."
    );
    // Every trial starts from the same documents, and the table says which.
    assert_eq!(
        lines[16],
        format!(
            "Every trial used the documents docs, version {} (6 documents).",
            project.resolve("extracted", "docs")
        )
    );
    // The latest optimization of a name is found again, for showing its results later.
    let (kept, running) = latest(&project.store, Some("chunk-and-search"))
        .unwrap()
        .unwrap();
    assert!(!running);
    assert_eq!(table(&kept), table(&made.record));
    assert_eq!(latest(&project.store, Some("another")).unwrap(), None);

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

    // The same instruction again: every trial is found by its keys and nothing runs.
    let again = project.optimize(&text);
    assert_ne!(again.run, made.run);
    for (first, second) in made.candidates.iter().zip(&again.candidates) {
        assert!(second.ran.is_empty(), "{} ran again", second.name);
        assert_eq!(second.state, first.state);
        assert_eq!(
            (second.index, second.report, second.recall),
            (first.index, first.report, first.recall)
        );
    }
    assert!(
        project
            .started(&again.run)
            .iter()
            .all(|record| record.outcome.as_deref() == Some("reused"))
    );

    // The index of a trial says how it was made.
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

    // With a limit, the first trials of the instruction run. The others stay in the list.
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
            "chunking=as-is,search=top10",
            "chunking=as-is,search=top1"
        ]
    );
    let skipped = named(&made, "chunking=structure,search=top10");
    assert_eq!(
        skipped.state,
        State::Skipped("The limit of 2 candidates was reached.".into())
    );
    assert!(skipped.index.is_none() && skipped.named.is_empty());
    assert!(
        project
            .store
            .resolve("index", "chunk-and-search/010")
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
        "Option as-is writes a tag. The trials are named by the optimization."
    );
    assert_eq!(
        project.refused(&text.replace(
            "as-is: {}",
            "as-is: { eval: [{ eval.evidence: { data: other } }] }"
        )),
        "Option as-is rewrites eval. Every trial is measured the same way."
    );
    // Neither the instruction nor the base says what to measure with.
    assert_eq!(
        project.refused(&text.replace("    data: default\n", "")),
        "The instruction and the base configuration name no evaluation data."
    );
    assert_eq!(project.store.runs().unwrap().len(), runs);

    // The base configuration can name the evaluation data instead of the instruction.
    let config =
        instruction("KnowledgeNestfile.yaml") + "\neval:\n  - eval.evidence: { data: default }\n";
    fs::write(project.root.path().join("KnowledgeNestfile.yaml"), config).unwrap();
    let from_base = project.optimize(&text.replace("    data: default\n", ""));
    assert_eq!(from_base.candidates[0].recall, Some(1.0));
    assert!(from_base.candidates[0].ran.is_empty());
}

#[test]
fn goes_on_after_a_trial_that_fails() {
    let project = Project::new();
    // The text of one document is lost from the store. A trial that chunks the documents anew
    // fails; one that uses the chunks that are there does not read the text.
    let documents = project
        .store
        .snapshot(project.resolve("extracted", "docs"))
        .unwrap();
    let item = ItemId::parse(&documents.items[0].1).unwrap();
    let lost = project.store.item(&item).unwrap()[0].object.clone();
    rusqlite::Connection::open(
        project
            .root
            .path()
            .join(".knowledge-nest/store/objects.sqlite"),
    )
    .unwrap()
    .execute("DELETE FROM objects WHERE id = ?", [lost.as_str()])
    .unwrap();

    let text = "optimize:
  base: KnowledgeNestfile.yaml
  vary:
    - name: search
      options:
        top10: {}
        top1:
          search:
            - retrieval.keyword: { top_k: 1 }
    - name: chunking
      options:
        as-is: {}
        length:
          transform:
            - chunk.length: { size: SIZES, overlap: 0 }
  eval:
    data: default
";
    let made = project.optimize(&text.replace("SIZES", "80"));
    assert_eq!(
        states(&made),
        ["evaluated", "evaluated", "failed", "evaluated", "failed"]
    );
    let failed = named(&made, "search=top10,chunking=length");
    assert!(matches!(&failed.state, State::Failed(reason) if !reason.is_empty()));
    assert!(failed.named.is_empty() && failed.recall.is_none() && failed.elapsed_ms.is_some());
    // The trial after the one that failed ran as usual.
    assert_eq!(named(&made, "search=top1,chunking=as-is").ran, ["eval"]);
    // The optimization is complete, and the run that failed is recorded as failed.
    let all = project.store.runs().unwrap();
    let record = all.iter().find(|record| record.id == made.run).unwrap();
    assert_eq!(record.state, "complete");
    assert_eq!(record.stats["candidates"][2]["state"], "failed");
    assert_eq!(
        project
            .started(&made.run)
            .iter()
            .filter(|record| record.state == "failed" && record.stage == "transform")
            .count(),
        2
    );
    assert!(table(&made.record).contains("search=top10,chunking=length (failed: "));

    // When three trials fail one after the other, the rest are not tried: what fails every
    // time is not a matter of the trial.
    let only_chunking = text
        .replace(
            "        top1:\n          search:\n            - retrieval.keyword: { top_k: 1 }\n",
            "",
        )
        .replace("        as-is: {}\n", "")
        .replace("SIZES", "[30, 40, 50, 60]");
    let made = project.optimize(&only_chunking);
    assert_eq!(
        states(&made),
        ["evaluated", "failed", "failed", "failed", "skipped"]
    );
    assert_eq!(
        made.candidates[4].state,
        State::Skipped("3 trials in a row failed, so the rest were not tried.".into())
    );
}

#[test]
fn says_what_a_scan_found_and_checks_the_evaluation_before_it_builds() {
    let project = Project::new();
    let config = instruction("KnowledgeNestfile.yaml");
    let first = project.served();
    assert_eq!(
        project.store.served().unwrap(),
        [("release".to_owned(), first)]
    );

    // A run says which documents the scan found changed, and how many were chunked.
    fs::write(project.root.path().join("docs/new.md"), "新しい文書です。").unwrap();
    let made = project.run(&config, false);
    let scan = made.scan.as_ref().unwrap();
    assert_eq!(scan.added, ["new.md"]);
    assert!(scan.updated.is_empty() && scan.deleted.is_empty());
    assert_eq!(scan.unchanged, 6);
    assert_eq!(made.chunked, Some(1));

    // What is served stays until an index is released, and the one before is kept.
    assert_eq!(project.served(), first);
    project.store.release("release", made.index).unwrap();
    project.store.release("release", made.index).unwrap();
    assert_eq!(
        project.store.releases("release").unwrap(),
        [first, made.index]
    );
    assert!(
        project
            .store
            .release("release", made.chunks.unwrap())
            .is_err()
    );

    // Evaluation data that is not there stops the run before anything is built.
    fs::write(
        project.root.path().join("docs/more.md"),
        "もう一つの文書です。",
    )
    .unwrap();
    let runs = project.store.runs().unwrap().len();
    let error = run(
        &project.store,
        &RunRequest {
            project: project.root.path(),
            indexes: &project.indexes(),
            config: &format!("{config}\neval:\n  - eval.evidence: {{ data: absent }}\n"),
            evaluate: true,
            release: false,
            latest: true,
            cause: None,
            progress: &|_, _| {},
        },
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "There is no evaluation data absent.");
    assert_eq!(project.store.runs().unwrap().len(), runs);
}
