//! A build on the new store from documents to search (docs/data-management.md): a RAG is made,
//! documents are added, and the next RAG is made.
//!
//! `tests/fixtures/extract/original` is the folder at the start; `additional` is copied in later.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use dunlin::index::{Found, search};
use dunlin::pipeline::{BuildOutcome, BuildRequest, build};
use dunlin::records::InfoId;
use dunlin::store::{Counts, Outcome, SnapshotId, Store};

const CONFIG: &str = "extract:\n  - source.filesystem: { path: ./docs }\n  - loader.text: {}\n  - tag: docs\n\ntransform:\n  - chunk.length: { unit: characters, size: 200, overlap: 20 }\n  - tag: chunks-200\n\nindex:\n  - index.keyword: { tokenizer: lindera }\n  - tag: main\n\nserve:\n  tag: release\n";

/// A trial that only changes the size of chunks, under tags of its own.
const TRIAL: &str = "extract:\n  - source.filesystem: { path: ./docs }\n  - loader.text: {}\n  - tag: docs\n\ntransform:\n  - chunk.length: { unit: characters, size: 80, overlap: 10 }\n  - tag: chunks-80\n\nindex:\n  - index.keyword: { tokenizer: lindera }\n  - tag: trial-80\n";

fn fixtures(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/extract")
        .join(name)
}

/// Copies a folder into another and gives every copied file a time `age` before now.
fn copy_into(from: &Path, to: &Path, age: Duration) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_into(&entry.path(), &target, age);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            fs::File::options()
                .write(true)
                .open(&target)
                .unwrap()
                .set_modified(SystemTime::now() - age)
                .unwrap();
        }
    }
}

struct Project {
    root: tempfile::TempDir,
    store: Store,
}

impl Project {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        copy_into(
            &fixtures("original"),
            &root.path().join("docs"),
            Duration::from_secs(3600),
        );
        let store = Store::open(&root.path().join(".dunlin/store")).unwrap();
        Self { root, store }
    }

    fn docs(&self) -> PathBuf {
        self.root.path().join("docs")
    }

    fn indexes(&self) -> PathBuf {
        self.root.path().join(".dunlin/indexes")
    }

    /// A build with the project's own configuration: it also moves what is served.
    fn build(&self) -> BuildOutcome {
        build(
            &self.store,
            &BuildRequest {
                source: "docs",
                root: &self.docs(),
                indexes: &self.indexes(),
                config: Some(CONFIG.as_bytes()),
                apply: dunlin::extract::Apply::ALL,
                extract_tag: "docs",
                transform_tag: "chunks-200",
                index_tag: "main",
                release: Some("release"),
                size: 200,
                overlap: 20,
            },
        )
        .unwrap()
    }

    /// A build with the trial configuration: what is served stays.
    fn build_trial(&self) -> BuildOutcome {
        build(
            &self.store,
            &BuildRequest {
                source: "docs",
                root: &self.docs(),
                indexes: &self.indexes(),
                config: Some(TRIAL.as_bytes()),
                apply: dunlin::extract::Apply::ALL,
                extract_tag: "docs",
                transform_tag: "chunks-80",
                index_tag: "trial-80",
                release: None,
                size: 80,
                overlap: 10,
            },
        )
        .unwrap()
    }

    fn search(&self, index: &str, query: &str) -> Vec<Found> {
        search(&self.store, &self.indexes(), index, query, 10).unwrap()
    }

    /// The documents a search finds, without repeats, in the order they are found.
    fn found(&self, index: &str, query: &str) -> Vec<String> {
        let mut documents = Vec::new();
        for found in self.search(index, query) {
            if !documents.contains(&found.document) {
                documents.push(found.document);
            }
        }
        documents
    }

    fn resolve(&self, tag: &str) -> SnapshotId {
        self.store.resolve(tag).unwrap().unwrap()
    }
}

#[test]
fn makes_a_rag_then_the_next_one_after_documents_are_added() {
    let project = Project::new();

    // The first RAG: four documents are read, chunked and indexed.
    let first = project.build();
    assert_eq!(first.extract.added.len(), 4);
    assert_eq!(first.extract.read, 4);
    assert_eq!(first.transform.chunks.outcome, Outcome::Created);
    assert_eq!(first.transform.chunked, 4);
    assert_eq!(first.index.index.outcome, Outcome::Created);
    assert!(first.index.built);
    let rag_1 = first.index.index.snapshot;
    assert_eq!(project.resolve("main"), rag_1);
    assert_eq!(project.resolve("release"), rag_1);

    // Each stage made its snapshot from the one before it.
    let parents = |id| project.store.snapshot(id).unwrap().parents;
    assert_eq!(parents(rag_1), [first.transform.chunks.snapshot]);
    assert_eq!(
        parents(first.transform.chunks.snapshot),
        [first.extract.extracted.snapshot]
    );
    assert_eq!(
        parents(first.extract.extracted.snapshot),
        [first.extract.files.snapshot]
    );

    // It answers from the documents it has, with the place and the text of what it found.
    let found = project.search("release", "ポンプの点検");
    assert_eq!(found[0].document, "manuals/pump.md");
    assert_eq!(found[0].source, "docs");
    assert!(
        found[0]
            .text
            .contains("ポンプの点検は毎月最初の営業日に行う。")
    );
    assert!(found[0].lines.0 >= 1 && found[0].lines.1 >= found[0].lines.0);
    // The best match comes first; "交換" is in the pump manual too.
    assert_eq!(
        project.found("release", "フィルター 交換")[0],
        "manuals/フィルター.md"
    );
    // What is only in the documents that come later is not found yet.
    assert!(project.found("release", "排水").is_empty());
    // "更新" is in other documents too, so something is found, but not the notes.
    assert!(
        !project
            .found("release", "更新履歴")
            .contains(&"release-notes.md".to_owned())
    );

    // Documents are added and the next RAG is made. Only the two new documents are read and
    // chunked; the index is a new version.
    copy_into(
        &fixtures("additional"),
        &project.docs(),
        Duration::from_secs(1800),
    );
    let second = project.build();
    assert_eq!(
        second.extract.added,
        ["manuals/valve.md", "release-notes.md"]
    );
    assert_eq!((second.extract.hashed, second.extract.read), (2, 2));
    assert_eq!(second.transform.chunks.outcome, Outcome::Created);
    assert_eq!(second.transform.chunked, 2);
    assert_eq!(
        second.transform.counts,
        Counts {
            added: 2,
            updated: 0,
            deleted: 0,
            unchanged: 4
        }
    );
    assert_eq!(second.index.index.outcome, Outcome::Created);
    assert!(second.index.built);
    let rag_2 = second.index.index.snapshot;
    assert_ne!(rag_2, rag_1);
    assert_ne!(second.index.location, first.index.location);
    assert_eq!(project.resolve("release"), rag_2);
    assert_eq!(project.store.history("release").unwrap(), [rag_1, rag_2]);

    // The next RAG answers from the added documents too.
    assert_eq!(project.found("release", "排水"), ["manuals/valve.md"]);
    assert_eq!(project.found("release", "更新履歴")[0], "release-notes.md");
    assert_eq!(
        project.search("release", "ポンプの点検")[0].document,
        "manuals/pump.md"
    );

    // The first RAG is still there under its number, and answers as it did.
    let earlier = rag_1.to_string();
    assert!(project.found(&earlier, "排水").is_empty());
    assert_eq!(
        project.search(&earlier, "ポンプの点検")[0].document,
        "manuals/pump.md"
    );

    // Nothing changed: no file is opened, nothing is chunked and no index is built.
    let third = project.build();
    assert_eq!((third.extract.hashed, third.extract.read), (0, 0));
    assert_eq!(third.extract.extracted.outcome, Outcome::Reused);
    assert_eq!(third.transform.chunks.outcome, Outcome::Reused);
    assert_eq!(third.transform.chunked, 0);
    assert_eq!(third.index.index.outcome, Outcome::Reused);
    assert!(!third.index.built);
    assert_eq!(third.index.index.snapshot, rag_2);
    assert_eq!(third.index.location, second.index.location);
    assert_eq!(project.store.history("release").unwrap().len(), 2);
}

#[test]
fn runs_only_what_follows_a_changed_section() {
    let project = Project::new();
    copy_into(
        &fixtures("additional"),
        &project.docs(),
        Duration::from_secs(1800),
    );
    let main = project.build();
    let served = main.index.index.snapshot;

    // A trial changes the size of chunks. The documents are not read again; every document is
    // chunked again and a new index is built, under the trial's own tags.
    let trial = project.build_trial();
    assert_eq!((trial.extract.hashed, trial.extract.read), (0, 0));
    assert_eq!(trial.extract.extracted.outcome, Outcome::Reused);
    assert_eq!(
        trial.extract.extracted.snapshot,
        main.extract.extracted.snapshot
    );
    assert_eq!(trial.transform.chunks.outcome, Outcome::Created);
    assert_eq!(trial.transform.chunked, 6);
    assert_eq!(trial.index.index.outcome, Outcome::Created);
    assert!(trial.index.built);
    let tried = trial.index.index.snapshot;
    assert_ne!(tried, served);

    // Both grow from the same documents, and what is served did not move.
    let parents = |id| project.store.snapshot(id).unwrap().parents;
    assert_eq!(
        parents(trial.transform.chunks.snapshot),
        parents(main.transform.chunks.snapshot)
    );
    assert_eq!(project.resolve("release"), served);
    assert_eq!(project.resolve("main"), served);
    assert_eq!(project.resolve("trial-80"), tried);
    assert_eq!(
        project.resolve("chunks-200"),
        main.transform.chunks.snapshot
    );
    assert_eq!(
        project.resolve("chunks-80"),
        trial.transform.chunks.snapshot
    );

    // Both answer; the trial's chunks are smaller.
    let longest = |index: &str| {
        project
            .search(index, "バルブ")
            .iter()
            .map(|found| found.text.chars().count())
            .max()
            .expect("something found")
    };
    assert!(longest("trial-80") <= 80);
    assert!(longest("main") > longest("trial-80"));

    // Each run says which configuration and which modules made its result.
    let runs = project.store.runs().unwrap();
    let made = |snapshot: SnapshotId, stage: &str| {
        runs.iter()
            .find(|run| run.snapshot == Some(snapshot) && run.stage == stage)
            .unwrap_or_else(|| panic!("no {stage} run made {snapshot}"))
    };
    let config = |snapshot, stage| {
        let config = made(snapshot, stage).config.clone().unwrap();
        String::from_utf8(project.store.bytes(&config).unwrap()).unwrap()
    };
    assert_eq!(config(served, "index"), CONFIG);
    assert_eq!(config(tried, "index"), TRIAL);
    let info = made(trial.transform.chunks.snapshot, "transform")
        .info
        .clone()
        .unwrap();
    assert_eq!(
        project.store.info(&info).unwrap()["modules"][0]["args"]["size"],
        80
    );

    // The index says what it is, and where it was built is a note on that.
    let listed = project.store.snapshot(tried).unwrap().items;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, ["keyword"]);
    let info = InfoId::parse(&listed[0].1).unwrap();
    let description = project.store.info(&info).unwrap();
    assert_eq!(description["modules"][0]["name"], "index.keyword");
    let notes = project.store.object_notes(info.as_str()).unwrap();
    assert_eq!(notes["built"]["generation"], 1);
    assert_eq!(notes["built"]["documents"], 6);

    // The index directory is lost: the next build finds the snapshot by its key and makes only
    // the directory again. The snapshot stays.
    fs::remove_dir_all(&trial.index.location).unwrap();
    let again = project.build_trial();
    assert_eq!(again.transform.chunked, 0);
    assert_eq!(again.index.index.outcome, Outcome::Reused);
    assert_eq!(again.index.index.snapshot, tried);
    assert!(again.index.built);
    assert_ne!(again.index.location, trial.index.location);
    let notes = project.store.object_notes(info.as_str()).unwrap();
    assert_eq!(notes["built"]["generation"], 2);
    assert_eq!(project.found("trial-80", "排水"), ["manuals/valve.md"]);
}
