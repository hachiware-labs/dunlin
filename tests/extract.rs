//! `extract` on the new store (docs/data-management.md): a folder is extracted, documents are
//! copied in and it is extracted again.
//!
//! `tests/fixtures/extract/original` is the folder at the start; `additional` is copied in later.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use knowledge_nest::extract::{Apply, ExtractOutcome, ExtractRequest, Withheld, extract};
use knowledge_nest::records::{ItemId, Location, object_id};
use knowledge_nest::store::{Counts, Outcome, SnapshotId, Store};

fn fixtures(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/extract")
        .join(name)
}

/// Copies a folder into another and gives every copied file a time `age` before now, so that a
/// test does not depend on how the file system keeps times on a copy.
fn copy_into(from: &Path, to: &Path, age: Duration) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_into(&entry.path(), &target, age);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            set_age(&target, age);
        }
    }
}

fn set_age(file: &Path, age: Duration) {
    fs::File::options()
        .write(true)
        .open(file)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

struct Project {
    _root: tempfile::TempDir,
    docs: PathBuf,
    store: Store,
}

impl Project {
    /// A project whose `docs` folder holds the original documents.
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join("docs");
        copy_into(&fixtures("original"), &docs, Duration::from_secs(3600));
        let store = Store::open(&root.path().join(".knowledge-nest/store")).unwrap();
        Self {
            _root: root,
            docs,
            store,
        }
    }

    fn extract(&self) -> ExtractOutcome {
        self.extract_from(&self.docs)
    }

    fn extract_from(&self, root: &Path) -> ExtractOutcome {
        self.run(root, Apply::ALL)
    }

    /// Extracts, taking in only some of the changes of the folder.
    fn extract_with(&self, apply: Apply) -> ExtractOutcome {
        self.run(&self.docs, apply)
    }

    fn run(&self, root: &Path, apply: Apply) -> ExtractOutcome {
        extract(
            &self.store,
            &ExtractRequest {
                source: "docs",
                root,
                tag: Some("docs"),
                config: Some(CONFIG.as_bytes()),
                apply,
                cause: None,
            },
        )
        .unwrap()
    }

    /// The documents of a snapshot with the ID of what each holds.
    fn entries(&self, id: SnapshotId) -> Vec<(String, String)> {
        self.store
            .snapshot(id)
            .unwrap()
            .items
            .into_iter()
            .map(|(name, item)| (name[1].clone(), item))
            .collect()
    }

    fn documents(&self, id: SnapshotId) -> Vec<String> {
        self.entries(id).into_iter().map(|(name, _)| name).collect()
    }

    /// The text of a document as a snapshot holds it, with the lines and bytes it covers.
    fn text(&self, id: SnapshotId, document: &str) -> (String, Option<Location>) {
        let entries = self.entries(id);
        let (_, item) = entries
            .iter()
            .find(|(name, _)| name == document)
            .unwrap_or_else(|| panic!("{document} is not in the snapshot"));
        let parts = self.store.item(&ItemId::parse(item).unwrap()).unwrap();
        assert_eq!(parts.len(), 1, "a text document is one part");
        assert_eq!(parts[0].kind, "text");
        let bytes = self.store.bytes(&parts[0].object).unwrap();
        // An object is named by the hash of its bytes.
        assert_eq!(object_id(&bytes), parts[0].object);
        (String::from_utf8(bytes).unwrap(), parts[0].location.clone())
    }
}

/// The configuration file of the project, as a build would pass it.
const CONFIG: &str = "extract:
  - source.filesystem: { path: ./docs }
  - loader.text: {}
  - tag: docs
";

const ORIGINAL: [&str; 4] = [
    "faq.txt",
    "guide.md",
    "manuals/pump.md",
    "manuals/フィルター.md",
];
const ADDED: [&str; 2] = ["manuals/valve.md", "release-notes.md"];

#[test]
fn adds_the_documents_copied_into_the_folder() {
    let project = Project::new();

    // The original documents become the first version.
    let first = project.extract();
    assert_eq!(first.files.outcome, Outcome::Created);
    assert_eq!(first.extracted.outcome, Outcome::Created);
    assert_eq!(first.added, ORIGINAL);
    assert!(first.updated.is_empty() && first.deleted.is_empty());
    assert_eq!((first.unchanged, first.hashed, first.read), (0, 4, 4));
    assert!(first.skipped.is_empty() && first.substituted.is_empty());
    let version_1 = first.extracted.snapshot;
    let snapshot = project.store.snapshot(version_1).unwrap();
    assert_eq!(snapshot.kind, "extracted");
    // A snapshot names what it was made from; nothing is written to the parent.
    assert_eq!(snapshot.parents, [first.files.snapshot]);
    assert_eq!(snapshot.tags, ["docs"]);
    let files = project.store.snapshot(first.files.snapshot).unwrap();
    assert_eq!(files.kind, "files");
    assert!(files.parents.is_empty() && files.tags.is_empty());
    let listed = project.entries(version_1);
    assert_eq!(project.documents(version_1), ORIGINAL);
    let original = fs::read_to_string(fixtures("original/guide.md")).unwrap();
    let (text, location) = project.text(version_1, "guide.md");
    assert_eq!(text, original);
    assert_eq!(
        location,
        Some(Location::TextRange {
            lines: (1, original.lines().count() as u64),
            bytes: (0, original.len() as u64),
        })
    );
    // The file list holds the same object as the text: a text file is its text.
    let guide = files.items.iter().find(|(name, _)| name[1] == "guide.md");
    assert_eq!(
        guide.map(|(_, id)| id.as_str()),
        Some(object_id(original.as_bytes()).as_str())
    );
    assert_eq!(
        project.store.resolve("extracted", "docs").unwrap(),
        Some(version_1)
    );
    assert_eq!(
        project.store.history("extracted", "docs").unwrap(),
        [version_1]
    );

    // Documents are copied into the folder and it is extracted again: they are added.
    copy_into(
        &fixtures("additional"),
        &project.docs,
        Duration::from_secs(1800),
    );
    let second = project.extract();
    assert_eq!(second.files.outcome, Outcome::Created);
    assert_eq!(second.extracted.outcome, Outcome::Created);
    assert_eq!(second.added, ADDED);
    assert!(second.updated.is_empty() && second.deleted.is_empty());
    // Only the two new files are opened; the four others are neither hashed nor read.
    assert_eq!((second.unchanged, second.hashed, second.read), (4, 2, 2));
    let version_2 = second.extracted.snapshot;
    assert_ne!(version_2, version_1);
    assert_ne!(second.files.snapshot, first.files.snapshot);
    let now = project.entries(version_2);
    assert_eq!(now.len(), 6);
    // The documents that did not change are the same items as before.
    for entry in &listed {
        assert!(now.contains(entry), "{} changed", entry.0);
    }
    let (text, _) = project.text(version_2, "manuals/valve.md");
    assert_eq!(
        text,
        fs::read_to_string(fixtures("additional/manuals/valve.md")).unwrap()
    );
    let diff = project.store.diff(version_1, version_2).unwrap();
    assert_eq!(
        diff.added,
        ADDED.map(|document| vec!["docs".to_owned(), document.to_owned()])
    );
    assert!(diff.updated.is_empty() && diff.removed.is_empty());
    assert_eq!(diff.unchanged, 4);

    // The tag moved to the new version, and the first version stays as it was.
    assert_eq!(
        project.store.resolve("extracted", "docs").unwrap(),
        Some(version_2)
    );
    assert_eq!(
        project.store.history("extracted", "docs").unwrap(),
        [version_1, version_2]
    );
    let earlier = project.store.snapshot(version_1).unwrap();
    assert!(earlier.tags.is_empty());
    assert_eq!(earlier.parents, [first.files.snapshot]);
    assert_eq!(project.entries(version_1), listed);
    assert_eq!(project.text(version_1, "guide.md").0, original);
    assert_eq!(
        project.store.snapshot(version_2).unwrap().parents,
        [second.files.snapshot]
    );

    // Nothing changed: no file is opened and no version is made.
    let third = project.extract();
    assert_eq!(third.files.outcome, Outcome::Unchanged);
    assert_eq!(third.extracted.outcome, Outcome::Reused);
    assert_eq!(third.files.snapshot, second.files.snapshot);
    assert_eq!(third.extracted.snapshot, version_2);
    assert!(third.added.is_empty() && third.updated.is_empty() && third.deleted.is_empty());
    assert_eq!((third.unchanged, third.hashed, third.read), (6, 0, 0));
    assert_eq!(project.store.history("extracted", "docs").unwrap().len(), 2);

    // Every run is recorded with its times and counts, also the ones that made nothing.
    let runs = project.store.runs().unwrap();
    assert_eq!(
        runs.iter()
            .map(|run| run.outcome.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>(),
        [
            "created",
            "created",
            "created",
            "created",
            "unchanged",
            "reused"
        ]
    );
    for run in &runs {
        assert_eq!(
            (run.stage.as_str(), run.state.as_str()),
            ("extract", "complete")
        );
        assert!(!run.degraded);
        assert!(
            run.completed_at
                .is_some_and(|completed| completed >= run.started_at)
        );
    }
    assert_eq!(runs[3].tag.as_deref(), Some("docs"));
    assert_eq!(runs[3].inputs, [second.files.snapshot]);
    assert_eq!(
        runs[3].counts,
        Some(Counts {
            added: 2,
            updated: 0,
            deleted: 0,
            unchanged: 4
        })
    );
    assert_eq!(runs[2].tag, None);

    // A run keeps the configuration file as it was, and how its result was made.
    for run in &runs {
        let config = run.config.as_ref().expect("the configuration file");
        assert_eq!(project.store.bytes(config).unwrap(), CONFIG.as_bytes());
    }
    // The file list comes from a scan, not from a process.
    assert_eq!(runs[2].info, None);
    let info = runs[3]
        .info
        .as_ref()
        .expect("how the documents were extracted");
    let files = project.store.snapshot(second.files.snapshot).unwrap();
    assert_eq!(
        project.store.info(info).unwrap(),
        serde_json::json!({
            "type": "extracted",
            "inputs": [files.digest().as_str()],
            "modules": [{ "name": "loader.text", "compatibility": 1, "args": {} }],
        })
    );
    // The same process on another file list is another description; the last run found the
    // second one's result by its key, so it is described the same way.
    assert_ne!(runs[1].info, runs[3].info);
    assert_eq!(runs[5].info, runs[3].info);
}

#[test]
fn follows_updates_and_deletions_but_not_dates_or_places() {
    let project = Project::new();
    let first = project.extract();
    let version_1 = first.extracted.snapshot;
    let before = project.entries(version_1);

    // One document is rewritten, one is deleted and one only gets a new time.
    let guide = project.docs.join("guide.md");
    let rewritten = fs::read_to_string(&guide).unwrap() + "\n## 追記\n\n更新した内容。\n";
    fs::write(&guide, &rewritten).unwrap();
    set_age(&guide, Duration::from_secs(600));
    fs::remove_file(project.docs.join("faq.txt")).unwrap();
    set_age(
        &project.docs.join("manuals/pump.md"),
        Duration::from_secs(600),
    );
    let second = project.extract();
    assert!(second.added.is_empty());
    assert_eq!(second.updated, ["guide.md"]);
    assert_eq!(second.deleted, ["faq.txt"]);
    // Two files are hashed because their times changed; only the rewritten one is read.
    assert_eq!((second.unchanged, second.hashed, second.read), (2, 2, 1));
    let version_2 = second.extracted.snapshot;
    let after = project.entries(version_2);
    assert_eq!(
        project.documents(version_2),
        ["guide.md", "manuals/pump.md", "manuals/フィルター.md"]
    );
    assert_eq!(project.text(version_2, "guide.md").0, rewritten);
    for document in ["manuals/pump.md", "manuals/フィルター.md"] {
        let find = |entries: &[(String, String)]| {
            entries.iter().find(|(name, _)| name == document).cloned()
        };
        assert_eq!(find(&after), find(&before), "{document} changed");
    }
    // The earlier version still has the deleted document and the earlier text.
    assert_eq!(project.entries(version_1), before);
    assert!(
        project
            .text(version_1, "faq.txt")
            .0
            .contains("よくある質問")
    );
    assert!(!project.text(version_1, "guide.md").0.contains("追記"));

    // The folder is copied to another place with other times: the same version.
    let copy = project.docs.with_file_name("copy");
    copy_into(&project.docs, &copy, Duration::from_secs(60));
    let third = project.extract_from(&copy);
    assert_eq!(third.hashed, 3);
    assert_eq!(third.read, 0);
    assert_eq!(third.files.outcome, Outcome::Unchanged);
    assert_eq!(third.files.snapshot, second.files.snapshot);
    assert_eq!(third.extracted.outcome, Outcome::Reused);
    assert_eq!(third.extracted.snapshot, version_2);
    assert_eq!(
        project.store.history("extracted", "docs").unwrap(),
        [version_1, version_2]
    );

    // The folder goes back to what it was at the start. The file list is a new one that holds
    // what the first one held, so the key of the first run is found: the tag returns to the
    // first version and nothing is read.
    fs::write(&guide, fs::read(fixtures("original/guide.md")).unwrap()).unwrap();
    set_age(&guide, Duration::from_secs(300));
    fs::copy(fixtures("original/faq.txt"), project.docs.join("faq.txt")).unwrap();
    set_age(&project.docs.join("faq.txt"), Duration::from_secs(300));
    let fourth = project.extract();
    assert_eq!(fourth.added, ["faq.txt"]);
    assert_eq!(fourth.updated, ["guide.md"]);
    assert_eq!(fourth.read, 0);
    assert_eq!(fourth.files.outcome, Outcome::Created);
    assert_ne!(fourth.files.snapshot, first.files.snapshot);
    let files = |id| project.store.snapshot(id).unwrap().digest();
    assert_eq!(files(fourth.files.snapshot), files(first.files.snapshot));
    assert_eq!(fourth.extracted.outcome, Outcome::Reused);
    assert_eq!(fourth.extracted.snapshot, version_1);
    assert_eq!(project.entries(version_1), before);
    assert_eq!(
        project.store.resolve("extracted", "docs").unwrap(),
        Some(version_1)
    );
    assert_eq!(
        project.store.history("extracted", "docs").unwrap(),
        [version_1, version_2, version_1]
    );
}

#[test]
fn keeps_an_earlier_text_for_a_file_it_cannot_read() {
    let project = Project::new();
    let first = project.extract();
    let version_1 = first.extracted.snapshot;
    let earlier = project.text(version_1, "manuals/pump.md").0;
    let degraded = || project.store.runs().unwrap().last().unwrap().degraded;

    // The document becomes unreadable, and a file of another format appears.
    let pump = project.docs.join("manuals/pump.md");
    fs::write(&pump, [0xff, 0xfe, 0x00]).unwrap();
    set_age(&pump, Duration::from_secs(600));
    let image = project.docs.join("logo.png");
    fs::write(&image, [0x89, b'P', b'N', b'G']).unwrap();
    set_age(&image, Duration::from_secs(600));
    let second = project.extract();
    assert_eq!(second.added, ["logo.png"]);
    assert_eq!(second.updated, ["manuals/pump.md"]);
    assert_eq!(second.substituted, ["manuals/pump.md"]);
    assert_eq!(second.skipped, ["logo.png"]);
    assert_eq!(second.read, 0);
    // The file list changed. The extracted documents are what they were: the earlier text
    // stands in, and the image is not among them.
    assert_eq!(second.files.outcome, Outcome::Created);
    assert_eq!(second.extracted.outcome, Outcome::Unchanged);
    assert_eq!(second.extracted.snapshot, version_1);
    assert_eq!(project.text(version_1, "manuals/pump.md").0, earlier);
    assert_eq!(project.documents(version_1), ORIGINAL);
    assert!(degraded());

    // A degraded run is not found by its key: the next run tries the file again.
    let third = project.extract();
    assert_eq!(third.files.outcome, Outcome::Unchanged);
    assert_eq!(third.extracted.outcome, Outcome::Unchanged);
    assert_eq!(third.substituted, ["manuals/pump.md"]);
    assert!(degraded());

    // Once the file is readable again, it is read without anything else being asked.
    fs::write(&pump, &earlier).unwrap();
    set_age(&pump, Duration::from_secs(300));
    let fourth = project.extract();
    assert!(fourth.substituted.is_empty());
    assert!(!degraded());
    // The content is the one of the first version, so the object remembers its item.
    assert_eq!(fourth.read, 0);
    assert_eq!(fourth.extracted.snapshot, version_1);
    assert_eq!(project.history_len(), 1);

    // With nothing failing, the next run is found by its key again.
    let fifth = project.extract();
    assert_eq!(fifth.extracted.outcome, Outcome::Reused);
}

#[test]
fn takes_in_only_the_changes_it_is_told_to() {
    let project = Project::new();
    let first = project.extract();
    let version_1 = first.extracted.snapshot;
    let original = project.text(version_1, "guide.md").0;

    // One document is rewritten, one is deleted and two are added.
    let guide = project.docs.join("guide.md");
    let rewritten = original.clone() + "\n## 追記\n\n更新した内容。\n";
    fs::write(&guide, &rewritten).unwrap();
    set_age(&guide, Duration::from_secs(600));
    fs::remove_file(project.docs.join("faq.txt")).unwrap();
    copy_into(
        &fixtures("additional"),
        &project.docs,
        Duration::from_secs(1800),
    );

    // Only additions are taken in: the rewritten document keeps its text and the deleted one
    // stays.
    let only_new = Apply {
        add: true,
        update: false,
        delete: false,
    };
    let held_back = || Withheld {
        added: Vec::new(),
        updated: vec!["guide.md".into()],
        deleted: vec!["faq.txt".into()],
    };
    let second = project.extract_with(only_new);
    assert_eq!(second.added, ADDED);
    assert!(second.updated.is_empty() && second.deleted.is_empty());
    assert_eq!(second.withheld, held_back());
    // The rewritten file is hashed to see that it changed; only the new ones are read.
    assert_eq!((second.unchanged, second.hashed, second.read), (4, 3, 2));
    let version_2 = second.extracted.snapshot;
    assert_eq!(
        project.documents(version_2),
        [
            "faq.txt",
            "guide.md",
            "manuals/pump.md",
            "manuals/valve.md",
            "manuals/フィルター.md",
            "release-notes.md"
        ]
    );
    assert_eq!(project.text(version_2, "guide.md").0, original);
    assert!(
        project
            .text(version_2, "faq.txt")
            .0
            .contains("よくある質問")
    );
    // The file list is the folder as it is taken, so the documents follow from it alone.
    assert_eq!(
        project.documents(second.files.snapshot),
        project.documents(version_2)
    );
    let runs = project.store.runs().unwrap();
    assert_eq!(
        runs[runs.len() - 2].stats["withheld"],
        serde_json::json!({ "added": [], "updated": ["guide.md"], "deleted": ["faq.txt"] })
    );

    // The same again: nothing is made, and what is held back is said again.
    let third = project.extract_with(only_new);
    assert_eq!(third.files.outcome, Outcome::Unchanged);
    assert_eq!(third.extracted.outcome, Outcome::Reused);
    assert_eq!(third.withheld, held_back());
    assert_eq!((third.unchanged, third.hashed, third.read), (6, 1, 0));

    // Every change is taken in: what was held back comes in now.
    let fourth = project.extract();
    assert!(fourth.added.is_empty());
    assert_eq!(fourth.updated, ["guide.md"]);
    assert_eq!(fourth.deleted, ["faq.txt"]);
    assert_eq!(fourth.withheld, Withheld::default());
    assert_eq!((fourth.unchanged, fourth.hashed, fourth.read), (4, 1, 1));
    let version_3 = fourth.extracted.snapshot;
    assert_eq!(project.documents(version_3).len(), 5);
    assert_eq!(project.text(version_3, "guide.md").0, rewritten);

    // Only deletions are taken in: a new file is not opened.
    let pump = project.docs.join("manuals/pump.md");
    fs::write(&pump, "# ポンプ\n\n書き換えた。\n").unwrap();
    set_age(&pump, Duration::from_secs(300));
    let new = project.docs.join("new.md");
    fs::write(&new, "# 新しい文書\n").unwrap();
    set_age(&new, Duration::from_secs(300));
    fs::remove_file(project.docs.join("release-notes.md")).unwrap();
    let fifth = project.extract_with(Apply {
        add: false,
        update: false,
        delete: true,
    });
    assert!(fifth.added.is_empty() && fifth.updated.is_empty());
    assert_eq!(fifth.deleted, ["release-notes.md"]);
    assert_eq!(
        fifth.withheld,
        Withheld {
            added: vec!["new.md".into()],
            updated: vec!["manuals/pump.md".into()],
            deleted: Vec::new(),
        }
    );
    assert_eq!((fifth.unchanged, fifth.hashed, fifth.read), (4, 1, 0));
    let version_4 = fifth.extracted.snapshot;
    assert_eq!(
        project.documents(version_4),
        [
            "guide.md",
            "manuals/pump.md",
            "manuals/valve.md",
            "manuals/フィルター.md"
        ]
    );
    assert!(
        !project
            .text(version_4, "manuals/pump.md")
            .0
            .contains("書き換えた")
    );
}

impl Project {
    fn history_len(&self) -> usize {
        self.store.history("extracted", "docs").unwrap().len()
    }
}
